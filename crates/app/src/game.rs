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
use cubara_voxel::{BlockId, BlockRegistry, ItemRegistry, RecipeBook};
use cubara_world::TerrainBlocks;
use cubara_world::World;

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

/// Load `assets/recipes/*.ron`, resolving ingredient names through `items`.
pub fn load_recipe_book(items: &ItemRegistry) -> RecipeBook {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    RecipeBook::load(&repo_root.join("assets/recipes"), items).expect("assets/recipes must load")
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
    /// The crafting bench, resolved by name. Right-clicking one opens the 3x3
    /// grid instead of placing whatever is held.
    ///
    /// Name-based, like the drop policy (`PHASE2_ARCHITECTURE.md` 4.1), and
    /// for the same reason: block 2.4 needs the same treatment for the furnace,
    /// and designing an `interact:` field in the block format now would mean
    /// designing it without the second case in hand.
    bench_block: Option<BlockId>,
    /// Whether the inventory screen is open. Screen state, not world state --
    /// what the *grid* holds is world state and lives on the player.
    inventory_open: bool,
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
            bench_block: None,
            inventory_open: false,
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
    pub fn advance(&mut self, dt: f32) {
        self.accumulator += dt as f64;

        // Not a tick's worth of time yet: leave the accumulated one-shot inputs
        // alone so a later frame's tick can consume them, rather than sampling
        // and clearing them here where no tick would apply them. `move_axes` is
        // re-read from live held state on the frame that does tick, so nothing
        // is lost by returning early.
        if self.accumulator < TICK_DT as f64 {
            return;
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
        };
        // A tick below will consume these, so it's safe to clear them now.
        self.look_delta = (0.0, 0.0);
        self.jump_pending = false;
        self.fly_toggle_pending = false;

        let mut ticks = 0;
        while self.accumulator >= TICK_DT as f64 {
            self.prev_player = self.sim.player;
            self.sim.tick(Arc::make_mut(&mut self.world), &input);
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
        self.terrain = Some(TerrainBlocks::from_registry(&registry));
        self.blocks_registry = Some(registry);
        self.items = Some(items);
        self.recipes = Some(recipes);
        self.bench_block = self
            .blocks_registry
            .as_ref()
            .and_then(|r| r.id_of("cubara:crafting_bench"));
    }

    /// Break the targeted block and put its item in the inventory.
    ///
    /// The drop is one-for-one **by name**: the block `cubara:oak_log` yields
    /// the item `cubara:oak_log`. A block with no matching item yields nothing.
    /// That is the placeholder policy `PHASE2_ARCHITECTURE.md` §4.1 records --
    /// §4's `drops:` and `requires_tier:` fields are block 2.4's work, and
    /// doing it by name now avoids inventing a data format 2.4 will replace.
    ///
    /// **A drop that does not fit is lost.** There are no dropped-item entities
    /// yet (they need ECS, 2.5), so the remainder `Inventory::add` hands back is
    /// logged and discarded. Refusing to break the block instead would be a
    /// gameplay decision, and those are the owner's.
    pub fn break_block(&mut self) -> Option<ChunkCoord> {
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, REACH)?;
        let [x, y, z] = hit.block;

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
            match registry
                .name_of(broken)
                .and_then(|name| items.id_of(name))
                .and_then(|item| items.new_stack(item, 1).ok())
            {
                Some(stack) => {
                    if let Some(lost) = self.sim.player.inventory.add(stack, items) {
                        log::debug!(
                            "inventory full: {} x{} lost (no dropped-item entities until ECS, 2.5)",
                            items.name_of(lost.item()).unwrap_or("?"),
                            lost.count()
                        );
                    }
                }
                None => log::debug!(
                    "{} has no item of the same name; it drops nothing",
                    registry.name_of(broken).unwrap_or("?")
                ),
            }
        }

        Some(Arc::make_mut(&mut self.world).set_block(x, y, z, BlockId::AIR))
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
        let hit = self.world.raycast(origin, dir, REACH)?;
        let target = [
            hit.block[0] + hit.normal[0],
            hit.block[1] + hit.normal[1],
            hit.block[2] + hit.normal[2],
        ];

        // Only now that the placement is certain to happen.
        let slot = self.sim.player.inventory.selected_slot() as usize;
        self.sim.player.inventory.take_one(slot, items)?;

        Some(Arc::make_mut(&mut self.world).set_block(target[0], target[1], target[2], block))
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
    /// Only the bench so far. Block 2.4 adds the furnace and is the point at
    /// which this should become a property of the block rather than a name
    /// comparison -- with two real cases to design against.
    fn interact(&mut self) -> bool {
        let (Some(bench), Some(terrain)) = (self.bench_block, self.terrain) else {
            return false;
        };
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let Some(hit) = self.world.raycast(origin, dir, REACH) else {
            return false;
        };
        let [x, y, z] = hit.block;
        if self.world.block_at(x, y, z, terrain) != bench {
            return false;
        }
        // Width lives on `Crafting` (world state), not on the screen: a 3x3
        // grid holding items in its outer cells is a different world from a
        // 2x2 one, and the hash already covers it.
        self.sim.player.crafting.set_width(3);
        self.inventory_open = true;
        true
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
        let panel = InventoryPanel::layout(width, height, self.sim.player.crafting.width());
        let Some((kind, index)) = panel.hit(x, y) else {
            return;
        };
        let slot = match kind {
            PanelSlotKind::Inventory => SlotRef::Inventory(index),
            PanelSlotKind::Grid => SlotRef::Grid(index),
            PanelSlotKind::Result => SlotRef::Result,
        };
        let player = &mut self.sim.player;
        player
            .crafting
            .click(slot, right, &mut player.inventory, items, book);
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
        let panel = InventoryPanel::layout(width, height, crafting.width());

        let swatch = |stack: cubara_voxel::ItemStack| {
            items.name_of(stack.item()).map(|name| HotbarSlot {
                color: swatch_color(name),
                count: stack.count(),
            })
        };

        let contents = panel
            .slots()
            .iter()
            .map(|s| match s.kind {
                PanelSlotKind::Inventory => {
                    self.sim.player.inventory.slot(s.index).and_then(swatch)
                }
                PanelSlotKind::Grid => crafting.cell(s.index).and_then(swatch),
                PanelSlotKind::Result => book
                    .and_then(|b| crafting.result(b, items))
                    .and_then(swatch),
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
            .raycast([0.5, 60.0, 0.5], [0.0, -1.0, 0.0], 100.0)
            .expect("ground below");

        // Out of reach from 60 blocks up: nothing changes.
        assert_eq!(game.break_block(), None);
        assert!(game
            .world()
            .is_solid_at(hit.block[0], hit.block[1], hit.block[2]));
    }

    #[test]
    fn editing_within_reach_marks_a_chunk_dirty() {
        let mut game = Game::new();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
            .expect("ground below");
        // Stand just above the surface, looking down — now it is within reach.
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        game.sim.player = Player::new(eye, 0.0, -1.5);

        let dirty = game.break_block().expect("a block was in reach");
        assert!(
            !game
                .world()
                .is_solid_at(ground.block[0], ground.block[1], ground.block[2]),
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
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
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
            !game.world().is_solid_at(block[0], block[1], block[2]),
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
}
