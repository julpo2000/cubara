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
use cubara_sim::{InputFrame, Player, REACH, TICK_DT};
use cubara_sim::{SlotRef, HOTBAR_WIDTH};
use cubara_voxel::{BlockRegistry, ChunkCoord, ItemRegistry, RecipeBook};
use cubara_world::{Furnace, TerrainBlocks, World};

use cubara_server::{Action, Effect, FurnaceSlot, Screen, Server};

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
/// The asset loaders and the world directory, re-exported from the crate that
/// now owns them.
///
/// They moved to `cubara-server` because the *server* decides what a block
/// means (`RESEARCH_MULTIPLAYER.md` §3.4) and because a dedicated server has to
/// be able to load them with no window. The client still needs the names — it
/// draws items and builds the same registries — so it says where they went
/// rather than keeping a second copy.
pub use cubara_server::assets::{
    load_item_registry, load_ore_registry, load_recipe_book, load_structure_registry, world_dir,
};

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

pub struct Game {
    /// The authoritative half (`docs/RESEARCH_MULTIPLAYER.md` §8): the world,
    /// the simulation and the registries.
    ///
    /// `Game` is now the **client**, plus the wiring that runs a server
    /// in-process for singleplayer (§3.3). Everything left on `Game` itself is
    /// input, screen state or presentation — the §8.1 table is the sorting.
    pub server: Server,
    /// The client's own world (`RESEARCH_MULTIPLAYER.md` §8.2).
    ///
    /// **A replica, not a cache, and not the server's.** The instinct is to
    /// share one `World` in singleplayer; that defeats the exercise, because the
    /// seam only tells you something if the client cannot reach into the
    /// server's state, and an in-process shortcut is exactly what will not exist
    /// over a socket.
    ///
    /// Affordable only because terrain is a pure function of the seed (§3.4), so
    /// this copy is **generated, never received**. What crosses is the edit
    /// overlay and the block entities, which is already how a `World` is built
    /// and already what the save format persists.
    ///
    /// It may be wrong, briefly, and nothing may treat it as authority.
    world: Arc<World>,
    /// The player's pose as of the *previous* completed tick -- together with
    /// `sim.player` (the current tick), what [`Game::camera_pose`] interpolates
    /// between for smooth rendering of a 60 Hz sim at any frame rate (§9).
    prev_player: Player,
    /// The block registry, shared with `NodeStreaming` rather than loaded
    /// twice -- ids are per-registry (`PHASE2_ARCHITECTURE.md` §1.2), so two
    /// loads would be two id spaces and the same number would mean different
    /// materials on each side. `None` until `resumed` builds it.
    /// Which ids the terrain's grass/soil/stone are, in that registry.
    /// What items exist. Loaded by the app, not by `cubara-render`: items are
    /// not a render concern (Rule 3).
    /// Every recipe, loaded alongside the items they name.
    /// Whether the inventory screen is open. Screen state, not world state --
    /// what the *grid* holds is world state and lives on the player.
    inventory_open: bool,
    /// The chunk the simulation radius was last updated around. `None` until
    /// the first tick, so it always runs once.
    /// The furnace whose screen is open, by world position. `None` when the
    /// open screen is the plain inventory or a bench.
    open_furnace: Option<[i32; 3]>,
    /// Every smelting recipe, loaded alongside the items they name.
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
        let server = Server::new();
        let player = server.sim.player;
        Self {
            // Same seed, so the same terrain -- generated here rather than
            // copied from the server, which is the whole point of §8.2. Over a
            // socket the seed arrives in the join handshake; in-process it is
            // read off the world that already exists.
            world: Arc::new(World::with_seed(server.world.seed())),
            server,
            prev_player: player,
            inventory_open: false,
            open_furnace: None,
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

    /// The client's replica (§8.2) -- what the renderer meshes and what the
    /// crosshair raycasts against. Never the server's world.
    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    /// The camera pose to render from: the sim's player pose, interpolated
    /// between its previous and current tick by the accumulator's leftover
    /// fraction. Render-side only (§9) -- never read back into the sim.
    pub fn camera_pose(&self) -> CameraPose {
        let alpha = (self.accumulator / TICK_DT as f64).clamp(0.0, 1.0) as f32;
        let player = self.prev_player.lerp(&self.server.sim.player, alpha);
        CameraPose {
            // The renderer works in floats, and that is the correct side of the
            // seam for it: a wrong last bit in a camera matrix is a sub-pixel
            // difference (§3.5, presentation may be float).
            eye: glam::Vec3::from_array(player.pos.to_f32()),
            look_dir: player.look_dir(),
        }
    }

    /// The block the player is currently looking at, within [`REACH`], for
    /// the renderer to outline -- computed by the sim's own raycast each
    /// tick (`cubara_sim::Sim::tick`), not here. The renderer draws it; it
    /// does not decide it (`ARCHITECTURE.md` Rule 3, issue #52).
    pub fn selected_block(&self) -> Option<[i32; 3]> {
        self.server.sim.target
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
        while self.accumulator >= TICK_DT as f64 {
            self.prev_player = self.server.sim.player;
            self.server.tick_sim(&input);
            // Mining advances *per tick*, not per frame -- §4.3, and the same
            // reason the tick loop exists. A catch-up burst of N ticks is N
            // ticks of progress, which is correct: that time really did pass.
            //
            // Between the server's two halves, which is where it has always
            // run: moving it would reorder the tick, and tick order is Rule 1.
            self.tick_mining(input.breaking);
            self.server.tick_world();
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
        // Once, after the whole burst: the replica catches up with everything
        // the server did, and reports what needs re-meshing. A catch-up of five
        // ticks that broke the same chunk five times is one re-mesh.
        self.sync()
    }

    /// Write the world to disk (#179). The server owns the world, so it owns
    /// the save; this is the client's shutdown reaching for it.
    pub fn save(&self) {
        self.server.save_to(&world_dir());
    }

    /// Replace this game's world with the one on disk, if there is one (#179).
    pub fn load(&mut self) -> bool {
        self.load_from(&world_dir())
    }

    /// [`load`](Self::load) from a specific directory.
    pub fn load_from(&mut self, dir: &std::path::Path) -> bool {
        let loaded = self.server.load_from(dir);
        if loaded {
            // A load replaces the server's world wholesale, so the replica is
            // rebuilt rather than patched: there is no edit stream that turns
            // one world into another. Over a socket this is a fresh join.
            self.world = Arc::new(World::with_seed(self.server.world.seed()));
            self.resync();
            // A load replaces the player wholesale, so the previous pose the
            // client interpolates from is now a pose from a different world.
            self.prev_player = self.server.sim.player;
        }
        loaded
    }

    /// The player's health, reduced to what the renderer draws
    /// (`PHASE2_ARCHITECTURE.md` §13.1).
    ///
    /// Both numbers, so `cubara-render` never learns what full health is --
    /// it is told the points and the maximum and works out the hearts (Rule 3).
    pub fn health_view(&self) -> cubara_render::HealthView {
        cubara_render::HealthView {
            points: self.server.sim.player.health,
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
    /// against `self.server.world` after an edit lands (e.g. two edits between
    /// ticks); issue #52 scoped out any change to raycasting itself anyway.
    /// Give the game the assets it needs to turn blocks into items and back.
    /// Called once, when the window and its registry exist.
    pub fn set_assets(
        &mut self,
        registry: Arc<BlockRegistry>,
        items: ItemRegistry,
        recipes: RecipeBook,
    ) {
        self.server.set_assets(registry, items, recipes);
        // The server just moved the player onto the ground; the client's
        // interpolation would otherwise smear them there from y = 48.
        self.prev_player = self.server.sim.player;
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
    /// Break the block the player is looking at, returning the chunk whose
    /// geometry is now stale.
    ///
    /// Reached only through [`apply`](Self::apply) -- the raycast is the
    /// server's, which is what stops a client naming its own target (§8.3).
    pub fn break_block(&mut self) -> Option<ChunkCoord> {
        self.server.apply(Action::Break);
        self.sync().into_iter().next()
    }

    /// One tick of mining (`PHASE2_ARCHITECTURE.md` §4.3). Returns the chunk to
    /// re-mesh on the tick the block finally gives way.
    ///
    /// **Progress is abandoned, not banked.** It is dropped when the button is
    /// released, when the player looks at a different block, or when the held
    /// tool changes -- each of those makes the stored `Mining` stop matching,
    /// and a non-match restarts from zero rather than resuming.
    fn tick_mining(&mut self, breaking: bool) {
        if !breaking {
            self.mining = None;
            return;
        }
        // Predicted on the *replica* (§8.1: "client predicts, server decides").
        // How far along a break is is display -- the break itself is an edit,
        // and that goes through the server below.
        let origin = self.server.sim.player.pos.to_f32();
        let dir = self.server.sim.player.look_dir().to_array();
        let Some(hit) = self.world.raycast(origin, dir, REACH, self.terrain()) else {
            // Looking at nothing in reach: whatever was in progress is gone.
            self.mining = None;
            return;
        };
        let (Some(registry), Some(terrain)) =
            (self.server.blocks_registry.as_deref(), self.server.terrain)
        else {
            return;
        };
        let target = self
            .world
            .block_at(hit.block[0], hit.block[1], hit.block[2], terrain);

        // Absent hardness means unbreakable -- no progress accrues and no
        // amount of holding the button changes that.
        let Some(hardness) = registry.hardness(target) else {
            return;
        };

        let held = self
            .server
            .sim
            .player
            .inventory
            .selected_stack()
            .map(|s| s.item());
        let speed = match (held, self.server.items.as_ref()) {
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
            return;
        }
        self.mining = None;
        // The break is an `Action`, so the *server* raycasts to decide what was
        // hit -- the client's own hit above chose when to ask, not what to
        // destroy (§8.3).
        self.server.apply(Action::Break);
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
        let registry = self.server.blocks_registry.as_deref()?;
        let terrain = self.server.terrain?;
        let target = self
            .world
            .block_at(m.block[0], m.block[1], m.block[2], terrain);
        let hardness = registry.hardness(target)?;
        if hardness == 0 {
            return Some(1.0);
        }
        Some((m.progress as f32 / hardness as f32).clamp(0.0, 1.0))
    }

    /// Place the held block, or use an interactive block under the crosshair.
    pub fn place_block(&mut self) -> Option<ChunkCoord> {
        self.server.apply(Action::Place);
        self.sync().into_iter().next()
    }

    /// Use whatever the player is looking at, reporting whether a screen
    /// opened. The server decides *what* was used (§8.3).
    ///
    /// Nothing on the game's own path calls this: right-click goes through
    /// [`Action::Place`], which tries interacting first so a bench stays usable
    /// while holding a block. This is the direct entry point the interaction
    /// tests drive, and the shape a second input binding would use.
    #[allow(dead_code)]
    fn interact(&mut self) -> bool {
        self.server.apply(Action::Interact);
        self.sync();
        self.open_furnace.is_some() || self.inventory_open
    }

    /// Break a specific block, bypassing the raycast.
    ///
    /// The direct entry point the drop and tier tests drive, rather than
    /// holding a button for eight ticks to assert one drop. Play goes through
    /// [`Action::Break`], where the server chooses the target -- including
    /// [`tick_mining`](Self::tick_mining), which since §8.3 asks for a break
    /// rather than naming one, so nothing on the game's own path reaches here.
    #[allow(dead_code)]
    fn break_at(&mut self, block: [i32; 3]) -> ChunkCoord {
        let cc = self.server.break_at(block);
        // The server does not journal a screen-close for a targeted break --
        // `CloseIfAt` is `Action::Break`'s doing, and this bypasses it.
        self.server.close_if_at(block);
        self.sync();
        cc
    }

    /// Take everything the server has done and apply it to this client:
    /// world edits onto the replica, block entities onto the replica, screens
    /// onto the screen state. Returns the chunks whose geometry is now stale.
    ///
    /// **The dirty chunks are derived, not received.** Each edit is applied to
    /// the client's own `World`, and the [`ChunkCoord`] its own `set_block`
    /// hands back is what needs re-meshing. A remote client would have to work
    /// it out exactly this way, because the server has no idea how its chunks
    /// are laid out on screen.
    fn sync(&mut self) -> Vec<ChunkCoord> {
        let effects = self.server.drain_effects();
        self.apply_effects(effects)
    }

    /// Rebuild the replica from the server's full state (§8.3's join
    /// handshake), rather than from a delta there is no way to compute.
    fn resync(&mut self) {
        let snapshot = self.server.snapshot();
        self.apply_effects(snapshot);
    }

    /// The one place effects are applied, whether they arrived as a stream or
    /// as a snapshot.
    fn apply_effects(&mut self, effects: Vec<Effect>) -> Vec<ChunkCoord> {
        let mut dirty = Vec::new();
        for e in effects {
            match e {
                Effect::Edit { pos, block } => {
                    let cc =
                        Arc::make_mut(&mut self.world).set_block(pos[0], pos[1], pos[2], block);
                    if !dirty.contains(&cc) {
                        dirty.push(cc);
                    }
                }
                Effect::BlockEntity { pos, furnace } => {
                    let world = Arc::make_mut(&mut self.world);
                    match furnace {
                        Some(f) => world.put_furnace(pos, f),
                        None => {
                            world.remove_block_entity(pos);
                        }
                    }
                }
                Effect::Open(Screen::Bench) => {
                    self.open_furnace = None;
                    self.inventory_open = true;
                }
                Effect::Open(Screen::Furnace(pos)) => {
                    self.open_furnace = Some(pos);
                    self.inventory_open = true;
                }
                Effect::CloseIfAt(pos) => {
                    if self.open_furnace == Some(pos) {
                        self.open_furnace = None;
                        self.inventory_open = false;
                    }
                }
            }
        }
        dirty
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
        let items = self.server.items.as_ref()?;
        let inv = &self.server.sim.player.inventory;
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

    /// The furnace screen currently open, if any -- read from the **replica**.
    ///
    /// Which is the interesting part: a furnace smelting away updates the screen
    /// because the server journals a `BlockEntity` effect every tick it changes
    /// and the client applies it, not because the client is looking at the
    /// server's furnace. That is the same path a remote client would use.
    pub fn open_furnace(&self) -> Option<Furnace> {
        let pos = self.open_furnace?;
        self.world.furnace_at(pos).copied()
    }

    /// Which ids the terrain is made of — delegated to the server, which owns
    /// the registries (§8.1).
    fn terrain(&self) -> TerrainBlocks {
        self.server.terrain()
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
            if let Some(items) = self.server.items.as_ref() {
                let player = &mut self.server.sim.player;
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
        let Some(items) = self.server.items.as_ref() else {
            self.inventory_open = false;
            return;
        };
        let player = &mut self.server.sim.player;
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
        let (Some(items), Some(book)) = (self.server.items.as_ref(), self.server.recipes.as_ref())
        else {
            return;
        };
        let panel = match self.open_furnace {
            Some(_) => InventoryPanel::layout_furnace(width, height),
            None => InventoryPanel::layout(width, height, self.server.sim.player.crafting.width()),
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
        let player = &mut self.server.sim.player;
        player
            .crafting
            .click(slot, right, &mut player.inventory, items, book);
    }

    /// Route a click on the open furnace's screen.
    ///
    /// The inventory half is the player's own and stays here; the three furnace
    /// slots are world state, so they go to the server as an
    /// [`Action::ClickFurnace`] and come back as a `BlockEntity` effect. That
    /// translation -- `PanelSlotKind` (where it is drawn) to [`FurnaceSlot`]
    /// (what it is) -- is the client's job, because a server that spoke in panel
    /// layouts would be a server that knew what a screen looks like.
    fn click_furnace(&mut self, pos: [i32; 3], kind: PanelSlotKind, index: usize) {
        let Some(items) = self.server.items.as_ref() else {
            return;
        };
        if kind == PanelSlotKind::Inventory {
            let player = &mut self.server.sim.player;
            player
                .crafting
                .click_inventory_only(index, &mut player.inventory, items);
            return;
        }
        let slot = match kind {
            PanelSlotKind::Grid => FurnaceSlot::Input,
            PanelSlotKind::Fuel => FurnaceSlot::Fuel,
            PanelSlotKind::Result => FurnaceSlot::Output,
            PanelSlotKind::Inventory => return,
        };
        self.server.apply(Action::ClickFurnace { pos, slot });
        // The furnace's new contents come back as a `BlockEntity` effect, and
        // the screen is drawn from the replica -- so without this the click
        // would appear to do nothing until the next tick.
        self.sync();
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
        let items = self.server.items.as_ref()?;
        let crafting = &self.server.sim.player.crafting;
        let book = self.server.recipes.as_ref();
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
                (PanelSlotKind::Inventory, _) => self
                    .server
                    .sim
                    .player
                    .inventory
                    .slot(s.index)
                    .and_then(swatch),
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
        self.server.sim.player.inventory.selected_slot()
    }

    /// Select a hotbar slot (number keys 1-9, passed as 0-8).
    pub fn select_hotbar(&mut self, index: u8) {
        self.server.sim.player.inventory.select(index);
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
    use cubara_voxel::FixedVec3;
    use cubara_voxel::{BlockId, ItemStack, ItemState};

    #[test]
    fn breaking_clears_the_targeted_block() {
        // No GPU involved — this is why gameplay does not belong on the renderer.
        let mut game = Game::new();
        // Look straight down from above the terrain.
        game.server.sim.player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 60.0, 0.5]),
            0.0,
            -1.5,
        );
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
        let eye = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.5, 0.5]);
        game.server.sim.player = Player::new(eye, 0.0, -1.5);

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
        let eye = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.5, 0.5]);
        game.server.sim.player = Player::new(eye, 0.0, -1.5);
        (game, ground.block)
    }

    #[test]
    fn breaking_a_block_puts_its_item_in_the_inventory() {
        let (mut game, block) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");
        let broken = game.world().block_at(block[0], block[1], block[2], terrain);
        let name = game
            .server
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(broken)
            .expect("the block has a name")
            .to_string();

        game.break_block().expect("a block was in reach");

        let items = game.server.items.as_ref().unwrap();
        let stack = game
            .server
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
            .server
            .blocks_registry
            .as_ref()
            .unwrap()
            .name_of(game.server.terrain.unwrap().stone)
            .expect("terrain stone has a name")
            .to_string();
        let items = game.server.items.as_ref().unwrap();
        let held = items
            .id_of(&stone_name)
            .expect("every shipped block needs an item of the same name to be placeable");
        let stack = items.new_stack(held, 5).unwrap();
        game.server.sim.player.inventory.add(stack, items);
        game.select_hotbar(0);

        game.place_block().expect("a face was in reach");

        assert_eq!(
            game.server.sim.player.inventory.slot(0).map(|s| s.count()),
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
        let items = game.server.items.as_ref().unwrap();
        let stick = items
            .id_of("cubara:stick")
            .expect("assets/items has a stick");
        game.server
            .sim
            .player
            .inventory
            .add(items.new_stack(stick, 3).unwrap(), items);
        game.select_hotbar(0);

        let before = game.world().edit_count();
        assert_eq!(game.place_block(), None);
        assert_eq!(game.world().edit_count(), before, "nothing was placed");
        assert_eq!(
            game.server.sim.player.inventory.slot(0).map(|s| s.count()),
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
        let items = game.server.items.as_ref().unwrap();
        let filler = items.id_of("cubara:stick").unwrap();
        for _ in 0..cubara_sim::SLOT_COUNT {
            game.server
                .sim
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
        assert_eq!(game.server.sim.player.inventory.selected_slot(), 3);

        // Releasing must not reselect -- otherwise every key-up would snap the
        // selection back to whichever number was let go of last.
        game.key_input(KeyCode::Digit1, true);
        game.key_input(KeyCode::Digit4, false);
        assert_eq!(game.server.sim.player.inventory.selected_slot(), 0);
    }

    /// Place a bench right where the player is looking, and aim at it.
    fn game_facing_a_bench() -> (Game, [i32; 3]) {
        let (mut game, ground) = game_looking_at_ground();
        let bench = game
            .server
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:crafting_bench")
            .expect("assets/blocks defines the bench");
        game.server.set_block(ground, bench);
        game.sync();
        (game, ground)
    }

    #[test]
    fn right_clicking_a_bench_opens_the_three_by_three_grid() {
        let (mut game, _) = game_facing_a_bench();
        let items = game.server.items.as_ref().unwrap();
        // Holding something placeable, to prove interaction wins over placing.
        game.server.sim.player.inventory.add(
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
        assert_eq!(game.server.sim.player.crafting.width(), 3, "at bench size");
    }

    #[test]
    fn right_clicking_anything_else_still_places() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.server.items.as_ref().unwrap();
        game.server.sim.player.inventory.add(
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
        let items = game.server.items.as_ref().unwrap();
        let book = game.server.recipes.as_ref().unwrap();
        let mut scratch = cubara_sim::Inventory::new();
        scratch.add(items.new_stack(item, count).unwrap(), items);
        let mut c = game.server.sim.player.crafting;
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
        game.server.sim.player.crafting = c;
    }

    #[test]
    fn closing_a_bench_returns_its_outer_cells_and_narrows() {
        // Cell 8 is the bottom-right of a 3x3 -- unreachable at width 2.
        // Narrowing must not strand it. `Crafting::close` empties all nine
        // cells regardless of width deliberately, and this is the test that
        // says why that mattered.
        let (mut game, _) = game_facing_a_bench();
        game.place_block();
        assert_eq!(game.server.sim.player.crafting.width(), 3);

        let stone = game
            .server
            .items
            .as_ref()
            .unwrap()
            .id_of("cubara:stone")
            .unwrap();
        load_cell(&mut game, 8, stone, 4);
        assert!(
            game.server.sim.player.crafting.cell(8).is_some(),
            "cell 8 is loaded"
        );

        game.toggle_inventory();
        assert!(!game.inventory_open(), "it closed");
        assert_eq!(game.server.sim.player.crafting.width(), 2, "and narrowed");
        assert!(
            game.server.sim.player.crafting.cell(8).is_none(),
            "the outer cell was emptied, not stranded"
        );
        assert!(
            game.server
                .sim
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
            let items = game.server.items.as_ref().unwrap();
            (
                items.id_of("cubara:plank").unwrap(),
                items.id_of("cubara:stick").unwrap(),
            )
        };
        // PPP / .S. / .S.
        for (cell, item) in [(0, plank), (1, plank), (2, plank), (4, stick), (7, stick)] {
            load_cell(&mut game, cell, item, 1);
        }

        let items = game.server.items.as_ref().unwrap();
        let made = game
            .server
            .sim
            .player
            .crafting
            .result(game.server.recipes.as_ref().unwrap(), items)
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
            !game.server.sim.player.is_free_fly(),
            "walking is the default mode"
        );
        game.key_input(KeyCode::F4, true);
        game.advance(TICK_DT);
        assert!(game.server.sim.player.is_free_fly());
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
        assert_eq!(game.server.sim.tick, 2);
        assert!(
            game.server.sim.player.is_free_fly(),
            "one press should flip the mode once (false -> true), not twice (-> false)"
        );
    }

    #[test]
    fn advance_by_exactly_one_tick_worth_of_time_runs_one_tick() {
        let mut game = Game::new();
        assert_eq!(game.server.sim.tick, 0);
        game.advance(TICK_DT);
        assert_eq!(game.server.sim.tick, 1);
    }

    #[test]
    fn sub_tick_dt_does_not_run_a_tick_yet() {
        let mut game = Game::new();
        game.advance(TICK_DT * 0.5);
        assert_eq!(
            game.server.sim.tick, 0,
            "half a tick's worth of time isn't a tick"
        );
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.server.sim.tick, 1, "the other half completes it");
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
            spread.server.sim.tick, 0,
            "5 * 0.1 tick < one tick: nothing simulated"
        );
        spread.advance(TICK_DT); // now cross the threshold -> exactly one tick
        assert_eq!(spread.server.sim.tick, 1);

        // The same 500 px of motion delivered in a single tick-sized frame.
        let mut once = Game::new();
        once.mouse_look(500.0, 0.0);
        once.advance(TICK_DT);
        assert_eq!(once.server.sim.tick, 1);

        assert_eq!(
            spread.server.sim.player.look_dir(),
            once.server.sim.player.look_dir(),
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

        assert_eq!(single.server.sim.tick, 1);
        assert_eq!(burst.server.sim.tick, 3);
        assert_eq!(
            single.server.sim.player.look_dir(),
            burst.server.sim.player.look_dir(),
            "the same single mouse-look delta must turn the player by the same \
             amount regardless of how many ticks ran in the same `advance` call"
        );
    }

    #[test]
    fn a_huge_dt_is_capped_not_caught_up_in_one_frame() {
        let mut game = Game::new();
        game.advance(1000.0 * TICK_DT); // a 1000-tick backlog in one call
        assert_eq!(
            game.server.sim.tick, MAX_TICKS_PER_FRAME as u64,
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

        assert_eq!(steady.server.sim.tick, jittery.server.sim.tick);
        assert_eq!(steady.server.sim.player, jittery.server.sim.player);
    }

    /// Put `block` directly in front of the player and aim at it, so a test can
    /// choose which material it breaks rather than taking whatever terrain is
    /// underfoot. Returns the position it was placed at.
    fn stand_over(game: &mut Game, block_name: &str) -> [i32; 3] {
        let id = game
            .server
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
        game.server.set_block(b, id);
        // Drain it, so the setup's own edit is not mistaken for the first thing
        // the test does. A client only ever learns about a world through these.
        game.sync();
        b
    }

    /// Put the furnace at `pos` into a known state **through the server**, so
    /// the client's replica is told about it.
    ///
    /// Reaching into `game.server.world` directly is the shortcut a real client
    /// does not have (§8.2), so a test does not get it either -- a test that
    /// took it would be setting up a world the client never sees, and would
    /// then fail for a reason unrelated to what it asserts.
    fn edit_furnace(game: &mut Game, pos: [i32; 3], f: impl FnOnce(&mut cubara_world::Furnace)) {
        let mut furnace = game
            .server
            .world
            .furnace_at(pos)
            .copied()
            .unwrap_or_default();
        f(&mut furnace);
        game.server.set_furnace(pos, furnace);
        game.sync();
    }

    /// Give the player `item` in the selected hotbar slot.
    fn hold(game: &mut Game, item: &str) {
        let items = game.server.items.as_ref().unwrap();
        let id = items
            .id_of(item)
            .unwrap_or_else(|| panic!("no item {item}"));
        let stack = items.new_stack(id, 1).expect("a stack of one");
        let slot = game.server.sim.player.inventory.selected_slot() as usize;
        game.server.sim.player.inventory.set_slot(slot, Some(stack));
    }

    fn count_of(game: &Game, item: &str) -> u8 {
        let items = game.server.items.as_ref().unwrap();
        let Some(id) = items.id_of(item) else {
            return 0;
        };
        (0..cubara_sim::SLOT_COUNT)
            .filter_map(|i| game.server.sim.player.inventory.slot(i))
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
        let before = match game
            .server
            .sim
            .player
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block().expect("a block was in reach");

        let after = match game
            .server
            .sim
            .player
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
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
        let before = match game
            .server
            .sim
            .player
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("a pick should carry durability, got {other:?}"),
        };

        game.break_block().expect("a block was in reach");

        let after = match game
            .server
            .sim
            .player
            .inventory
            .selected_stack()
            .unwrap()
            .state()
        {
            ItemState::Durability { remaining } => remaining,
            other => panic!("still a pick, got {other:?}"),
        };
        assert_eq!(after, before, "the wasted swing cost nothing");
    }

    #[test]
    fn a_tool_at_zero_durability_leaves_the_slot() {
        let (mut game, _) = game_looking_at_ground();
        let items = game.server.items.as_ref().unwrap();
        let pick = items.id_of("cubara:stone_pick").unwrap();
        let nearly_dead = ItemStack::new(
            pick,
            1,
            ItemState::Durability { remaining: 1 },
            items.max_stack(pick),
        )
        .expect("a worn pick");
        let slot = game.server.sim.player.inventory.selected_slot() as usize;
        game.server
            .sim
            .player
            .inventory
            .set_slot(slot, Some(nearly_dead));
        stand_over(&mut game, "cubara:stone");

        game.break_block().expect("a block was in reach");

        assert!(
            game.server.sim.player.inventory.slot(slot).is_none(),
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
        game.server.add_furnace(pos);
        game.sync();
        game.open_furnace = Some(pos);
        game.inventory_open = true;
        pos
    }

    fn item(game: &Game, name: &str) -> cubara_voxel::ItemId {
        game.server.items.as_ref().unwrap().id_of(name).expect(name)
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
        assert_eq!(game.server.sim.player.crafting.width(), 3);
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
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 1));
            f.fuel = Some((log, 4));
        });

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
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 1)));

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
        let stack = game
            .server
            .items
            .as_ref()
            .unwrap()
            .new_stack(raw, 3)
            .unwrap();
        game.server.sim.player.crafting.set_held(Some(stack));

        // Into the input slot.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        assert_eq!(
            game.open_furnace().unwrap().input,
            Some((raw, 3)),
            "the held stack went in"
        );
        assert!(
            game.server.sim.player.crafting.held().is_none(),
            "hand is empty"
        );

        // And back out.
        game.click_furnace(pos, PanelSlotKind::Grid, 0);
        assert_eq!(game.open_furnace().unwrap().input, None);
        assert_eq!(
            game.server.sim.player.crafting.held().map(|s| s.count()),
            Some(3)
        );
    }

    #[test]
    fn the_output_slot_is_take_only() {
        // Putting something back into the output would let the next completed
        // smelt stack onto it, which is work out of nothing.
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let stack = game
            .server
            .items
            .as_ref()
            .unwrap()
            .new_stack(raw, 1)
            .unwrap();
        game.server.sim.player.crafting.set_held(Some(stack));

        game.click_furnace(pos, PanelSlotKind::Result, 0);

        assert_eq!(game.open_furnace().unwrap().output, None, "nothing went in");
        assert!(
            game.server.sim.player.crafting.held().is_some(),
            "still held"
        );
    }

    #[test]
    fn breaking_a_furnace_takes_its_state_with_it() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 5)));

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
            .server
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:furnace")
            .unwrap();
        let items = game.server.items.as_ref().unwrap();
        let id = items.id_of("cubara:furnace").unwrap();
        let stack = items.new_stack(id, 1).unwrap();
        let slot = game.server.sim.player.inventory.selected_slot() as usize;
        game.server.sim.player.inventory.set_slot(slot, Some(stack));

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
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 2)));

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
            let full = game
                .server
                .items
                .as_ref()
                .unwrap()
                .new_stack(raw, 64)
                .unwrap();
            game.server.sim.player.inventory.set_slot(i, Some(full));
        }

        game.break_at(b);

        assert_eq!(
            game.server.sim.entities.len(),
            1,
            "the drop is on the floor"
        );
        let (_, d) = game.server.sim.entities.sorted()[0];
        assert_eq!(
            game.server.items.as_ref().unwrap().name_of(d.stack.item()),
            Some("cubara:soil")
        );
    }

    #[test]
    fn a_broken_furnace_spills_its_contents() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 4));
            f.fuel = Some((log, 2));
        });

        game.break_at(pos);

        // Three would-be-lost stacks: input, fuel, and the furnace's own drop
        // goes to the inventory, so two entities plus whatever did not fit.
        assert!(
            game.server.sim.entities.len() >= 2,
            "input and fuel are on the floor, got {}",
            game.server.sim.entities.len()
        );
    }

    #[test]
    fn walking_over_a_dropped_item_picks_it_up() {
        let (mut game, _) = game_looking_at_ground();
        let stack = {
            let items = game.server.items.as_ref().unwrap();
            let id = items.id_of("cubara:cobble").unwrap();
            items.new_stack(id, 7).unwrap()
        };
        // Right where the player is standing.
        let at = game.server.sim.player.pos;
        game.server
            .sim
            .entities
            .spawn_item(stack, at, FixedVec3::ZERO);

        game.advance(TICK_DT);

        assert_eq!(game.server.sim.entities.len(), 0, "collected");
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
                let home = game.server.sim.player.pos;
                game.advance(TICK_DT);
                game.server.sim.player.pos = home + FixedVec3::from_f32([4000.0, 0.0, 0.0]);
                for _ in 0..total - 2 {
                    game.advance(TICK_DT);
                }
                // Come back: the chunk wakes and catches up.
                game.server.sim.player.pos = home;
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
        edit_furnace(game, pos, |f| {
            f.input = Some((raw, 8));
            f.fuel = Some((log, 32));
        });
    }

    #[test]
    fn a_chunk_the_player_leaves_goes_dormant() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        load_furnace(&mut game, pos);
        game.advance(TICK_DT);

        let coord = ChunkCoord::from_block(pos[0], pos[1], pos[2]);
        // Read from the *server*: which chunks simulate is authority (§8.1),
        // and the client's replica has no lifecycle at all -- it is told about
        // edits, not about what is ticking.
        assert_eq!(
            game.server.world.chunk_states().get(coord),
            cubara_world::ChunkState::Active,
            "active while the player is here"
        );

        game.server.sim.player.pos += FixedVec3::from_f32([4000.0, 0.0, 0.0]);
        game.advance(TICK_DT);

        assert!(
            matches!(
                game.server.world.chunk_states().get(coord),
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

        game.server.sim.player.pos += FixedVec3::from_f32([4000.0, 0.0, 0.0]);
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
        game.server.sim.player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 11.0, 0.5]),
            0.0,
            0.0,
        );
        let full = game.server.sim.player.health;

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.server.sim.player.on_ground {
                break;
            }
        }

        assert!(game.server.sim.player.on_ground, "it landed");
        assert!(
            game.server.sim.player.health < full,
            "landing from ten blocks left {} of {full} health",
            game.server.sim.player.health
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
        let spawn = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.0, 0.5]);
        game.server.sim.player = Player::new(spawn, 0.0, 0.0);
        // Give them something to lose, then drop them from lethal height.
        hold(&mut game, "cubara:iron_pick");
        let carried = game.server.sim.player.inventory;
        game.server.sim.player.pos =
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 60.0, 0.5]);

        for _ in 0..600 {
            game.advance(TICK_DT);
            if game.server.sim.player.pos.y <= spawn.y && game.server.sim.player.on_ground {
                break;
            }
        }

        assert_eq!(
            game.server.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "respawned at full health"
        );
        assert_eq!(
            game.server.sim.player.inventory, carried,
            "and kept the pick"
        );
        // **Position too.** Without this the test passed while respawn did not
        // actually move anyone: `physics::step` wrote `player.pos` from its own
        // local box *after* the damage was applied, silently undoing the
        // respawn. Health and inventory alone could not see that.
        assert!(
            game.server.sim.player.pos.distance_squared(spawn)
                < (5 * cubara_voxel::fixed::ONE as i128 / 2).pow(2),
            "respawned at {:?} rather than near spawn {spawn:?}",
            game.server.sim.player.pos
        );
    }

    #[test]
    fn walking_off_a_low_step_does_not_hurt() {
        let (mut game, _) = game_looking_at_ground();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, game.terrain())
            .expect("ground below");
        game.server.sim.player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 2.5, 0.5]),
            0.0,
            0.0,
        );
        let full = game.server.sim.player.health;

        for _ in 0..300 {
            game.advance(TICK_DT);
        }

        assert_eq!(game.server.sim.player.health, full, "a short drop is free");
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
        game.server.sim.player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 80.0, 0.5]),
            0.0,
            -1.5,
        );
        // Toggle free-fly on, then descend through the whole drop.
        game.fly_toggle_pending = true;
        game.down = true;
        for _ in 0..600 {
            game.advance(TICK_DT);
        }
        let health_in_flight = game.server.sim.player.health;
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
            game.server.sim.player.on_ground,
            "the player never settled on the ground"
        );
        assert_eq!(
            game.server.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "starting the game cost {} health",
            cubara_sim::MAX_HEALTH - game.server.sim.player.health
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
        game.server.sim.player.take_damage(cubara_sim::MAX_HEALTH);
        for _ in 0..600 {
            game.advance(TICK_DT);
        }

        assert_eq!(
            game.server.sim.player.health,
            cubara_sim::MAX_HEALTH,
            "respawning cost health, so death loops"
        );
        assert!(game.server.sim.player.on_ground, "and it landed");
    }

    #[test]
    fn there_is_solid_stone_however_far_down_you_go() {
        // The world has no floor. Generation never had `y` bounds -- what was
        // missing was streaming and simulating anywhere but chunk layers 0..=2.
        let (game, _) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");
        let registry = game.server.blocks_registry.as_ref().unwrap();
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
        let terrain = game.server.terrain.expect("assets are set");
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
        let terrain = game.server.terrain.expect("assets are set");
        let deep = [3, -2_000, 7];

        let cc = game.server.set_block(deep, BlockId::AIR);
        game.sync();
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
        let terrain = game.server.terrain.expect("assets are set");
        let deep = ChunkCoord::new(0, -64, 0);
        let chunk = game
            .world()
            .chunk_at(deep, terrain)
            .expect("a chunk that deep still generates");
        let registry = game.server.blocks_registry.as_ref().unwrap();
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
        game.server.add_furnace(deep);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, deep, |f| {
            f.input = Some((raw, 2));
            f.fuel = Some((log, 4));
        });
        // Stand next to it.
        game.server.sim.player.pos = cubara_voxel::FixedVec3::from_f32([0.5, -1_000.0, 0.5]);
        game.server.sim.player.spawn = game.server.sim.player.pos;

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
            game.server.sim.player.pos =
                cubara_voxel::FixedVec3::from_f32([0.5, mined[1] as f32 + 3.5, 0.5]);
            game.break_at(mined);
            carried = game.server.sim.player.inventory;

            game.server.save_to(&dir);
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
            reopened.server.sim.player.inventory, carried,
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

    // ── The client's replica world (RESEARCH_MULTIPLAYER §8.2) ──────────────

    /// The claim the whole section rests on: **terrain is generated, never
    /// sent.** Two worlds built from one seed agree everywhere, and nothing
    /// crossed the seam to make that true.
    ///
    /// If this ever stops holding, the replica stops being affordable and the
    /// design changes -- so it is worth a test of its own rather than being
    /// implied by the ones below.
    #[test]
    fn the_replica_generates_the_same_terrain_as_the_server_with_nothing_sent() {
        let (game, _) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");
        assert_eq!(
            game.world().seed(),
            game.server.world.seed(),
            "same seed, which is all the client is given"
        );
        for y in -40..80 {
            assert_eq!(
                game.world().is_solid_at(3, y, 7, terrain),
                game.server.world.is_solid_at(3, y, 7, terrain),
                "the two worlds disagree about (3, {y}, 7)"
            );
        }
    }

    /// They are genuinely two worlds, not one behind an accessor.
    ///
    /// An edit written straight into the server's world -- bypassing the
    /// journal, which is the one thing production code may never do -- must
    /// **not** appear on the client. That is what proves there is no in-process
    /// shortcut left: if this test fails, `Game::world()` is the server's world
    /// again and the seam is decorative.
    #[test]
    fn an_edit_that_skips_the_journal_never_reaches_the_client() {
        let (mut game, _) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");
        let at = [11, 30, 11];
        let stone = game
            .server
            .blocks_registry
            .as_ref()
            .unwrap()
            .id_of("cubara:stone")
            .unwrap();

        Arc::make_mut(&mut game.server.world).set_block(at[0], at[1], at[2], stone);
        game.sync();

        assert!(
            game.server.world.is_solid_at(at[0], at[1], at[2], terrain),
            "the server has it"
        );
        assert!(
            !game.world().is_solid_at(at[0], at[1], at[2], terrain),
            "and the client was never told, because nothing told it"
        );
    }

    /// The ordinary path: an edit made through the server reaches the replica,
    /// and the chunk the client must re-mesh is the one it worked out itself.
    #[test]
    fn an_edit_reaches_the_replica_and_names_its_own_dirty_chunk() {
        let (mut game, ground) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");

        game.server.set_block(ground, BlockId::AIR);
        let dirty = game.sync();

        assert!(
            !game
                .world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the replica applied the edit"
        );
        assert_eq!(
            dirty,
            vec![ChunkCoord::from_block(ground[0], ground[1], ground[2])],
            "and derived the stale chunk from its own world, not from the server"
        );
    }

    /// A catch-up burst that edits one chunk repeatedly is one re-mesh, not
    /// five. `sync` runs once after the whole burst, which is where that
    /// falls out.
    #[test]
    fn a_burst_of_edits_in_one_chunk_is_one_dirty_chunk() {
        let (mut game, ground) = game_looking_at_ground();
        for dy in 0..4 {
            game.server
                .set_block([ground[0], ground[1] - dy, ground[2]], BlockId::AIR);
        }
        assert_eq!(game.sync().len(), 1, "four edits, one chunk, one re-mesh");
    }

    /// The furnace screen is drawn from the **replica**, so a furnace smelting
    /// away has to be replicated every tick it changes -- otherwise the panel
    /// would freeze the moment it opened.
    ///
    /// This is the block-entity half of §8.3 doing real work rather than being
    /// a message type nobody sends.
    #[test]
    fn a_smelting_furnace_updates_the_clients_screen_through_block_entity_effects() {
        let (mut game, _) = game_looking_at_ground();
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        let log = item(&game, "cubara:oak_log");
        edit_furnace(&mut game, pos, |f| {
            f.input = Some((raw, 1));
            f.fuel = Some((log, 4));
        });

        let before = game.open_furnace().expect("open");
        for _ in 0..210 {
            game.advance(TICK_DT);
        }
        let after = game.open_furnace().expect("still open");

        assert_ne!(
            before, after,
            "the client's copy of the furnace moved with the server's"
        );
        assert_eq!(
            after,
            game.server.world.furnace_at(pos).copied().unwrap(),
            "and matches it exactly"
        );
    }

    /// The join handshake (§8.3): a replica with nothing in it is brought up to
    /// date from a snapshot, not from a delta -- because there is no delta from
    /// a world it has never seen.
    ///
    /// Driven the way a load does it, since that is the one thing today that
    /// replaces a world wholesale.
    #[test]
    fn a_snapshot_rebuilds_a_replica_that_has_seen_nothing() {
        let (mut game, ground) = game_looking_at_ground();
        let terrain = game.server.terrain.expect("assets are set");
        let pos = open_a_furnace(&mut game);
        let raw = item(&game, "cubara:raw_iron");
        edit_furnace(&mut game, pos, |f| f.input = Some((raw, 2)));
        game.server.set_block(ground, BlockId::AIR);
        game.sync();

        // Throw the replica away, exactly as a load does, and rejoin.
        game.world = Arc::new(World::with_seed(game.server.world.seed()));
        assert!(
            game.world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the fresh replica has the untouched terrain"
        );
        game.resync();

        assert!(
            !game
                .world()
                .is_solid_at(ground[0], ground[1], ground[2], terrain),
            "the snapshot carried the edit"
        );
        assert_eq!(
            game.world().furnace_at(pos).copied(),
            game.server.world.furnace_at(pos).copied(),
            "and the block entity"
        );
    }

    /// Playing must keep the two worlds in step. Mine a block by holding the
    /// button, the way the game does, and the replica ends up agreeing with the
    /// server about every edit in the chunk.
    #[test]
    fn mining_through_the_action_path_keeps_the_two_worlds_in_step() {
        let (mut game, _) = game_looking_at_ground();
        stand_over(&mut game, "cubara:stone");
        hold(&mut game, "cubara:stone_pick");
        mine_for(&mut game, 20).expect("it broke");

        let server_edits: Vec<_> = game.server.world.edits().collect();
        let client_edits: Vec<_> = game.world().edits().collect();
        assert_eq!(
            server_edits, client_edits,
            "the replica saw every edit the server made, and no others"
        );
    }
}
