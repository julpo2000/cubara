//! The game: world state, the simulation, and what input does to them.
//!
//! Deliberately separate from the renderer (`ARCHITECTURE.md` Rule 3). The renderer
//! draws what it is given; it does not decide where the player is looking or which
//! block a click breaks. When those lived on `Renderer` it could place blocks, which
//! is the boundary error the rule names — and the pattern that makes the reference
//! anti-pattern codebase impossible to change one system at a time.
//!
//! Nothing here touches the GPU, so all of it is testable without an adapter. It does
//! own the fixed-timestep accumulator (block 1.6, issue #57): [`Game::advance`] takes
//! a wall-clock `dt` and turns it into zero or more fixed [`cubara_sim::TICK_DT`]
//! steps, but never calls `Instant::now()` itself -- that stays in `main.rs`, per
//! `docs/PHASE1_ARCHITECTURE.md` §9, so this is testable with arbitrary, scripted
//! frame times instead of real wall-clock timing.

use std::sync::Arc;

use cubara_render::CameraPose;
use cubara_render::{swatch_color, HotbarSlot, InventoryPanel, PanelSlotKind};
use cubara_sim::{InputFrame, Player, Sim, REACH, TICK_DT};
use cubara_sim::{SlotRef, HOTBAR_WIDTH};
use cubara_voxel::ChunkCoord;
use cubara_voxel::{
    BlockId, BlockRegistry, DropRule, Interact, ItemRegistry, ItemStack, ItemState, RecipeBook,
    SmeltBook,
};
use cubara_world::TerrainBlocks;
use cubara_world::World;
use cubara_world::{ChunkState, Furnace, SmeltCtx, TimedProcess};

use winit::keyboard::KeyCode;

/// Caps how many fixed steps a single [`Game::advance`] call runs -- a stalled or
/// backgrounded window can hand back a huge `dt` (seconds, not milliseconds), and
/// without a cap the sim would try to fully "catch up" by ticking hundreds of times
/// in one frame, which take longer than real time to run, which produces a bigger
/// backlog next frame: a spiral of death. Past the cap the leftover backlog is
/// dropped, not accumulated -- the sim falls behind wall-clock time rather than
/// locking up trying to chase it.
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Everything the player *is* and *does*: the world they're in and the simulation
/// running against it.
/// Load `assets/items/*.ron`.
///
/// In the app rather than in `cubara-render` alongside `load_registry`: items
/// are not a render concern (`ARCHITECTURE.md` Rule 3), and nothing about them
/// needs a GPU. `CARGO_MANIFEST_DIR` is `crates/app`, so `../..` reaches the
/// repo root regardless of the caller's working directory -- the same trick
/// `load_registry` uses from `crates/render`.
pub fn load_item_registry() -> ItemRegistry {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ItemRegistry::load(&repo_root.join("assets/items")).expect("assets/items must load")
}

/// Load `assets/structures/*.ron` -- the shapes worldgen grows.
pub fn load_structure_registry() -> cubara_voxel::StructureRegistry {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    cubara_voxel::StructureRegistry::load(&repo_root.join("assets/structures"))
        .expect("assets/structures must load")
}

/// Where the world lives on disk.
///
/// One world, in a fixed place next to the executable's project root. Named
/// worlds and a world-picker are a gameplay/UI decision nobody has made
/// (#179), and inventing one would be inventing a menu; this is the smallest
/// thing that makes "still there after you close it" true, which is what
/// `ROADMAP.md` says phase 1 delivers.
pub fn world_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("saves/world")
}

/// Load `assets/smelting/*.ron`, resolving item names through `items`.
pub fn load_smelt_book(items: &ItemRegistry) -> SmeltBook {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    SmeltBook::load(&repo_root.join("assets/smelting"), items).expect("assets/smelting must load")
}

/// Load `assets/ores/*.ron` -- which ores exist, and how common they are.
pub fn load_ore_registry() -> cubara_voxel::OreRegistry {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    cubara_voxel::OreRegistry::load(&repo_root.join("assets/ores")).expect("assets/ores must load")
}

/// Load `assets/recipes/*.ron`, resolving ingredient names through `items`.
pub fn load_recipe_book(items: &ItemRegistry) -> RecipeBook {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    RecipeBook::load(&repo_root.join("assets/recipes"), items).expect("assets/recipes must load")
}

/// A break part-way through (`PHASE2_ARCHITECTURE.md` §4.3).
///
/// Keyed by the block position *and* the tool being used: change either and the
/// progress is dropped rather than carried over, which is what "abandoned, not
/// banked" means in practice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mining {
    block: [i32; 3],
    /// The item id held when this break started, so switching tools restarts.
    /// `None` for a bare hand.
    tool: Option<cubara_voxel::ItemId>,
    /// Work done so far, in the same units as the block's `hardness`. One tick
    /// adds the tool's `speed`, so this reaching `hardness` is exactly
    /// `ceil(hardness / speed)` ticks -- the §4.3 formula, without a division.
    progress: u32,
}

/// How far from the player, in chunks, the simulation keeps running
/// (`PHASE2_ARCHITECTURE.md` §11.4).
///
/// **Deliberately unrelated to render distance.** Coupling them would let the
/// settings menu quietly change what the world simulates. Small, because
/// simulation is the expensive part and dormancy is what makes a big world
/// affordable; expected to grow once block 2.7 makes a dormant chunk nearly
/// free.
const SIM_RADIUS_CHUNKS: i32 = 4;

/// Advance one furnace by `ticks`, whether that is one ordinary tick or a
/// dormant chunk's whole backlog.
fn advance_furnace(
    world: &mut World,
    pos: [i32; 3],
    ticks: u64,
    items: &ItemRegistry,
    smelting: &SmeltBook,
) {
    let Some(f) = world.furnace_at_mut(pos) else {
        return;
    };
    // Resolved to plain numbers once, here: a furnace only ever asks about the
    // one item in its fuel slot and the one its recipe outputs, so nothing in
    // the catch-up needs a registry (§12.3).
    let recipe = f.input.and_then(|(id, _)| smelting.for_input(id));
    let ctx = SmeltCtx {
        recipe,
        fuel_burn: f.fuel.and_then(|(id, _)| items.burn_ticks(id)),
        output_max: recipe.map(|r| items.max_stack(r.output)).unwrap_or(64),
    };
    // Bounded catch-up (§12.1): one ordinary tick and a million-tick backlog go
    // through the same call, and cost the same.
    f.advance(ticks, &ctx);
}

/// The middle of block `b`, where an item dropped by breaking it appears.
fn drop_centre(b: [i32; 3]) -> glam::Vec3 {
    glam::Vec3::new(b[0] as f32 + 0.5, b[1] as f32 + 0.5, b[2] as f32 + 0.5)
}

pub struct Game {
    /// The world being played. Behind an [`Arc`] so meshing jobs can carry the exact
    /// snapshot they were queued against; an edit publishes a new one.
    world: Arc<World>,
    sim: Sim,
    /// The player's pose as of the *previous* completed tick -- together with
    /// `sim.player` (the current tick), what [`Game::camera_pose`] interpolates
    /// between for smooth rendering of a 60 Hz sim at any frame rate (§9).
    prev_player: Player,
    /// The block registry, shared with `NodeStreaming` rather than loaded
    /// twice -- ids are per-registry (`PHASE2_ARCHITECTURE.md` §1.2), so two
    /// loads would be two id spaces and the same number would mean different
    /// materials on each side. `None` until `resumed` builds it.
    blocks_registry: Option<Arc<BlockRegistry>>,
    /// Which ids the terrain's grass/soil/stone are, in that registry.
    terrain: Option<TerrainBlocks>,
    /// What items exist. Loaded by the app, not by `cubara-render`: items are
    /// not a render concern (Rule 3).
    items: Option<ItemRegistry>,
    /// Every recipe, loaded alongside the items they name.
    recipes: Option<RecipeBook>,
    /// Whether the inventory screen is open. Screen state, not world state --
    /// what the *grid* holds is world state and lives on the player.
    inventory_open: bool,
    /// The chunk the simulation radius was last updated around. `None` until
    /// the first tick, so it always runs once.
    sim_centre: Option<ChunkCoord>,
    /// The furnace whose screen is open, by world position. `None` when the
    /// open screen is the plain inventory or a bench.
    open_furnace: Option<[i32; 3]>,
    /// Every smelting recipe, loaded alongside the items they name.
    smelting: Option<SmeltBook>,
    /// Whether the break button is currently held. Read once per `advance`
    /// into [`InputFrame::breaking`].
    breaking: bool,
    /// The break in progress, if any (`PHASE2_ARCHITECTURE.md` §4.3).
    ///
    /// **Not on the chunk, and not in the save format.** It is transient, it
    /// belongs to one player, and §4.3 decided progress is abandoned rather
    /// than banked -- so there is nothing here worth persisting, and putting it
    /// on the chunk would make it block-entity-shaped (§7) for something the
    /// player cannot even see.
    mining: Option<Mining>,
    /// Wall-clock seconds not yet consumed by a fixed tick. `f64`, not `f32`
    /// like everything else here -- this is the one value that keeps being
    /// added to across a whole play session (thousands of frames), and
    /// `f32`'s ~7 significant digits let rounding error accumulate enough to
    /// tip a close call across a tick boundary a step early or late (this
    /// is exactly what `frame_rate_independent_movement_reaches_the_same_state`
    /// caught: two runs summing to the same total elapsed time, chopped into
    /// frames differently, landed one tick apart with an `f32` accumulator).
    accumulator: f64,
    // Held movement keys, translated into `InputFrame::move_axes` once per
    // `advance` call.
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    /// Whether the free-fly toggle key is currently held -- tracked so
    /// `key_input` can tell a fresh press from OS key-repeat and only raise
    /// `fly_toggle_pending` on the rising edge.
    fly_toggle_held: bool,
    /// Jump / fly-toggle: `true` from the tick they were pressed until the
    /// next `advance` call hands them to `Sim::tick` as an `InputFrame`
    /// button edge, then cleared -- see `advance`'s doc comment for why
    /// they're consumed by only the first tick of a catch-up burst.
    jump_pending: bool,
    fly_toggle_pending: bool,
    /// Mouse motion (pixels) accumulated since the last `advance` call.
    look_delta: (f32, f32),
}

impl Game {
    /// Start above the terrain near the origin, looking out over it and slightly
    /// down (yaw ~35°, gentle downward pitch). Walking mode by default (not
    /// free-fly, per issue #53's Context: the point of this block is a world
    /// you land in and walk, not one you start already flying over) -- gravity
    /// carries the player down onto the terrain below.
    pub fn new() -> Self {
        let player = Player::new(glam::vec3(0.0, 48.0, 0.0), 0.6, -0.3);
        Self {
            world: Arc::new(World::new()),
            sim: Sim::new(0, player),
            prev_player: player,
            blocks_registry: None,
            terrain: None,
            items: None,
            recipes: None,
            inventory_open: false,
            sim_centre: None,
            open_furnace: None,
            smelting: None,
            breaking: false,
            mining: None,
            accumulator: 0.0,
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
            fly_toggle_held: false,
            jump_pending: false,
            fly_toggle_pending: false,
            look_delta: (0.0, 0.0),
        }
    }

    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    /// The camera pose to render from: the sim's player pose, interpolated
    /// between its previous and current tick by the accumulator's leftover
    /// fraction. Render-side only (§9) -- never read back into the sim.
    pub fn camera_pose(&self) -> CameraPose {
        let alpha = (self.accumulator / TICK_DT as f64).clamp(0.0, 1.0) as f32;
        let player = self.prev_player.lerp(&self.sim.player, alpha);
        CameraPose {
            eye: player.pos,
            look_dir: player.look_dir(),
        }
    }

    /// The block the player is currently looking at, within [`REACH`], for
    /// the renderer to outline -- computed by the sim's own raycast each
    /// tick (`cubara_sim::Sim::tick`), not here. The renderer draws it; it
    /// does not decide it (`ARCHITECTURE.md` Rule 3, issue #52).
    pub fn selected_block(&self) -> Option<[i32; 3]> {
        self.sim.target
    }

    /// Record a movement key going down/up. Unmapped keys are ignored (returns
    /// whether the key was one the game cares about).
    ///
    /// Space and F4 double as both a *held* signal (free-fly's vertical axis,
    /// respectively nothing) and a *rising-edge* signal (jump; the free-fly
    /// toggle) -- `cubara-sim` decides which one applies, based on which mode
    /// the player is currently in, so this method just reports both truthfully
    /// rather than guessing the mode here (Rule 3: no gameplay decisions on
    /// this side of the seam beyond packaging raw input).
    pub fn key_input(&mut self, key: KeyCode, pressed: bool) -> bool {
        match key {
            KeyCode::KeyW | KeyCode::ArrowUp => self.forward = pressed,
            KeyCode::KeyS | KeyCode::ArrowDown => self.back = pressed,
            KeyCode::KeyA | KeyCode::ArrowLeft => self.left = pressed,
            KeyCode::KeyD | KeyCode::ArrowRight => self.right = pressed,
            KeyCode::Space => {
                if pressed && !self.up {
                    self.jump_pending = true;
                }
                self.up = pressed;
            }
            KeyCode::ShiftLeft | KeyCode::ShiftRight | KeyCode::ControlLeft => self.down = pressed,
            KeyCode::F4 => {
                if pressed && !self.fly_toggle_held {
                    self.fly_toggle_pending = true;
                }
                self.fly_toggle_held = pressed;
            }
            // Hotbar selection. Applied on press only -- releasing a number
            // key must not reselect, and holding it must not repeat.
            KeyCode::Digit1
            | KeyCode::Digit2
            | KeyCode::Digit3
            | KeyCode::Digit4
            | KeyCode::Digit5
            | KeyCode::Digit6
            | KeyCode::Digit7
            | KeyCode::Digit8
            | KeyCode::Digit9 => {
                if pressed {
                    let slot = match key {
                        KeyCode::Digit1 => 0,
                        KeyCode::Digit2 => 1,
                        KeyCode::Digit3 => 2,
                        KeyCode::Digit4 => 3,
                        KeyCode::Digit5 => 4,
                        KeyCode::Digit6 => 5,
                        KeyCode::Digit7 => 6,
                        KeyCode::Digit8 => 7,
                        _ => 8,
                    };
                    self.select_hotbar(slot);
                }
            }
            _ => return false,
        }
        true
    }

    /// Feed a raw mouse-motion delta (pixels) toward the next tick's look input.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        self.look_delta.0 += dx;
        self.look_delta.1 += dy;
    }

    /// Advance the simulation by `dt` wall-clock seconds: zero or more fixed
    /// [`TICK_DT`] steps, capped at [`MAX_TICKS_PER_FRAME`]. Input is sampled
    /// once (currently-held keys + mouse motion since the last call) and that
    /// same [`InputFrame`] drives every step this call runs -- ticks never read
    /// live input themselves, so a scripted/replayed input sequence (block 1.8)
    /// reproduces exactly.
    ///
    /// The one-shot accumulated inputs -- `look_delta` (however far the mouse
    /// moved since the last tick) and the `jump`/`toggle_fly` key edges -- are
    /// held until a tick actually consumes them, and cleared after the *first*
    /// tick of any catch-up burst. Two failure modes both come from getting
    /// that wrong, and only `move_axes` (a genuinely *held* continuous state,
    /// correctly re-applied once per backlog tick) is exempt:
    ///
    /// - **Draining them on a sub-tick frame drops them.** The renderer runs
    ///   uncapped, so on a fast machine most frames are shorter than one 60 Hz
    ///   tick and run zero ticks. Sampling+clearing the accumulators every
    ///   frame (as this did) threw away ~59 of every 60 frames' mouse motion --
    ///   you could barely look around at high FPS. So: bail out before touching
    ///   them when there isn't a tick's worth of time yet.
    /// - **Re-applying them on every tick of a burst multiplies them.** A
    ///   frame-pacing hiccup forces 2+ ticks into one call; reusing the
    ///   unmodified total each time (correct for held `move_axes`) would turn
    ///   one frame's mouse motion, or one key press, into N. So: clear them
    ///   after the first tick.
    pub fn advance(&mut self, dt: f32) -> Vec<ChunkCoord> {
        self.accumulator += dt as f64;

        // Not a tick's worth of time yet: leave the accumulated one-shot inputs
        // alone so a later frame's tick can consume them, rather than sampling
        // and clearing them here where no tick would apply them. `move_axes` is
        // re-read from live held state on the frame that does tick, so nothing
        // is lost by returning early.
        if self.accumulator < TICK_DT as f64 {
            return Vec::new();
        }

        let mut input = InputFrame {
            move_axes: [
                (self.right as i32 - self.left as i32) as f32,
                (self.up as i32 - self.down as i32) as f32,
                (self.forward as i32 - self.back as i32) as f32,
            ],
            look_delta: [self.look_delta.0, self.look_delta.1],
            jump: self.jump_pending,
            toggle_fly: self.fly_toggle_pending,
            breaking: self.breaking,
        };
        // A tick below will consume these, so it's safe to clear them now.
        self.look_delta = (0.0, 0.0);
        self.jump_pending = false;
        self.fly_toggle_pending = false;

        let mut ticks = 0;
        let mut dirty = Vec::new();
        while self.accumulator >= TICK_DT as f64 {
            self.prev_player = self.sim.player;
            // Read before the mutable borrow of `world`.
            let terrain = self.terrain();
            self.sim
                .tick(Arc::make_mut(&mut self.world), &input, terrain);
            // Mining advances *per tick*, not per frame -- §4.3, and the same
            // reason the tick loop exists. A catch-up burst of N ticks is N
            // ticks of progress, which is correct: that time really did pass.
            if let Some(cc) = self.tick_mining(input.breaking) {
                dirty.push(cc);
            }
            self.tick_furnaces();
            // Dropped items fall, age out, and get picked up -- on the same
            // fixed clock as everything else (§10.4, Rule 1).
            if let Some(items) = self.items.as_ref() {
                self.sim.tick_entities(&self.world, terrain, items);
            }
            input.jump = false;
            input.toggle_fly = false;
            input.look_delta = [0.0, 0.0];
            self.accumulator -= TICK_DT as f64;
            ticks += 1;
            if ticks >= MAX_TICKS_PER_FRAME {
                // Spiral-of-death guard: fall behind wall-clock time rather than
                // trying to fully catch up, which would only make the next
                // frame's backlog worse.
                self.accumulator = 0.0;
                break;
            }
        }
        dirty
    }

    /// Stand the player on the terrain under their column, and make that their
    /// spawn point.
    ///
    /// **Without this the game is unplayable.** `Game::new` places the player at
    /// y = 48 because terrain does not exist yet at that point -- but the
    /// surface under that column is at y = 15, a 32-block drop. Once block 2.9a
    /// made falling hurt, that is 29 damage against 20 health: the player dies
    /// on the first landing, respawns at the same point in mid-air, and dies
    /// again, forever.
    ///
    /// Every test missed it because they all reposition the player just above
    /// the ground before doing anything. `the_game_does_not_start_by_killing_the_player`
    /// is the one that starts the way the app does.
    ///
    /// Two blocks above the surface, not exactly on it: the eye is 1.62 above
    /// the feet, so this leaves a fraction of a block to settle -- well inside
    /// the 3-block safe fall, and it avoids having to reach for the private
    /// eye-height constant from another crate.
    fn place_player_on_ground(&mut self) {
        let Some(terrain) = self.terrain else {
            return;
        };
        let p = self.sim.player.pos;
        let Some(hit) = self
            .world
            .raycast([p.x, 200.0, p.z], [0.0, -1.0, 0.0], 400.0, terrain)
        else {
            return;
        };
        let standing = glam::vec3(p.x, hit.block[1] as f32 + 2.0, p.z);
        self.sim.player.pos = standing;
        self.sim.player.velocity = glam::Vec3::ZERO;
        self.sim.player.fall_distance = 0.0;
        // Death returns here, not to wherever `Game::new` happened to start.
        self.sim.player.spawn = standing;
        self.prev_player = self.sim.player;
    }

    /// Write the world to disk (#179).
    ///
    /// Best-effort and non-fatal: a failed save is logged, not a crash. Losing
    /// a session is bad; losing it *and* taking the window down with it is
    /// worse, and the player may be quitting precisely because something is
    /// already wrong.
    pub fn save(&self) {
        self.save_to(&world_dir());
    }

    /// [`save`](Self::save) into a specific directory -- what the tests drive,
    /// so they never touch the real world folder.
    pub fn save_to(&self, dir: &std::path::Path) {
        let (Some(registry), Some(items), Some(blocks)) = (
            self.blocks_registry.as_deref(),
            self.items.as_ref(),
            self.terrain,
        ) else {
            return;
        };
        match cubara_sim::save_world(dir, &self.sim, &self.world, registry, items, blocks) {
            Ok(()) => log::info!("world saved to {}", dir.display()),
            Err(e) => log::error!("could not save the world: {e}"),
        }
    }

    /// Replace this game's world with the one on disk, if there is one (#179).
    ///
    /// Returns whether anything was loaded. A missing save is the normal first
    /// run, not an error. A save that exists but *fails* to load is logged and
    /// ignored rather than fatal -- most often it is a version mismatch after
    /// the generator changed, and refusing to start the game over it would be
    /// worse than starting a fresh world.
    ///
    /// **Called after `set_assets`**, which stands the player on the ground:
    /// this overwrites that position with the saved one, so a player who
    /// quit in a mineshaft comes back to the mineshaft.
    pub fn load(&mut self) -> bool {
        let dir = world_dir();
        self.load_from(&dir)
    }

    /// [`load`](Self::load) from a specific directory.
    pub fn load_from(&mut self, dir: &std::path::Path) -> bool {
        let (Some(registry), Some(items), Some(blocks)) = (
            self.blocks_registry.as_deref(),
            self.items.as_ref(),
            self.terrain,
        ) else {
            return false;
        };
        if !dir.join("level.ron").exists() {
            return false;
        }
        match cubara_sim::load_world(dir, registry, items, blocks) {
            Ok((sim, world)) => {
                self.sim = sim;
                self.world = Arc::new(world);
                self.prev_player = self.sim.player;
                // The simulation radius is recomputed from scratch: the saved
                // world has no chunk lifecycle (§11), by design.
                self.sim_centre = None;
                log::info!("world loaded from {}", dir.display());
                true
            }
            Err(e) => {
                log::error!(
                    "could not load {}: {e} -- starting a fresh world",
                    dir.display()
                );
                false
            }
        }
    }

    /// The player's health, reduced to what the renderer draws
    /// (`PHASE2_ARCHITECTURE.md` §13.1).
    ///
    /// Both numbers, so `cubara-render` never learns what full health is --
    /// it is told the points and the maximum and works out the hearts (Rule 3).
    pub fn health_view(&self) -> cubara_render::HealthView {
        cubara_render::HealthView {
            points: self.sim.player.health,
            max_points: cubara_sim::MAX_HEALTH,
        }
    }

    /// Whether the break button is held. Held state rather than an edge, since
    /// mining advances for as long as it is down (§4.3).
    ///
    /// Releasing abandons any break in progress on the next tick -- that is
    /// [`tick_mining`](Self::tick_mining)'s doing, not this setter's, so the
    /// abandon rule lives in one place.
    pub fn set_breaking(&mut self, held: bool) {
        self.breaking = held;
    }

    /// Break (`place = false`) or place (`true`) the block the player is looking
    /// at, within [`REACH`]. Returns the [`ChunkCoord`] whose geometry is now
    /// stale so the caller can re-mesh it, or `None` if nothing was in reach.
    ///
    /// Placing puts the block against the hit face. Uses the sim's current
    /// (non-interpolated) pose -- editing is a gameplay decision, and
    /// interpolation is a render-only concern (§9) that must never feed back
    /// into it. Raycasts fresh rather than reusing [`Sim::target`](cubara_sim::Sim)
    /// -- that field only updates on the next tick, so it can go stale
    /// against `self.world` after an edit lands (e.g. two edits between
    /// ticks); issue #52 scoped out any change to raycasting itself anyway.
    /// Give the game the assets it needs to turn blocks into items and back.
    /// Called once, when the window and its registry exist.
    pub fn set_assets(
        &mut self,
        registry: Arc<BlockRegistry>,
        items: ItemRegistry,
        recipes: RecipeBook,
    ) {
        self.terrain = Some(
            TerrainBlocks::from_registry(&registry)
                .with_oak(&load_structure_registry(), &registry)
                .with_ores(&load_ore_registry(), &registry),
        );
        self.blocks_registry = Some(registry);
        self.smelting = Some(load_smelt_book(&items));
        // Terrain is known for the first time here, so this is where the player
        // can be put somewhere that exists.
        self.place_player_on_ground();
        self.items = Some(items);
        self.recipes = Some(recipes);
    }

    /// Break the targeted block and put its drop in the inventory.
    ///
    /// **The break always happens; the drop is conditional.** Three things
    /// decide what you get (`PHASE2_ARCHITECTURE.md` §4, block 2.4a):
    ///
    /// 1. The block's [`DropRule`] -- its own name, a specific item, or
    ///    nothing.
    /// 2. Its `requires_tier` against the held tool's tier. **Too low a tier
    ///    still breaks the block, but yields nothing.** §4 chose that over
    ///    refusing to break: a block that will not break with no explanation
    ///    reads as a bug, where one that breaks and drops nothing teaches the
    ///    rule in one go.
    /// 3. Whether an item of that name exists at all.
    ///
    /// **Durability is spent only on a break that yielded something.** A
    /// failed-tier break costs nothing -- §4: you are not punished twice for
    /// the same mistake. Breaking bare-handed costs nothing either, since only
    /// a tool carries durability.
    ///
    /// **A drop that does not fit is lost.** There are no dropped-item entities
    /// yet (they need ECS, 2.5), so the remainder `Inventory::add` hands back is
    /// logged and discarded. Refusing to break the block instead would be a
    /// gameplay decision, and those are the owner's.
    ///
    /// **No longer on the game's own path**, since 2.4b: playing mines over
    /// several ticks via [`tick_mining`](Self::tick_mining), and both go
    /// through the same [`break_at`](Self::break_at). Kept as the instant-break
    /// entry point, which is what the drop and tier tests drive directly rather
    /// than holding a button for eight ticks to assert one drop.
    #[allow(dead_code)]
    pub fn break_block(&mut self) -> Option<ChunkCoord> {
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        Some(self.break_at(hit.block))
    }

    /// One tick of mining (`PHASE2_ARCHITECTURE.md` §4.3). Returns the chunk to
    /// re-mesh on the tick the block finally gives way.
    ///
    /// **Progress is abandoned, not banked.** It is dropped when the button is
    /// released, when the player looks at a different block, or when the held
    /// tool changes -- each of those makes the stored `Mining` stop matching,
    /// and a non-match restarts from zero rather than resuming.
    fn tick_mining(&mut self, breaking: bool) -> Option<ChunkCoord> {
        if !breaking {
            self.mining = None;
            return None;
        }
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let Some(hit) = self.world.raycast(origin, dir, REACH, self.terrain()) else {
            // Looking at nothing in reach: whatever was in progress is gone.
            self.mining = None;
            return None;
        };
        let (registry, terrain) = (self.blocks_registry.as_deref()?, self.terrain?);
        let target = self
            .world
            .block_at(hit.block[0], hit.block[1], hit.block[2], terrain);

        // Absent hardness means unbreakable -- no progress accrues and no
        // amount of holding the button changes that.
        let hardness = registry.hardness(target)?;

        let held = self.sim.player.inventory.selected_stack().map(|s| s.item());
        let speed = match (held, self.items.as_ref()) {
            (Some(item), Some(items)) => items.speed(item),
            // An empty hand, or assets not yet wired: speed 1, §4.3's floor.
            _ => 1,
        };

        let fresh = Mining {
            block: hit.block,
            tool: held,
            progress: 0,
        };
        let m = match self.mining {
            Some(m) if m.block == fresh.block && m.tool == fresh.tool => m,
            _ => fresh,
        };
        let progress = m.progress + speed;
        if progress < hardness {
            self.mining = Some(Mining { progress, ..m });
            return None;
        }
        self.mining = None;
        Some(self.break_at(hit.block))
    }

    /// How far along the current break is, `0.0..1.0`, for the renderer to draw
    /// a crack overlay with. `None` when nothing is being mined.
    ///
    /// Exposed as a fraction rather than as the raw counters so that drawing it
    /// needs no access to the registries -- the renderer does not own gameplay
    /// (Rule 3).
    ///
    /// Nothing draws this yet -- the crack overlay is 2.4d, deliberately out of
    /// 2.4b's scope (#159). This is the fraction it will consume, and it is
    /// pinned by a test so the hook cannot rot before then.
    #[allow(dead_code)]
    pub fn mining_progress(&self) -> Option<f32> {
        let m = self.mining?;
        let registry = self.blocks_registry.as_deref()?;
        let terrain = self.terrain?;
        let target = self
            .world
            .block_at(m.block[0], m.block[1], m.block[2], terrain);
        let hardness = registry.hardness(target)?;
        if hardness == 0 {
            return Some(1.0);
        }
        Some((m.progress as f32 / hardness as f32).clamp(0.0, 1.0))
    }

    /// Break the block at `block`, applying §4's drop and durability rules.
    /// The shared tail of [`break_block`](Self::break_block) (instant, for
    /// tests and for anything that bypasses mining) and
    /// [`tick_mining`](Self::tick_mining) (timed, what the game actually does),
    /// so the two cannot drift apart on what a break *yields*.
    fn break_at(&mut self, block: [i32; 3]) -> ChunkCoord {
        let [x, y, z] = block;
        // Whatever state the block owned goes with it (§7) -- but its contents
        // now spill onto the floor rather than being destroyed (block 2.5,
        // §10.4). This is one of the five sites that used to lose items.
        if let Some(f) = Arc::make_mut(&mut self.world).remove_block_entity(block) {
            let contents: Vec<_> = [f.input, f.fuel, f.output].into_iter().flatten().collect();
            if let Some(items) = self.items.as_ref() {
                let spawned: Vec<_> = contents
                    .into_iter()
                    .filter_map(|(id, count)| items.new_stack(id, count).ok())
                    .collect();
                for stack in spawned {
                    self.sim
                        .entities
                        .spawn_item(stack, drop_centre(block), glam::Vec3::ZERO);
                }
            }
        }
        if self.open_furnace == Some(block) {
            self.open_furnace = None;
            self.inventory_open = false;
        }

        // The drop is the optional part; the break is not. Assets are always
        // set in the real app, but making the whole action depend on them
        // would mean a missing registry shows up as clicks that silently do
        // nothing -- the least debuggable failure there is.
        //
        // Read the three as separate fields rather than through a helper: the
        // borrow checker tracks disjoint field borrows, so `items` can stay
        // borrowed while `self.sim.player.inventory` is mutated. A helper
        // returning them all borrows the whole of `self`.
        if let (Some(registry), Some(terrain), Some(items)) = (
            self.blocks_registry.as_deref(),
            self.terrain,
            self.items.as_ref(),
        ) {
            let broken = self.world.block_at(x, y, z, terrain);
            let held = self.sim.player.inventory.selected_stack();
            let held_tier = held.map(|s| items.tier(s.item())).unwrap_or(0);

            let drop = if held_tier < registry.requires_tier(broken) {
                log::debug!(
                    "{} needs tier {}, holding tier {held_tier}: breaks, yields nothing",
                    registry.name_of(broken).unwrap_or("?"),
                    registry.requires_tier(broken),
                );
                None
            } else {
                match registry.drops(broken) {
                    DropRule::Nothing => None,
                    DropRule::SameName => registry
                        .name_of(broken)
                        .and_then(|name| items.id_of(name))
                        .map(|item| (item, 1u8)),
                    DropRule::Item(d) => items.id_of(&d.item).map(|item| (item, d.count)),
                }
            };

            match drop.and_then(|(item, count)| items.new_stack(item, count).ok()) {
                Some(stack) => {
                    // Block 2.5: what does not fit falls on the floor rather
                    // than being destroyed (§10.4).
                    if let Some(rest) = self.sim.player.inventory.add(stack, items) {
                        self.sim
                            .entities
                            .spawn_item(rest, drop_centre(block), glam::Vec3::ZERO);
                    }
                    // Only a break that yielded something wears the tool.
                    self.wear_held_tool();
                }
                None => log::debug!(
                    "{} yielded nothing",
                    registry.name_of(broken).unwrap_or("?")
                ),
            }
        }

        Arc::make_mut(&mut self.world).set_block(x, y, z, BlockId::AIR)
    }

    /// Spend one point of the held tool's durability, removing the stack when
    /// it reaches zero (`PHASE2_ARCHITECTURE.md` §4, decision C).
    ///
    /// A no-op for anything that is not a tool: only an item declaring
    /// `durability` carries [`ItemState::Durability`], so an empty hand or a
    /// stack of planks falls through untouched.
    ///
    /// The worn stack is rebuilt rather than mutated because `ItemStack`
    /// enforces its own invariant (a stack with state is a stack of one), and
    /// going through `ItemStack::new` is what keeps that enforcement in one
    /// place.
    fn wear_held_tool(&mut self) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        let inv = &mut self.sim.player.inventory;
        let slot = inv.selected_slot() as usize;
        let Some(stack) = inv.slot(slot) else {
            return;
        };
        let ItemState::Durability { remaining } = stack.state() else {
            return;
        };
        let left = remaining.saturating_sub(1);
        if left == 0 {
            inv.set_slot(slot, None);
            return;
        }
        let worn = ItemStack::new(
            stack.item(),
            stack.count(),
            ItemState::Durability { remaining: left },
            items.max_stack(stack.item()),
        )
        .ok();
        inv.set_slot(slot, worn);
    }

    /// Place the held hotbar item's block against the targeted face, consuming
    /// one of it.
    ///
    /// The same name mapping as [`break_block`](Self::break_block), backwards.
    /// An item with no matching block -- a stick, an ingot -- places nothing
    /// **and consumes nothing**: a click that does nothing must not quietly
    /// spend an item.
    pub fn place_block(&mut self) -> Option<ChunkCoord> {
        // An interactive block under the crosshair takes precedence over
        // placing. Otherwise a bench would be unusable the moment you are
        // holding anything -- which is most of the time.
        if self.interact() {
            return None;
        }

        let registry = self.blocks_registry.as_deref()?;
        let items = self.items.as_ref()?;
        let held = self.sim.player.inventory.selected_stack()?;
        let block = registry.id_of(items.name_of(held.item())?)?;

        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH, self.terrain())?;
        let target = [
            hit.block[0] + hit.normal[0],
            hit.block[1] + hit.normal[1],
            hit.block[2] + hit.normal[2],
        ];

        // Only now that the placement is certain to happen.
        let slot = self.sim.player.inventory.selected_slot() as usize;
        self.sim.player.inventory.take_one(slot, items)?;

        // A block that owns state gets it the moment it is placed, rather than
        // on first use -- so a furnace someone never opens still ticks, and the
        // world hash covers it either way.
        let interactive = self
            .blocks_registry
            .as_deref()
            .map(|r| r.interact(block) == Interact::Furnace)
            .unwrap_or(false);
        let world = Arc::make_mut(&mut self.world);
        let cc = world.set_block(target[0], target[1], target[2], block);
        if interactive {
            world.add_furnace(target);
        }
        Some(cc)
    }

    /// The hotbar reduced to what drawing needs: a swatch colour and a count
    /// per slot, plus which is held.
    ///
    /// The reduction lives here, not in `cubara-render`: that crate does not
    /// know what an item is, nor that slots 0..9 of a 36-slot array are the
    /// hotbar (Rule 3). Returns `None` before assets are set, so the HUD simply
    /// does not draw rather than drawing nine empty boxes.
    ///
    /// Colours come from `swatch_color`, the same deterministic name hash a
    /// block with no texture file already uses -- so a stone block in the world
    /// and a stone item in the hand read as the same material. Real item icons
    /// are art that does not exist yet.
    pub fn hotbar_slots(&self) -> Option<[Option<HotbarSlot>; HOTBAR_WIDTH]> {
        let items = self.items.as_ref()?;
        let inv = &self.sim.player.inventory;
        let mut out = [None; HOTBAR_WIDTH];
        for (i, out_slot) in out.iter_mut().enumerate() {
            let Some(stack) = inv.slot(i) else { continue };
            let Some(name) = items.name_of(stack.item()) else {
                continue;
            };
            *out_slot = Some(HotbarSlot {
                color: swatch_color(name),
                count: stack.count(),
            });
        }
        Some(out)
    }

    /// If the targeted block is interactive, act on it and report `true`.
    ///
    /// Reads [`Interact`] off the block registry rather than comparing names.
    /// The name comparison this replaces carried a note saying block 2.4 was
    /// the point to generalise it, "with two real cases to design against" --
    /// the furnace is that second case.
    fn interact(&mut self) -> bool {
        let (Some(registry), Some(terrain)) = (self.blocks_registry.as_deref(), self.terrain)
        else {
            return false;
        };
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let Some(hit) = self.world.raycast(origin, dir, REACH, self.terrain()) else {
            return false;
        };
        let [x, y, z] = hit.block;
        match registry.interact(self.world.block_at(x, y, z, terrain)) {
            Interact::None => false,
            Interact::Bench => {
                // Width lives on `Crafting` (world state), not on the screen: a
                // 3x3 grid holding items in its outer cells is a different world
                // from a 2x2 one, and the hash already covers it.
                self.sim.player.crafting.set_width(3);
                self.open_furnace = None;
                self.inventory_open = true;
                true
            }
            Interact::Furnace => {
                // A furnace placed before this block existed (or loaded from an
                // older save) has no entity yet; give it one on first use rather
                // than refusing to open.
                Arc::make_mut(&mut self.world).add_furnace([x, y, z]);
                self.open_furnace = Some([x, y, z]);
                self.inventory_open = true;
                true
            }
        }
    }

    /// One tick of every furnace in the world (`PHASE2_ARCHITECTURE.md` §7).
    ///
    /// Iterates positions in `BTreeMap` order, so which furnace ticks first is
    /// the positions' own order rather than a hash seed's -- Rule 1, and the
    /// same reason the hash iterates them that way.
    ///
    /// In this scope every furnace ticks every tick, because every loaded chunk
    /// is active. Block 2.6's dormant chunks and 2.7's catch-up are what change
    /// that, and [`Furnace::advance`] already takes an elapsed count so they can.
    fn tick_furnaces(&mut self) {
        let (Some(items), Some(smelting)) = (self.items.as_ref(), self.smelting.as_ref()) else {
            return;
        };

        // Bring the simulation radius up to date (§11): chunks the player has
        // left go dormant, chunks they have reached wake up.
        //
        // Only when the player has actually changed chunk. Standing still can
        // change no chunk's state, and this walks a (2r+1)²x3 box -- 243
        // lookups at radius 4 -- which is pure waste every tick the player is
        // not moving, which is most of them.
        let centre = ChunkCoord::from_world_pos(self.sim.player.pos.to_array());
        let now = self.sim.tick;
        let woken = if self.sim_centre == Some(centre) {
            Vec::new()
        } else {
            self.sim_centre = Some(centre);
            Arc::make_mut(&mut self.world).update_simulation_radius(centre, SIM_RADIUS_CHUNKS, now)
        };
        let caught_up: std::collections::BTreeMap<ChunkCoord, u64> =
            woken.into_iter().map(|w| (w.coord, w.elapsed)).collect();

        let world = Arc::make_mut(&mut self.world);
        let positions = world.block_entity_positions();
        if positions.is_empty() {
            return;
        }

        // **One pass over the block entities**, not one pass per chunk. The
        // obvious shape -- for each active chunk, find the entities in it --
        // is O(chunks x entities) and allocates a vector of every block entity
        // in the world for each of the 243 chunks in range. This is O(entities).
        for pos in positions {
            let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
            if world.chunk_states().get(coord) != ChunkState::Active {
                continue;
            }
            // A chunk that woke this tick owes the ticks it slept through *plus*
            // this one -- the same total the two-pass version produced, which is
            // what keeps the dormancy gate test passing.
            let ticks = caught_up.get(&coord).copied().unwrap_or(0) + 1;
            advance_furnace(world, pos, ticks, items, smelting);
        }
    }

    /// The furnace screen currently open, if any.
    pub fn open_furnace(&self) -> Option<Furnace> {
        let pos = self.open_furnace?;
        self.world.furnace_at(pos).copied()
    }

    /// Which ids the terrain is made of, or a treeless default before assets
    /// are set.
    ///
    /// Trees are solid, so physics and raycasting need this -- `is_solid_at`
    /// cannot answer from the density field alone any more. The fallback is a
    /// world with no trees rather than a panic: `Game::new()` runs before a
    /// window exists, and a headless test that never sets assets should still
    /// be able to walk around.
    fn terrain(&self) -> TerrainBlocks {
        self.terrain.unwrap_or(TerrainBlocks {
            oak: None,
            ores: cubara_world::OreSet::EMPTY,
            grass: BlockId::AIR,
            soil: BlockId::AIR,
            stone: BlockId::AIR,
        })
    }

    /// Whether the inventory screen is open.
    pub fn inventory_open(&self) -> bool {
        self.inventory_open
    }

    /// Open or close the screen. Closing is **refused** while the crafting grid
    /// cannot empty into the inventory (2.2b's `close`), so items in the grid
    /// are never eaten by walking away from them.
    pub fn toggle_inventory(&mut self) {
        if !self.inventory_open {
            self.inventory_open = true;
            return;
        }
        // A furnace screen has no crafting grid to empty -- its slots belong to
        // the block and stay in it. Only the cursor needs somewhere to go.
        if self.open_furnace.take().is_some() {
            self.inventory_open = false;
            if let Some(items) = self.items.as_ref() {
                let player = &mut self.sim.player;
                if let Some(held) = player.crafting.held() {
                    if let Some(lost) = player.inventory.add(held, items) {
                        log::debug!(
                            "inventory full: {} x{} lost closing a furnace",
                            items.name_of(lost.item()).unwrap_or("?"),
                            lost.count()
                        );
                    }
                    player.crafting.set_held(None);
                }
            }
            return;
        }
        let Some(items) = self.items.as_ref() else {
            self.inventory_open = false;
            return;
        };
        let player = &mut self.sim.player;
        if player.crafting.close(&mut player.inventory, items) {
            self.inventory_open = false;
            // Back to the inventory's own grid. `close` emptied all nine cells
            // regardless of width, so narrowing strands nothing.
            player.crafting.set_width(2);
        } else {
            log::debug!("inventory full: the crafting grid still holds items, staying open");
        }
    }

    /// Route a click on the open screen. `(x, y)` is in window pixels.
    ///
    /// The layout that decides *which* slot lives in `cubara-render` and is the
    /// same one the screen is drawn from, so a click cannot land on a slot other
    /// than the one under the cursor.
    pub fn click_panel(&mut self, x: f32, y: f32, right: bool, width: u32, height: u32) {
        let (Some(items), Some(book)) = (self.items.as_ref(), self.recipes.as_ref()) else {
            return;
        };
        let panel = match self.open_furnace {
            Some(_) => InventoryPanel::layout_furnace(width, height),
            None => InventoryPanel::layout(width, height, self.sim.player.crafting.width()),
        };
        let Some((kind, index)) = panel.hit(x, y) else {
            return;
        };
        if let Some(pos) = self.open_furnace {
            self.click_furnace(pos, kind, index);
            return;
        }
        let slot = match kind {
            PanelSlotKind::Inventory => SlotRef::Inventory(index),
            PanelSlotKind::Grid => SlotRef::Grid(index),
            PanelSlotKind::Result => SlotRef::Result,
            // A furnace slot cannot appear in the crafting layout; ignoring it
            // is the safe branch rather than mapping it to a grid cell.
            PanelSlotKind::Fuel => return,
        };
        let player = &mut self.sim.player;
        player
            .crafting
            .click(slot, right, &mut player.inventory, items, book);
    }

    /// A click on the open furnace's screen.
    ///
    /// Swap-on-click, matching the crafting cursor's feel: clicking a furnace
    /// slot with something held puts it in, clicking with an empty hand takes
    /// what is there. The output slot is take-only -- putting an ingot back
    /// into the output would be a way to duplicate work when the next smelt
    /// completes and stacks onto it.
    ///
    /// Uses the crafting cursor (`player.crafting.held()`) rather than a second
    /// one, so a player never has two things in hand at once and closing either
    /// screen has one rule for what happens to it.
    fn click_furnace(&mut self, pos: [i32; 3], kind: PanelSlotKind, index: usize) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        if kind == PanelSlotKind::Inventory {
            let player = &mut self.sim.player;
            player
                .crafting
                .click_inventory_only(index, &mut player.inventory, items);
            return;
        }
        let held = self.sim.player.crafting.held();
        let world = Arc::make_mut(&mut self.world);
        let Some(f) = world.furnace_at_mut(pos) else {
            return;
        };
        let slot = match kind {
            PanelSlotKind::Grid => &mut f.input,
            PanelSlotKind::Fuel => &mut f.fuel,
            PanelSlotKind::Result => {
                // Take-only.
                if held.is_none() {
                    if let Some((id, count)) = f.output.take() {
                        if let Ok(stack) = items.new_stack(id, count) {
                            self.sim.player.crafting.set_held(Some(stack));
                        }
                    }
                }
                return;
            }
            PanelSlotKind::Inventory => return,
        };
        match held {
            Some(stack) => {
                let previous = slot.replace((stack.item(), stack.count()));
                let give_back = previous.and_then(|(id, c)| items.new_stack(id, c).ok());
                self.sim.player.crafting.set_held(give_back);
            }
            None => {
                let taken = slot.take().and_then(|(id, c)| items.new_stack(id, c).ok());
                self.sim.player.crafting.set_held(taken);
            }
        }
    }

    /// The screen's layout and contents, or `None` when it is closed.
    ///
    /// Walks the same layout the renderer draws from, filling one entry per
    /// slot -- which is what keeps `contents` and `slots()` in step without
    /// either side knowing the other's ordering.
    pub fn panel_view(
        &self,
        width: u32,
        height: u32,
    ) -> Option<(InventoryPanel, Vec<Option<HotbarSlot>>, Option<HotbarSlot>)> {
        if !self.inventory_open {
            return None;
        }
        let items = self.items.as_ref()?;
        let crafting = &self.sim.player.crafting;
        let book = self.recipes.as_ref();
        let furnace = self.open_furnace();
        let panel = match self.open_furnace {
            Some(_) => InventoryPanel::layout_furnace(width, height),
            None => InventoryPanel::layout(width, height, crafting.width()),
        };

        let swatch = |stack: cubara_voxel::ItemStack| {
            items.name_of(stack.item()).map(|name| HotbarSlot {
                color: swatch_color(name),
                count: stack.count(),
            })
        };
        // A furnace slot holds `(id, count)` rather than an `ItemStack`, since
        // nothing in a furnace has durability.
        let furnace_swatch = |slot: Option<(cubara_voxel::ItemId, u8)>| {
            slot.and_then(|(id, count)| {
                items.name_of(id).map(|name| HotbarSlot {
                    color: swatch_color(name),
                    count,
                })
            })
        };

        let contents = panel
            .slots()
            .iter()
            .map(|s| match (s.kind, furnace) {
                (PanelSlotKind::Inventory, _) => {
                    self.sim.player.inventory.slot(s.index).and_then(swatch)
                }
                (PanelSlotKind::Grid, Some(f)) => furnace_swatch(f.input),
                (PanelSlotKind::Fuel, Some(f)) => furnace_swatch(f.fuel),
                (PanelSlotKind::Result, Some(f)) => furnace_swatch(f.output),
                (PanelSlotKind::Grid, None) => crafting.cell(s.index).and_then(swatch),
                (PanelSlotKind::Result, None) => book
                    .and_then(|b| crafting.result(b, items))
                    .and_then(swatch),
                // Only a furnace layout produces a fuel slot.
                (PanelSlotKind::Fuel, None) => None,
            })
            .collect();
        let held = crafting.held().and_then(swatch);
        Some((panel, contents, held))
    }

    /// Which hotbar slot is held, for the renderer.
    pub fn selected_hotbar_slot(&self) -> u8 {
        self.sim.player.inventory.selected_slot()
    }

    /// Select a hotbar slot (number keys 1-9, passed as 0-8).
    pub fn select_hotbar(&mut self, index: u8) {
        self.sim.player.inventory.select(index);
    }
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaking_clears_the_targeted_block() {
        // No GPU involved — this is why gameplay does not belong on the renderer.
        let mut game = Game::new();
        // Look straight down from above the terrain.
        game.sim.player = Player::new(glam::vec3(0.5, 60.0, 0.5), 0.0, -1.5);
        let hit = game
            .world()
            .raycast([0.5, 60.0, 0.5], [0.0, -1.0, 0.0], 100.0, game.terrain())
            .expect("ground below");

        // Out of reach from 60 blocks up: nothing changes.
        assert_eq!(game.break_block(), None);
        assert!(game
            .world()
            .is_solid_at(hit.block[0], hit.block[1], hit.block[2], game.terrain()));
    }

    #[test]
    fn editing_within_reach_marks_a_chunk_dirty() {
        let mut game = Game::new();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        // Stand just above the surface, looking down — now it is within reach.
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        game.sim.player = Player::new(eye, 0.0, -1.5);

        let dirty = game.break_block().expect("a block was in reach");
        assert!(
            !game.world().is_solid_at(
                ground.block[0],
                ground.block[1],
                ground.block[2],
                game.terrain()
            ),
            "the targeted block is now air"
        );
        let b = ground.block;
        assert_eq!(
            dirty,
            ChunkCoord::from_world_pos([b[0] as f32, b[1] as f32, b[2] as f32]),
            "the dirty chunk is the one containing the broken block"
        );
    }

    /// A game with the real registries wired in, standing just above the
    /// ground and looking down -- the fixture every break/place test needs.
    /// Uses the shipped `assets/`, not a synthetic registry: a block whose
    /// name has no matching item file is exactly the failure these tests
    /// should catch, and a fixture would hide it.
    fn game_looking_at_ground() -> (Game, [i32; 3]) {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        game.sim.player = Player::new(eye, 0.0, -1.5);
        (game, ground.block)
    }

    #[test]
    fn breaking_a_block_puts_its_item_in_the_inventory() {
        let (mut game, block) = game_looking_at_ground();
        let terrain = game.terrain.expect("assets are set");
        let broken = game.world().block_at(block[0], block[1], block[2], terrain);
        let name = game
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(broken)
            .expect("the block has a name")
            .to_string();

        game.break_block().expect("a block was in reach");

        let items = game.items.as_ref().unwrap();
        let stack = game
            .sim
            .player
            .inventory
            .slot(0)
            .expect("slot 0 holds the drop");
        assert_eq!(
            items.name_of(stack.item()),
            Some(name.as_str()),
            "breaking {name} must yield the item of the same name"
        );
        assert_eq!(stack.count(), 1);
    }

    #[test]
    fn placing_consumes_exactly_one_of_the_held_stack() {
        let (mut game, _) = game_looking_at_ground();
        // The terrain's own stone, resolved by name in both registries, so
        // this test does not depend on which materials happen to ship -- only
        // on the block and its item sharing a name, which is the drop policy.
        let stone_name = game
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(game.terrain.unwrap().stone)
            .expect("terrain stone has a name")
            .to_string();
        let items = game.items.as_ref().unwrap();
        let held = items
            .id_of(&stone_name)
            .expect("every shipped block needs an item of the same name to be placeable");
        let stack = items.new_stack(held, 5).unwrap();
        game.sim.player.inventory.add(stack, items);
        game.select_hotbar(0);

        game.place_block().expect("a face was in reach");

        assert_eq!(
            game.sim.player.inventory.slot(0).map(|s| s.count()),
            Some(4),
            "placing spends exactly one"
        );
    }

    #[test]
    fn placing_with_an_empty_hand_changes_nothing() {
        let (mut game, _) = game_looking_at_ground();
        let before = game.world().edit_count();
        assert_eq!(game.place_block(), None, "nothing held, nothing placed");
        assert_eq!(game.world().edit_count(), before);
    }

    #[test]
    fn placing_an_item_with_no_block_consumes_nothing() {
        // A stick is not a block. The click must do nothing *and* not quietly
        // spend the stick -- an action that fails silently should not also
        // cost you something.
        let (mut game, _) = game_looking_at_ground();
        let items = game.items.as_ref().unwrap();
        let stick = items
            .id_of("cubara:stick")
            .expect("assets/items has a stick");
        game.sim
            .player
            .inventory
            .add(items.new_stack(stick, 3).unwrap(), items);
        game.select_hotbar(0);

        let before = game.world().edit_count();
        assert_eq!(game.place_block(), None);
        assert_eq!(game.world().edit_count(), before, "nothing was placed");
        assert_eq!(
            game.sim.player.inventory.slot(0).map(|s| s.count()),
            Some(3),
            "and nothing was consumed"
        );
    }

    #[test]
    fn breaking_with_a_full_inventory_still_breaks_the_block() {
        // The drop is lost -- there are no dropped-item entities until ECS
        // (2.5). Recorded behaviour, not a silent bug: what must not happen is
        // the block refusing to break, which would read as the game being stuck.
        let (mut game, block) = game_looking_at_ground();
        let items = game.items.as_ref().unwrap();
        let filler = items.id_of("cubara:stick").unwrap();
        for _ in 0..cubara_sim::SLOT_COUNT {
            game.sim
                .player
                .inventory
                .add(items.new_stack(filler, 64).unwrap(), items);
        }

        game.break_block().expect("a block was in reach");
        assert!(
            !game
                .world()
                .is_solid_at(block[0], block[1], block[2], game.terrain()),
            "the block breaks even when the drop has nowhere to go"
        );
    }

    #[test]
    fn number_keys_select_hotbar_slots_on_press_only() {
        let mut game = Game::new();
        assert!(game.key_input(KeyCode::Digit4, true));
        assert_eq!(game.sim.player.inventory.selected_slot(), 3);

        // Releasing must not reselect -- otherwise every key-up would snap the
        // selection back to whichever number was let go of last.
        game.key_input(KeyCode::Digit1, true);
        game.key_input(KeyCode::Digit4, false);
        assert_eq!(game.sim.player.inventory.selected_slot(), 0);
    }

    /// Place a bench right where the player is looking, and aim at it.
    fn game_facing_a_bench() -> (Game, [i32; 3]) {
        let (mut game, ground) = game_looking_at_ground();
        let bench = game
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:crafting_bench")
            .expect("assets/blocks defines the bench");
        std::sync::Arc::make_mut(&mut game.world).set_block(ground[0], ground[1], ground[2], bench);
        (game, ground)
    }

    #[test]
    fn right_clicking_a_bench_opens_the_three_by_three_grid() {
        let (mut game, _) = game_facing_a_bench();
        let items = game.items.as_ref().unwrap();
        // Holding something placeable, to prove interaction wins over placing.
        game.sim.player.inventory.add(
            items
                .new_stack(items.id_of("cubara:stone").unwrap(), 5)
                .unwrap(),
            items,
        );
        game.select_hotbar(0);
        let before = game.world().edit_count();

        assert_eq!(game.place_block(), None, "no block was placed");
        assert_eq!(game.world().edit_count(), before, "the world is unchanged");
        assert!(game.inventory_open(), "the screen opened");
        assert_eq!(game.sim.player.crafting.width(), 3, "at bench size");
    }

    #[test]
    fn right_clicking_anything_else_still_places() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.items.as_ref().unwrap();
        game.sim.player.inventory.add(
            items
                .new_stack(items.id_of("cubara:stone").unwrap(), 5)
                .unwrap(),
            items,
        );
        game.select_hotbar(0);

        assert!(game.place_block().is_some(), "a normal block still places");
        assert!(!game.inventory_open(), "and no screen opened");
    }

    /// Put one `item` into grid `cell`, the way a player would: pick it up
    /// from a scratch inventory, then put it down. Going through `click`
    /// rather than reaching into the cells keeps these tests honest about the
    /// path the real game takes.
    fn load_cell(game: &mut Game, cell: usize, item: cubara_voxel::ItemId, count: u8) {
        let items = game.items.as_ref().unwrap();
        let book = game.recipes.as_ref().unwrap();
        let mut scratch = cubara_sim::Inventory::new();
        scratch.add(items.new_stack(item, count).unwrap(), items);
        let mut c = game.sim.player.crafting;
        c.click(
            cubara_sim::SlotRef::Inventory(0),
            false,
            &mut scratch,
            items,
            book,
        );
        c.click(
            cubara_sim::SlotRef::Grid(cell),
            false,
            &mut scratch,
            items,
            book,
        );
        game.sim.player.crafting = c;
    }

    #[test]
    fn closing_a_bench_returns_its_outer_cells_and_narrows() {
        // Cell 8 is the bottom-right of a 3x3 -- unreachable at width 2.
        // Narrowing must not strand it. `Crafting::close` empties all nine
        // cells regardless of width deliberately, and this is the test that
        // says why that mattered.
        let (mut game, _) = game_facing_a_bench();
        game.place_block();
        assert_eq!(game.sim.player.crafting.width(), 3);

        let stone = game.items.as_ref().unwrap().id_of("cubara:stone").unwrap();
        load_cell(&mut game, 8, stone, 4);
        assert!(
            game.sim.player.crafting.cell(8).is_some(),
            "cell 8 is loaded"
        );

        game.toggle_inventory();
        assert!(!game.inventory_open(), "it closed");
        assert_eq!(game.sim.player.crafting.width(), 2, "and narrowed");
        assert!(
            game.sim.player.crafting.cell(8).is_none(),
            "the outer cell was emptied, not stranded"
        );
        assert!(
            game.sim
                .player
                .inventory
                .slots()
                .flatten()
                .any(|s| s.item() == stone),
            "and its contents came back to the inventory"
        );
    }

    #[test]
    fn a_wooden_pick_can_be_crafted_at_a_bench() {
        // The 3x3 recipe that is unreachable without this issue -- and the
        // first rung of the ladder that needs a bench at all.
        let (mut game, _) = game_facing_a_bench();
        game.place_block();

        let (plank, stick) = {
            let items = game.items.as_ref().unwrap();
            (
                items.id_of("cubara:plank").unwrap(),
                items.id_of("cubara:stick").unwrap(),
            )
        };
        // PPP / .S. / .S.
        for (cell, item) in [(0, plank), (1, plank), (2, plank), (4, stick), (7, stick)] {
            load_cell(&mut game, cell, item, 1);
        }

        let items = game.items.as_ref().unwrap();
        let made = game
            .sim
            .player
            .crafting
            .result(game.recipes.as_ref().unwrap(), items)
            .expect("the grid makes something");
        assert_eq!(
            items.name_of(made.item()),
            Some("cubara:wooden_pick"),
            "a bench makes the wooden pick"
        );
    }

    #[test]
    fn every_shipped_block_has_an_item_of_the_same_name() {
        // The drop policy is "block name -> item of the same name"
        // (PHASE2_ARCHITECTURE.md 4.1), so a block with no matching item file
        // silently drops nothing. That is not a crash, not a warning, and not
        // visible until someone mines it and wonders why their inventory is
        // empty -- which is exactly how this was found: three of the three
        // blocks the world is made of had no items.
        //
        // Until 2.4 replaces the policy with real `drops:` tables, this is what
        // keeps the two asset directories in step.
        let registry = cubara_render::load_registry();
        let items = load_item_registry();

        let missing: Vec<&str> = registry
            .ids()
            .filter(|&id| id != BlockId::AIR)
            .filter_map(|id| registry.name_of(id))
            .filter(|name| items.id_of(name).is_none())
            .collect();

        assert!(
            missing.is_empty(),
            "these blocks would drop nothing when broken -- add assets/items/<name>.ron              for each, or give 2.4's drop table an entry: {missing:?}"
        );
    }

    #[test]
    fn key_input_reports_whether_the_key_was_mapped() {
        let mut game = Game::new();
        assert!(game.key_input(KeyCode::KeyW, true));
        assert!(game.key_input(KeyCode::F4, true), "the free-fly toggle key");
        assert!(!game.key_input(KeyCode::KeyP, true), "unmapped key");
    }

    #[test]
    fn fly_toggle_flips_the_mode_on_a_single_press() {
        let mut game = Game::new();
        assert!(
            !game.sim.player.is_free_fly(),
            "walking is the default mode"
        );
        game.key_input(KeyCode::F4, true);
        game.advance(TICK_DT);
        assert!(game.sim.player.is_free_fly());
    }

    #[test]
    fn fly_toggle_edge_is_consumed_once_not_once_per_catchup_tick() {
        // A single key press must flip the mode exactly once, even when it
        // lands in a frame whose accumulator backlog forces several ticks to
        // run in one `advance` call -- reusing the same `InputFrame` across a
        // catch-up burst is correct for held movement, but a button edge
        // reapplied on every one of those ticks would flip the mode back and
        // forth instead of once. Two ticks makes a naive double-application
        // observable: it would leave the mode back at `false`.
        let mut game = Game::new();
        game.key_input(KeyCode::F4, true);
        game.advance(2.0 * TICK_DT);
        assert_eq!(game.sim.tick, 2);
        assert!(
            game.sim.player.is_free_fly(),
            "one press should flip the mode once (false -> true), not twice (-> false)"
        );
    }

    #[test]
    fn advance_by_exactly_one_tick_worth_of_time_runs_one_tick() {
        let mut game = Game::new();
        assert_eq!(game.sim.tick, 0);
        game.advance(TICK_DT);
        assert_eq!(game.sim.tick, 1);
    }

    #[test]
    fn sub_tick_dt_does_not_run_a_tick_yet() {
        let mut game = Game::new();
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.sim.tick, 0, "half a tick's worth of time isn't a tick");
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.sim.tick, 1, "the other half completes it");
    }

    #[test]
    fn mouse_look_across_sub_tick_frames_is_not_dropped() {
        // The reported "can't look around on a fast machine" bug. The renderer
        // runs uncapped, so at high FPS most frames are shorter than one 60 Hz
        // tick and run zero ticks. Mouse motion arriving on those frames must
        // accumulate until a tick consumes it -- not be sampled and discarded
        // frame by frame, which dropped nearly all of it on a fast GPU.
        let mut spread = Game::new();
        for _ in 0..5 {
            spread.mouse_look(100.0, 0.0);
            spread.advance(TICK_DT * 0.1); // sub-tick: no tick runs yet
        }
        assert_eq!(
            spread.sim.tick, 0,
            "5 * 0.1 tick < one tick: nothing simulated"
        );
        spread.advance(TICK_DT); // now cross the threshold -> exactly one tick
        assert_eq!(spread.sim.tick, 1);

        // The same 500 px of motion delivered in a single tick-sized frame.
        let mut once = Game::new();
        once.mouse_look(500.0, 0.0);
        once.advance(TICK_DT);
        assert_eq!(once.sim.tick, 1);

        assert_eq!(
            spread.sim.player.look_dir(),
            once.sim.player.look_dir(),
            "mouse motion spread over sub-tick frames must turn the player by \
             the same total as the same motion in one frame -- none dropped"
        );
    }

    #[test]
    fn a_multi_tick_catch_up_burst_applies_mouse_look_only_once() {
        // The opposite failure: `look_delta` is a one-shot accumulated total,
        // not a held state like `move_axes`. Reusing the unmodified frame across
        // every tick of a catch-up burst would multiply one frame's mouse motion
        // by however many ticks ran -- sporadic, inconsistent-feeling turns.
        let mut single = Game::new();
        single.mouse_look(1000.0, 0.0);
        single.advance(TICK_DT); // exactly one tick

        let mut burst = Game::new();
        burst.mouse_look(1000.0, 0.0);
        burst.advance(3.0 * TICK_DT); // three ticks in one catch-up burst

        assert_eq!(single.sim.tick, 1);
        assert_eq!(burst.sim.tick, 3);
        assert_eq!(
            single.sim.player.look_dir(),
            burst.sim.player.look_dir(),
            "the same single mouse-look delta must turn the player by the same \
             amount regardless of how many ticks ran in the same `advance` call"
        );
    }

    #[test]
    fn a_huge_dt_is_capped_not_caught_up_in_one_frame() {
        let mut game = Game::new();
        game.advance(1000.0 * TICK_DT); // a 1000-tick backlog in one call
        assert_eq!(
            game.sim.tick, MAX_TICKS_PER_FRAME as u64,
            "capped at MAX_TICKS_PER_FRAME, not fully caught up"
        );
        assert_eq!(
            game.accumulator, 0.0,
            "the leftover backlog is dropped, not carried into the next frame"
        );
    }

    #[test]
    fn frame_rate_independent_movement_reaches_the_same_state() {
        // The property Rule 1 exists for: the same input, driven by wildly
        // different frame timings that sum to the same elapsed time, must land
        // on identical sim state -- not "close", identical. 1000 ticks, per the
        // issue's own "Done when" (#57).
        let mut steady = Game::new();
        let mut jittery = Game::new();
        steady.key_input(KeyCode::KeyW, true);
        jittery.key_input(KeyCode::KeyW, true);

        let total_ticks = 1000u64;
        let total_time = total_ticks as f64 * TICK_DT as f64;

        // 1000 frames of exactly one tick's worth of time each.
        for _ in 0..total_ticks {
            steady.advance(TICK_DT);
        }

        // The same total time, spread over wildly uneven frame deltas -- some
        // under a tick, some several ticks at once (forcing the catch-up loop),
        // in `f64` so the *test's own* bookkeeping isn't what introduces drift
        // (`Game::advance`'s internal accumulator is `f64` for the same reason).
        let mut elapsed = 0.0f64;
        let deltas: [f64; 5] = [0.1, 3.7, 0.02, 1.0, 0.5].map(|m| m * TICK_DT as f64);
        let mut i = 0;
        while elapsed < total_time {
            let remaining = total_time - elapsed;
            let dt = deltas[i % deltas.len()].min(remaining);
            jittery.advance(dt as f32);
            elapsed += dt;
            i += 1;
        }

        assert_eq!(steady.sim.tick, jittery.sim.tick);
        assert_eq!(steady.sim.player, jittery.sim.player);
    }

    /// Put `block` directly in front of the player and aim at it, so a test can
    /// choose which material it breaks rather than taking whatever terrain is
    /// underfoot. Returns the position it was placed at.
    fn stand_over(game: &mut Game, block_name: &str) -> [i32; 3] {
        let id = game
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of(block_name)
            .unwrap_or_else(|| panic!("no block {block_name}"));
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let b = ground.block;
        Arc::make_mut(&mut game.world).set_block(b[0], b[1], b[2], id);
        b
    }

    /// Give the player `item` in the selected hotbar slot.
    fn hold(game: &mut Game, item: &str) {
        let items = game.items.as_ref().unwrap();
        let id = items
            .id_of(item)
            .unwrap_or_else(|| panic!("no item {item}"));
        let stack = items.new_stack(id, 1).expect("a stack of one");
        let slot = game.sim.player.inventory.selected_slot() as usize;
        game.sim.player.inventory.set_slot(slot, Some(stack));
    }

    fn count_of(game: &Game, item: &str) -> u8 {
        let items = game.items.as_ref().unwrap();
        let Some(id) = items.id_of(item) else {
            return 0;
        };
        (0..cubara_sim::SLOT_COUNT)
            .filter_map(|i| game.sim.player.inventory.slot(i))
            .filter(|s| s.item() == id)
            .map(|s| s.count())
            .sum()
    }

    #[test]
    fn a_tool_below_the_required_tier_breaks_the_block_but_yields_nothing() {
        // §4's rule, and the reason it was chosen over refusing to break: the
        // block goes, the drop does not. Iron ore needs tier 2; a wooden pick
        // is tier 1.
        let (mut game, _) = game_looking_at_ground();
        let b = stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");

        game.break_block().expect("a block was in reach");

        assert!(
            !game.world().is_solid_at(b[0], b[1], b[2], game.terrain()),
            "the block still breaks"
        );
        assert_eq!(count_of(&game, "cubara:raw_iron"), 0, "but yields nothing");
    }

    #[test]
    fn the_required_tier_yields_the_declared_drop_not_the_block() {
        // The other half: a stone pick is tier 2, so iron ore yields
        // `cubara:raw_iron` -- the declared drop, and *not* an item named after
        // the block, which is what the pre-2.4a policy would have given.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:stone_pick");

        game.break_block().expect("a block was in reach");

        assert_eq!(count_of(&game, "cubara:raw_iron"), 1);
        assert_eq!(count_of(&game, "cubara:iron_ore"), 0);
    }

    #[test]
    fn stone_yields_cobble() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:wooden_pick");

        game.break_block().expect("a block was in reach");

        assert_eq!(count_of(&game, "cubara:cobble"), 1);
        assert_eq!(count_of(&game, "cubara:stone"), 0);
    }

    #[test]
    fn stone_by_hand_yields_nothing() {
        // requires_tier 1, and the empty hand is tier 0.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");

        game.break_block().expect("a block was in reach");

        assert_eq!(count_of(&game, "cubara:cobble"), 0);
    }

    #[test]
    fn leaves_yield_nothing_whatever_is_held() {
        // `drops: Nothing` ignores the tool entirely -- an iron pick is tier 3
        // and still gets no leaves.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:oak_leaves");
        hold(&mut game, "cubara:iron_pick");

        game.break_block().expect("a block was in reach");

        assert_eq!(count_of(&game, "cubara:oak_leaves"), 0);
    }

    #[test]
    fn a_successful_break_costs_one_durability() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        let before = match game.sim.player.inventory.selected_stack().unwrap().state() {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block().expect("a block was in reach");

        let after = match game.sim.player.inventory.selected_stack().unwrap().state() {
            ItemState::Durability { remaining } => remaining,
            other => panic!("still a pick, got {other:?}"),
        };
        assert_eq!(after, before - 1);
    }

    #[test]
    fn a_failed_tier_break_costs_no_durability() {
        // §4: you are not punished twice for the same mistake.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");
        let before = match game.sim.player.inventory.selected_stack().unwrap().state() {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block().expect("a block was in reach");

        let after = match game.sim.player.inventory.selected_stack().unwrap().state() {
            ItemState::Durability { remaining } => remaining,
            other => panic!("still a pick, got {other:?}"),
        };
        assert_eq!(after, before, "the wasted swing cost nothing");
    }

    #[test]
    fn a_tool_at_zero_durability_leaves_the_slot() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.items.as_ref().unwrap();
        let pick = items.id_of("cubara:stone_pick").unwrap();
        let nearly_dead = ItemStack::new(
            pick,
            1,
            ItemState::Durability { remaining: 1 },
            items.max_stack(pick),
        )
        .expect("a worn pick");
        let slot = game.sim.player.inventory.selected_slot() as usize;
        game.sim.player.inventory.set_slot(slot, Some(nearly_dead));
        stand_over(&mut game, "cubara:stone");

        game.break_block().expect("a block was in reach");

        assert!(
            game.sim.player.inventory.slot(slot).is_none(),
            "the spent tool is gone"
        );
        assert_eq!(
            count_of(&game, "cubara:cobble"),
            1,
            "the last break counted"
        );
    }

    #[test]
    fn breaking_bare_handed_is_not_an_error() {
        // Nothing to wear, and the hand is tier 0 -- soil requires nothing, so
        // this still yields.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:soil");

        game.break_block().expect("a block was in reach");

        assert_eq!(count_of(&game, "cubara:soil"), 1);
    }

    /// Aim at `block_name` placed underfoot and hold the break button, then run
    /// `ticks` sim ticks. Returns how many ticks it took to break, or `None` if
    /// it had not broken by then.
    fn mine_for(game: &mut Game, ticks: u32) -> Option<u32> {
        game.set_breaking(true);
        for t in 1..=ticks {
            let dirty = game.advance(TICK_DT);
            if !dirty.is_empty() {
                return Some(t);
            }
        }
        None
    }

    #[test]
    fn mining_takes_ceil_hardness_over_speed_ticks() {
        // §4.3's formula, on the real assets: stone is hardness 30, a stone
        // pick is speed 4, so ceil(30/4) = 8 ticks.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");

        assert_eq!(mine_for(&mut game, 20), Some(8));
    }

    #[test]
    fn a_faster_tool_breaks_the_same_block_in_fewer_ticks() {
        // The whole point of the block: the tool changes the time, not just
        // whether you get a drop. Stone at hardness 30: hand 30, wooden 15,
        // stone 8, iron 5.
        let cases = [
            (None, 30),
            (Some("cubara:wooden_pick"), 15),
            (Some("cubara:stone_pick"), 8),
            (Some("cubara:iron_pick"), 5),
        ];
        for (tool, want) in cases {
            let (mut game, _) = game_looking_at_ground();
            stand_over(&mut game, "cubara:stone");
            if let Some(t) = tool {
                hold(&mut game, t);
            }
            assert_eq!(
                mine_for(&mut game, 60),
                Some(want),
                "wrong tick count for {tool:?}"
            );
        }
    }

    #[test]
    fn releasing_the_button_abandons_progress() {
        // §4.3: abandoned, not banked. Six ticks of an eight-tick break, then
        // let go -- starting again must cost the full eight, not two.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");

        game.set_breaking(true);
        for _ in 0..6 {
            assert!(game.advance(TICK_DT).is_empty());
        }
        game.set_breaking(false);
        game.advance(TICK_DT);

        assert_eq!(mine_for(&mut game, 20), Some(8), "restarted from zero");
    }

    #[test]
    fn switching_tools_abandons_progress() {
        // The stored break is keyed by tool as well as position.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:wooden_pick");

        game.set_breaking(true);
        for _ in 0..10 {
            assert!(game.advance(TICK_DT).is_empty(), "wooden pick needs 15");
        }
        hold(&mut game, "cubara:stone_pick");

        // Fresh start at speed 4: eight more ticks, not the two that would be
        // left if the wooden pick's progress had carried over.
        assert_eq!(mine_for(&mut game, 20), Some(8));
    }

    #[test]
    fn a_timed_break_applies_the_same_drop_rules_as_an_instant_one() {
        // `break_at` is shared, so 2.4a's tier gate still holds: iron ore
        // needs tier 2, and a wooden pick mines it (slowly) for nothing.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:wooden_pick");

        // hardness 45 at speed 2 -> 23 ticks.
        assert_eq!(mine_for(&mut game, 40), Some(23));
        assert_eq!(count_of(&game, "cubara:raw_iron"), 0, "tier too low");

        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:iron_ore");
        hold(&mut game, "cubara:stone_pick");
        // hardness 45 at speed 4 -> 12 ticks.
        assert_eq!(mine_for(&mut game, 40), Some(12));
        assert_eq!(count_of(&game, "cubara:raw_iron"), 1);
    }

    #[test]
    fn mining_progress_reports_a_fraction_that_climbs_to_the_break() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        assert_eq!(game.mining_progress(), None, "nothing started yet");

        game.set_breaking(true);
        let mut last = 0.0;
        for _ in 0..7 {
            game.advance(TICK_DT);
            let p = game.mining_progress().expect("a break is in progress");
            assert!(p > last, "progress must climb: {p} after {last}");
            assert!(p < 1.0, "not finished yet: {p}");
            last = p;
        }
        game.advance(TICK_DT);
        assert_eq!(game.mining_progress(), None, "finished, so nothing pending");
    }

    #[test]
    fn mining_is_tick_identical_across_two_runs() {
        // Rule 1: same inputs, same tick, same result. Two independent games
        // driven identically must break on the same tick.
        let run = || {
            let (mut game, _) = game_looking_at_ground();
            stand_over(&mut game, "cubara:iron_ore");
            hold(&mut game, "cubara:stone_pick");
            mine_for(&mut game, 40)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn one_frame_of_several_ticks_advances_mining_by_all_of_them() {
        // A catch-up burst is N ticks of progress, unlike `jump`, which is a
        // one-shot. That difference is deliberate (see `InputFrame::breaking`).
        //
        // Five ticks, not more: `MAX_TICKS_PER_FRAME` caps a frame's catch-up
        // at five, so an iron pick (speed 6) on stone (hardness 30) is the
        // longest break that can finish inside one frame.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:iron_pick");
        game.set_breaking(true);

        // 5.5 rather than exactly 5.0: `TICK_DT * 5.0` in `f32` can land a
        // hair under five ticks' worth once widened to the `f64` accumulator,
        // and this test is about the burst, not about a rounding boundary.
        // The surplus stays in the accumulator; the cap still limits it to five.
        let dirty = game.advance(TICK_DT * 5.5);
        assert!(!dirty.is_empty(), "five ticks in one frame breaks it");
    }

    #[test]
    fn a_frame_longer_than_the_catch_up_cap_still_only_mines_the_cap() {
        // The spiral-of-death guard applies to mining too: a frame worth 20
        // ticks runs five, so a break needing eight is not finished by it.
        // Mining must not be a way to smuggle progress past the cap.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        game.set_breaking(true);

        assert!(
            game.advance(TICK_DT * 20.0).is_empty(),
            "capped at five ticks, and stone at speed 4 needs eight"
        );
    }

    /// Place a furnace at the block the player is looking at and open it.
    fn open_a_furnace(game: &mut Game) -> [i32; 3] {
        let pos = stand_over(game, "cubara:furnace");
        Arc::make_mut(&mut game.world).add_furnace(pos);
        game.open_furnace = Some(pos);
        game.inventory_open = true;
        pos
    }

    fn item(game: &Game, name: &str) -> cubara_voxel::ItemId {
        game.items.as_ref().unwrap().id_of(name).expect(name)
    }

    #[test]
    fn right_clicking_a_furnace_opens_its_screen() {
        // And it reads `Interact` off the registry rather than comparing names,
        // which is what block 2.4c generalised.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:furnace");

        assert!(game.interact(), "the furnace is interactive");
        assert!(game.inventory_open());
        assert!(
            game.open_furnace().is_some(),
            "a furnace screen, not a bench"
        );
    }

    #[test]
    fn a_bench_still_opens_the_three_by_three_grid() {
        // The same registry lookup must keep the bench working -- Rule 5's
        // "one implementation" cuts both ways.
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:crafting_bench");

        assert!(game.interact());
        assert!(game.inventory_open());
        assert!(game.open_furnace().is_none(), "a bench, not a furnace");
        assert_eq!(game.sim.player.crafting.width(), 3);
    }

    #[test]
    fn a_furnace_smelts_raw_iron_into_an_ingot_over_ticks() {
        // The last rung of REQUIREMENTS #5: ore you mined becomes metal you can
        // craft with. 200 ticks per ingot, and a log burns 80 -- so this needs
        // three logs' worth of fuel, which is the point of checking it end to
        // end rather than trusting the unit tests.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        {
            let f = Arc::make_mut(&mut game.world).furnace_at_mut(pos).unwrap();
            f.input = Some((raw, 1));
            f.fuel = Some((log, 4));
        }

        for _ in 0..210 {
            game.advance(TICK_DT);
        }

        let f = game.open_furnace().expect("still open");
        assert_eq!(
            f.output,
            Some((item(&game, "cubara:iron_ingot"), 1)),
            "one ingot"
        );
        assert_eq!(f.input, None, "the raw iron was consumed");
    }

    #[test]
    fn a_furnace_with_no_fuel_smelts_nothing() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        Arc::make_mut(&mut game.world)
            .furnace_at_mut(pos)
            .unwrap()
            .input = Some((raw, 1));

        for _ in 0..400 {
            game.advance(TICK_DT);
        }

        let f = game.open_furnace().unwrap();
        assert_eq!(f.output, None);
        assert_eq!(f.input, Some((raw, 1)), "nothing consumed either");
    }

    #[test]
    fn clicking_the_furnace_slots_puts_items_in_and_takes_them_out() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let stack = game.items.as_ref().unwrap().new_stack(raw, 3).unwrap();
        game.sim.player.crafting.set_held(Some(stack));

        // Into the input slot.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        assert_eq!(
            game.open_furnace().unwrap().input,
            Some((raw, 3)),
            "the held stack went in"
        );
        assert!(game.sim.player.crafting.held().is_none(), "hand is empty");

        // And back out.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        assert_eq!(game.open_furnace().unwrap().input, None);
        assert_eq!(game.sim.player.crafting.held().map(|s| s.count()), Some(3));
    }

    #[test]
    fn the_output_slot_is_take_only() {
        // Putting something back into the output would let the next completed
        // smelt stack onto it, which is work out of nothing.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let stack = game.items.as_ref().unwrap().new_stack(raw, 1).unwrap();
        game.sim.player.crafting.set_held(Some(stack));

        game.click_furnace(pos, PanelSlotKind::Result, 0);

        assert_eq!(game.open_furnace().unwrap().output, None, "nothing went in");
        assert!(game.sim.player.crafting.held().is_some(), "still held");
    }

    #[test]
    fn breaking_a_furnace_takes_its_state_with_it() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        Arc::make_mut(&mut game.world)
            .furnace_at_mut(pos)
            .unwrap()
            .input = Some((raw, 5));

        game.break_at(pos);

        assert!(game.world().furnace_at(pos).is_none(), "the entity is gone");
        assert!(!game.inventory_open(), "and its screen closed");
    }

    #[test]
    fn placing_a_furnace_gives_it_state_immediately() {
        // Not on first use: a furnace nobody opens must still tick, and the
        // world hash must cover it either way.
        let (mut game, _) = game_looking_at_ground();
        let furnace = game
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:furnace")
            .unwrap();
        let items = game.items.as_ref().unwrap();
        let id = items.id_of("cubara:furnace").unwrap();
        let stack = items.new_stack(id, 1).unwrap();
        let slot = game.sim.player.inventory.selected_slot() as usize;
        game.sim.player.inventory.set_slot(slot, Some(stack));

        let cc = game.place_block().expect("placed");
        let _ = (furnace, cc);
        assert_eq!(
            game.world().block_entities().count(),
            1,
            "the placed furnace owns state"
        );
    }

    #[test]
    fn closing_a_furnace_screen_keeps_its_contents_in_the_block() {
        // Unlike a crafting grid, whose cells are emptied into the inventory on
        // close: a furnace's slots belong to the block, not to the screen.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        Arc::make_mut(&mut game.world)
            .furnace_at_mut(pos)
            .unwrap()
            .input = Some((raw, 2));

        game.toggle_inventory();

        assert!(!game.inventory_open(), "closed");
        assert_eq!(
            game.world().furnace_at(pos).unwrap().input,
            Some((raw, 2)),
            "contents stayed in the furnace"
        );
    }

    #[test]
    fn a_drop_that_does_not_fit_falls_on_the_floor_instead_of_vanishing() {
        // Block 2.5's whole reason for existing. Before this, the item was
        // logged and destroyed.
        let (mut game, _) = game_looking_at_ground();
        let b = stand_over(&mut game, "cubara:soil");
        // Fill every slot with something that will not stack with soil.
        let raw = item(&game, "cubara:raw_iron");
        for i in 0..cubara_sim::SLOT_COUNT {
            let full = game.items.as_ref().unwrap().new_stack(raw, 64).unwrap();
            game.sim.player.inventory.set_slot(i, Some(full));
        }

        game.break_at(b);

        assert_eq!(game.sim.entities.len(), 1, "the drop is on the floor");
        let (_, d) = game.sim.entities.sorted()[0];
        assert_eq!(
            game.items.as_ref().unwrap().name_of(d.stack.item()),
            Some("cubara:soil")
        );
    }

    #[test]
    fn a_broken_furnace_spills_its_contents() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        {
            let f = Arc::make_mut(&mut game.world).furnace_at_mut(pos).unwrap();
            f.input = Some((raw, 4));
            f.fuel = Some((log, 2));
        }

        game.break_at(pos);

        // Three would-be-lost stacks: input, fuel, and the furnace's own drop
        // goes to the inventory, so two entities plus whatever did not fit.
        assert!(
            game.sim.entities.len() >= 2,
            "input and fuel are on the floor, got {}",
            game.sim.entities.len()
        );
    }

    #[test]
    fn walking_over_a_dropped_item_picks_it_up() {
        let (mut game, _) = game_looking_at_ground();
        let stack = {
            let items = game.items.as_ref().unwrap();
            let id = items.id_of("cubara:cobble").unwrap();
            items.new_stack(id, 7).unwrap()
        };
        // Right where the player is standing.
        let at = game.sim.player.pos;
        game.sim.entities.spawn_item(stack, at, glam::Vec3::ZERO);

        game.advance(TICK_DT);

        assert_eq!(game.sim.entities.len(), 0, "collected");
        assert_eq!(count_of(&game, "cubara:cobble"), 7);
    }

    #[test]
    fn a_dormant_chunk_ends_where_a_continuously_ticked_one_would() {
        // **The phase 2 exit gate's dormant test** (§11.3), and the reason
        // block 2.4c insisted `Furnace::advance` take an elapsed count.
        //
        // Run the same furnace two ways for the same number of ticks: once with
        // the player standing next to it the whole time, and once with the
        // player far away so the chunk sleeps and is caught up on return.
        for total in [50u64, 199, 200, 201, 450] {
            let continuous = {
                let (mut game, _) = game_looking_at_ground();
                let pos = open_a_furnace(&mut game);
                load_furnace(&mut game, pos);
                for _ in 0..total {
                    game.advance(TICK_DT);
                }
                game.world().furnace_at(pos).copied().expect("still there")
            };

            let slept = {
                let (mut game, _) = game_looking_at_ground();
                let pos = open_a_furnace(&mut game);
                load_furnace(&mut game, pos);
                // Exactly `total` ticks here too, or the comparison is against
                // a different amount of elapsed time rather than against
                // dormancy: one nearby, the middle away, one back home.
                let home = game.sim.player.pos;
                game.advance(TICK_DT);
                game.sim.player.pos = home + glam::Vec3::new(4000.0, 0.0, 0.0);
                for _ in 0..total - 2 {
                    game.advance(TICK_DT);
                }
                // Come back: the chunk wakes and catches up.
                game.sim.player.pos = home;
                game.advance(TICK_DT);
                game.world().furnace_at(pos).copied().expect("still there")
            };

            assert_eq!(
                continuous.output, slept.output,
                "output diverged after {total} ticks"
            );
            assert_eq!(
                continuous.input, slept.input,
                "input diverged after {total} ticks"
            );
        }
    }

    /// A furnace with enough raw iron and fuel to run for a long while.
    fn load_furnace(game: &mut Game, pos: [i32; 3]) {
        let raw = item(game, "cubara:raw_iron");
        let log = item(game, "cubara:oak_log");
        let f = Arc::make_mut(&mut game.world).furnace_at_mut(pos).unwrap();
        f.input = Some((raw, 8));
        f.fuel = Some((log, 32));
    }

    #[test]
    fn a_chunk_the_player_leaves_goes_dormant() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        load_furnace(&mut game, pos);
        game.advance(TICK_DT);

        let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
        assert_eq!(
            game.world().chunk_states().get(coord),
            cubara_world::ChunkState::Active,
            "active while the player is here"
        );

        game.sim.player.pos += glam::Vec3::new(4000.0, 0.0, 0.0);
        game.advance(TICK_DT);

        assert!(
            matches!(
                game.world().chunk_states().get(coord),
                cubara_world::ChunkState::Dormant { .. }
            ),
            "dormant once they leave"
        );
    }

    #[test]
    fn a_furnace_in_a_dormant_chunk_does_not_tick() {
        // Not "does nothing" -- deferred. The catch-up test above is the other
        // half, and together they are what makes dormancy invisible.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        load_furnace(&mut game, pos);
        game.advance(TICK_DT);
        let after_one = game.world().furnace_at(pos).copied().unwrap();

        game.sim.player.pos += glam::Vec3::new(4000.0, 0.0, 0.0);
        for _ in 0..500 {
            game.advance(TICK_DT);
        }

        let now = game.world().furnace_at(pos).copied().unwrap();
        assert_eq!(
            now.progress, after_one.progress,
            "a dormant furnace did not advance"
        );
    }

    #[test]
    fn falling_onto_the_ground_actually_hurts() {
        // Through the real physics and the real tick loop, not the damage
        // formula in isolation: the formula is unit-tested in `cubara-sim`, and
        // what this asserts is that a fall *reaches* it.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        // Ten blocks: past the 3-block safe distance, but **survivable**.
        // A longer drop would deal more than full health, and death restores
        // it -- so a lethal fall reads as "no damage" here. That is what the
        // first version of this test measured, and it is why the lethal case
        // has its own test below, asserting the respawn instead.
        game.sim.player = Player::new(
            glam::vec3(0.5, ground.block[1] as f32 + 11.0, 0.5),
            0.0,
            0.0,
        );
        let full = game.sim.player.health;

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.sim.player.on_ground {
                break;
            }
        }

        assert!(game.sim.player.on_ground, "it landed");
        assert!(
            game.sim.player.health < full,
            "landing from ten blocks left {} of {full} health",
            game.sim.player.health
        );
    }

    #[test]
    fn a_lethal_fall_returns_you_to_spawn_with_your_things() {
        // The owner's decision (§13.4): death costs position, not progress.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        let spawn = glam::vec3(0.5, ground.block[1] as f32 + 3.0, 0.5);
        game.sim.player = Player::new(spawn, 0.0, 0.0);
        // Give them something to lose, then drop them from lethal height.
        hold(&mut game, "cubara:iron_pick");
        let carried = game.sim.player.inventory;
        game.sim.player.pos = glam::vec3(0.5, ground.block[1] as f32 + 60.0, 0.5);

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.sim.player.pos.y <= spawn.y + 0.001 && game.sim.player.on_ground {
                break;
            }
        }

        assert_eq!(
            game.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "respawned at full health"
        );
        assert_eq!(game.sim.player.inventory, carried, "and kept the pick");
        // **Position too.** Without this the test passed while respawn did not
        // actually move anyone: `physics::step` wrote `player.pos` from its own
        // local box *after* the damage was applied, silently undoing the
        // respawn. Health and inventory alone could not see that.
        assert!(
            game.sim.player.pos.distance(spawn) < 2.5,
            "respawned at {} rather than near spawn {spawn}",
            game.sim.player.pos
        );
    }

    #[test]
    fn walking_off_a_low_step_does_not_hurt() {
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        game.sim.player = Player::new(glam::vec3(0.5, ground.block[1] as f32 + 2.5, 0.5), 0.0, 0.0);
        let full = game.sim.player.health;

        for _ in 0..300 {
            game.advance(TICK_DT);
        }

        assert_eq!(game.sim.player.health, full, "a short drop is free");
    }

    #[test]
    fn free_fly_never_hurts_however_far_you_descend() {
        // It is a debug mode; dropping out of the sky in it must not kill you
        // (§13.3). The fall distance is cleared every tick it is active.
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        game.sim.player = Player::new(
            glam::vec3(0.5, ground.block[1] as f32 + 80.0, 0.5),
            0.0,
            -1.5,
        );
        // Toggle free-fly on, then descend through the whole drop.
        game.fly_toggle_pending = true;
        game.down = true;
        for _ in 0..600 {
            game.advance(TICK_DT);
        }
        let health_in_flight = game.sim.player.health;
        assert_eq!(
            health_in_flight,
            cubara_sim::MAX_HEALTH,
            "free-fly descent cost health"
        );
    }

    #[test]
    fn the_game_does_not_start_by_killing_the_player() {
        // **The test that was missing.** Every other test in this file
        // repositions the player just above the ground before doing anything,
        // so none of them started the game the way the app does -- and the app
        // started it 32 blocks above the terrain, which block 2.9a turned into
        // 29 damage against 20 health. The player died on the first landing,
        // respawned at the same mid-air point, and died again, forever.
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );

        for _ in 0..600 {
            game.advance(TICK_DT);
        }

        assert!(
            game.sim.player.on_ground,
            "the player never settled on the ground"
        );
        assert_eq!(
            game.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "starting the game cost {} health",
            cubara_sim::MAX_HEALTH - game.sim.player.health
        );
    }

    #[test]
    fn the_spawn_point_is_somewhere_survivable() {
        // The other half: respawning must not drop the player into a lethal
        // fall, or death becomes a loop rather than a setback.
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );

        // Kill them outright, then let the world run.
        game.sim.player.take_damage(cubara_sim::MAX_HEALTH);
        for _ in 0..600 {
            game.advance(TICK_DT);
        }

        assert_eq!(
            game.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "respawning cost health, so death loops"
        );
        assert!(game.sim.player.on_ground, "and it landed");
    }

    #[test]
    fn there_is_solid_stone_however_far_down_you_go() {
        // The world has no floor. Generation never had `y` bounds -- what was
        // missing was streaming and simulating anywhere but chunk layers 0..=2.
        let (game, _) = game_looking_at_ground();
        let terrain = game.terrain.expect("assets are set");
        let registry = game.blocks_registry.as_ref().unwrap();
        for y in [-1, -100, -5_000, -100_000] {
            let block = game.world().block_at(0, y, 0, terrain);
            assert_eq!(
                registry.name_of(block),
                Some("cubara:stone"),
                "expected stone at y = {y}"
            );
            assert!(game.world().is_solid_at(0, y, 0, terrain), "solid at {y}");
        }
    }

    #[test]
    fn there_is_open_sky_however_far_up_you_go() {
        let (game, _) = game_looking_at_ground();
        let terrain = game.terrain.expect("assets are set");
        for y in [100, 5_000, 100_000] {
            assert!(
                !game.world().is_solid_at(0, y, 0, terrain),
                "expected air at y = {y}"
            );
        }
    }

    #[test]
    fn a_block_can_be_placed_and_broken_far_below_the_old_world_floor() {
        // y = 0 used to be the bottom of the world. Editing below it has to
        // persist like any other edit -- the overlay is keyed by world
        // position and never had a floor either.
        let (mut game, _) = game_looking_at_ground();
        let terrain = game.terrain.expect("assets are set");
        let deep = [3, -2_000, 7];

        let cc = Arc::make_mut(&mut game.world).set_block(deep[0], deep[1], deep[2], BlockId::AIR);
        assert_eq!(cc, ChunkCoord::from_block(deep[0], deep[1], deep[2]));
        assert!(
            !game.world().is_solid_at(deep[0], deep[1], deep[2], terrain),
            "the deep block was mined out"
        );

        // And its neighbours are still stone, so the edit is local.
        assert!(game
            .world()
            .is_solid_at(deep[0] + 1, deep[1], deep[2], terrain));
    }

    #[test]
    fn a_chunk_far_below_the_old_floor_generates_and_meshes() {
        // Generating at depth must produce a real chunk, not an empty or
        // panicking one -- `ChunkCoord` is i32 and `region_of` uses div_euclid,
        // both of which were already correct for negative coordinates.
        let (game, _) = game_looking_at_ground();
        let terrain = game.terrain.expect("assets are set");
        let deep = ChunkCoord::new(0, -64, 0);
        let chunk = game
            .world()
            .chunk_at(deep, terrain)
            .expect("a chunk that deep still generates");
        let registry = game.blocks_registry.as_ref().unwrap();
        assert_eq!(
            registry.name_of(chunk.get(0, 0, 0)),
            Some("cubara:stone"),
            "a chunk a thousand blocks down is solid rock"
        );
    }

    #[test]
    fn the_simulation_follows_the_player_downward() {
        // A furnace a long way below the old world floor must tick when the
        // player is next to it -- the simulated band moves with them now.
        let (mut game, _) = game_looking_at_ground();
        let deep = [0, -1_000, 0];
        Arc::make_mut(&mut game.world).add_furnace(deep);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        {
            let f = Arc::make_mut(&mut game.world).furnace_at_mut(deep).unwrap();
            f.input = Some((raw, 2));
            f.fuel = Some((log, 4));
        }
        // Stand next to it.
        game.sim.player.pos = glam::vec3(0.5, -1_000.0, 0.5);
        game.sim.player.spawn = game.sim.player.pos;

        for _ in 0..250 {
            game.advance(TICK_DT);
        }

        let f = game.world().furnace_at(deep).expect("still there");
        assert!(
            f.progress > 0 || f.output.is_some(),
            "a furnace at y = -1000 never ticked"
        );
    }

    /// A `Game` with assets wired, saving into a scratch directory rather than
    /// the real world folder.
    fn game_with_assets() -> Game {
        let mut game = Game::new();
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        game.set_assets(
            std::sync::Arc::new(cubara_render::load_registry()),
            items,
            recipes,
        );
        game
    }

    #[test]
    fn a_world_survives_being_closed_and_reopened() {
        // **#179's test, and the one that was missing.** The save *format* was
        // tested from the start; nothing tested that a player action reaches
        // it. `save_world`/`load_world` were never called from this crate at
        // all, so the world was not still there after you closed it -- which is
        // the one sentence ROADMAP.md uses to describe phase 1's result.
        let dir = std::env::temp_dir().join(format!(
            "cubara-app-roundtrip-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mined;
        let carried;
        {
            let mut game = game_with_assets();
            // Do something a player would: take a block, and move.
            let ground = game
                .world()
                .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
                .expect("ground");
            mined = ground.block;
            game.sim.player.pos = glam::vec3(0.5, mined[1] as f32 + 3.5, 0.5);
            game.break_at(mined);
            carried = game.sim.player.inventory;

            game.save_to(&dir);
        }

        let mut reopened = game_with_assets();
        assert!(reopened.load_from(&dir), "the save did not load");

        assert!(
            !reopened
                .world()
                .is_solid_at(mined[0], mined[1], mined[2], reopened.terrain()),
            "the mined block came back"
        );
        assert_eq!(
            reopened.sim.player.inventory, carried,
            "the inventory did not survive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_save_is_a_normal_first_run() {
        let mut game = game_with_assets();
        let empty = std::env::temp_dir().join("cubara-no-such-world-12345");
        let _ = std::fs::remove_dir_all(&empty);
        assert!(
            !game.load_from(&empty),
            "reported a load that did not happen"
        );
    }
}
