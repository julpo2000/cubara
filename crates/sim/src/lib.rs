//! The deterministic simulation (`docs/PHASE1_ARCHITECTURE.md` §9).
//!
//! [`Sim`] is the one thing allowed to change gameplay state. It advances by
//! tick number, never by elapsed wall-clock time -- `ARCHITECTURE.md`'s
//! determinism rule. `cubara-app` owns a fixed-timestep accumulator and
//! calls [`Sim::tick`] a whole number of times per frame; this crate never
//! reads the system clock and never reaches for a global, unseeded random
//! source (`scripts/check-architecture.sh` greps for both). GPU-free and
//! window-free, so all of it is testable with no adapter and no window.

mod crafting;
pub mod entity;
mod hash;
mod input;
mod inventory;
mod physics;
mod player;
mod rng;
mod save;

pub use crafting::{Crafting, SlotRef};
pub use entity::{despawn_ticks, DroppedItem, Entities, EntityKey, PICKUP_RADIUS};
pub use hash::{hash_region, WorldHash};
pub use input::InputFrame;
pub use inventory::{Inventory, HOTBAR_WIDTH, SLOT_COUNT};
pub use player::{Player, FALL_DAMAGE_PER_BLOCK, HEART, MAX_HEALTH, REGEN_INTERVAL, SAFE_FALL};
pub use rng::WorldRng;
pub use save::{load_world, save_world, LoadError, SaveError, FORMAT_VERSION};

use cubara_world::{TerrainBlocks, World};

/// One fixed simulation step, in seconds. 60 Hz.
pub const TICK_DT: f32 = 1.0 / 60.0;

/// How far the player can target a block, in blocks -- shared by selection
/// (this block, #52) and editing (`cubara-app::Game::edit_block`), so the
/// highlighted block and the one an edit actually lands on can never drift
/// apart into two different reach distances.
pub const REACH: f32 = 6.0;

/// Everything the player *is* and *does*, plus the world's own randomness --
/// the whole of what phase 1 considers "the game state" outside `World`
/// itself. `tick`/`player`/`target` are `pub`: read freely (rendering
/// interpolates against `player` and draws the outline at `target`, a save
/// file will want `tick`), but the only way to *change* any of this is
/// [`Sim::tick`].
#[derive(Debug)]
pub struct Sim {
    /// How many fixed steps have run since this `Sim` was created.
    pub tick: u64,
    rng: WorldRng,
    pub player: Player,
    /// The block the player is currently looking at, within [`REACH`] --
    /// recomputed every tick from the player's own raycast. `None` when
    /// nothing solid is in reach. The renderer draws the outline; it does
    /// not decide what's selected (issue #52's Rule 3 boundary) -- this
    /// field is the seam.
    pub target: Option<[i32; 3]>,
    /// Everything on the floor (`PHASE2_ARCHITECTURE.md` §10). Ticked here,
    /// so dropped items fall and despawn on the same fixed clock the player
    /// walks on -- Rule 1.
    pub entities: Entities,
}

impl Sim {
    pub fn new(seed: u64, player: Player) -> Self {
        Self {
            tick: 0,
            rng: WorldRng::new(seed, 0),
            player,
            target: None,
            entities: Entities::default(),
        }
    }

    /// A pseudo-random `f32` in `[0, 1)` from the world's own RNG stream --
    /// exposed so a future tick-driven system (weather, mob spawns, phase 2)
    /// draws from the same explicit, seeded state everything else does,
    /// never a global, unseeded generator.
    pub fn roll(&mut self) -> f32 {
        self.rng.next_f32()
    }

    /// Advance the simulation by exactly one fixed step of [`TICK_DT`]
    /// seconds: a free-fly toggle edge (consumed here, exactly once, so a
    /// multi-tick catch-up burst can't flip it more than the real key press
    /// warrants), then either free-fly or real walking physics against
    /// `world`, depending on which mode the player is currently in
    /// (`docs/PHASE1_ARCHITECTURE.md` §10, issue #53), then re-targets from
    /// the resulting pose (issue #52).
    /// `blocks` is which ids the terrain is made of. The tick needs it because
    /// trees are solid (block 2.3a) and a tree is specific ids -- the density
    /// field alone can no longer answer "can I walk here".
    /// One tick of the entities, and the player picking up whatever is in
    /// reach (§10.4).
    ///
    /// Separate from [`tick`](Self::tick) only because it needs the item
    /// registry, which the sim does not otherwise carry -- the caller has it.
    /// It is still called once per fixed step, from the same loop.
    pub fn tick_entities(
        &mut self,
        world: &World,
        blocks: TerrainBlocks,
        items: &cubara_voxel::ItemRegistry,
    ) {
        if self.entities.is_empty() {
            return;
        }
        self.entities
            .tick(TICK_DT, items, |x, y, z| world.is_solid_at(x, y, z, blocks));
        self.entities
            .collect_nearby(self.player.pos, &mut self.player.inventory, items);
    }

    pub fn tick(&mut self, world: &mut World, input: &InputFrame, blocks: TerrainBlocks) {
        if input.toggle_fly {
            self.player.free_fly = !self.player.free_fly;
        }
        if self.player.free_fly {
            self.player.velocity = glam::Vec3::ZERO;
            self.player.on_ground = false;
            // Free-fly is a debug mode and must never hurt: dropping out of it
            // should not kill you (§13.3), so the accumulated fall goes with it.
            self.player.fall_distance = 0.0;
            self.player.apply_free_fly(input, TICK_DT);
        } else {
            physics::step(&mut self.player, input, TICK_DT, |x, y, z| {
                world.is_solid_at(x, y, z, blocks)
            });
        }
        // After physics, so damage taken this tick resets the counter before it
        // is incremented -- a tick you were hurt on is not a damage-free tick.
        self.player.tick_regeneration();
        self.target = world
            .raycast(
                self.player.pos.to_array(),
                self.player.look_dir().to_array(),
                REACH,
                blocks,
            )
            .map(|hit| hit.block);
        self.tick += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A treeless terrain palette: these tests are about the tick, not about
    /// what the world is made of.
    fn blocks() -> TerrainBlocks {
        TerrainBlocks {
            oak: None,
            ores: cubara_world::OreSet::EMPTY,
            grass: cubara_voxel::BlockId::STONE,
            soil: cubara_voxel::BlockId::STONE,
            stone: cubara_voxel::BlockId::STONE,
        }
    }

    fn no_input() -> InputFrame {
        InputFrame::default()
    }

    #[test]
    fn tick_counter_advances_by_exactly_one_per_call() {
        let mut world = World::new();
        let mut sim = Sim::new(0, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        for expected in 1..=100u64 {
            sim.tick(&mut world, &no_input(), blocks());
            assert_eq!(sim.tick, expected);
        }
    }

    #[test]
    fn tick_targets_the_block_the_player_is_looking_at() {
        let mut world = World::new();
        let ground = world
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, blocks())
            .expect("ground below");
        // Hover just above the surface, looking straight down at it.
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        let mut sim = Sim::new(0, Player::new(eye, 0.0, -std::f32::consts::FRAC_PI_2));
        sim.player.free_fly = true; // hold position -- only the raycast matters here
        sim.tick(&mut world, &no_input(), blocks());
        assert_eq!(sim.target, Some(ground.block));
    }

    #[test]
    fn tick_leaves_target_none_when_nothing_is_in_reach() {
        let mut world = World::new();
        // High above the terrain, looking straight up into open sky: nothing
        // solid within REACH in either direction.
        let mut sim = Sim::new(0, Player::new(glam::vec3(0.5, 500.0, 0.5), 0.0, 0.0));
        sim.player.free_fly = true;
        sim.tick(&mut world, &no_input(), blocks());
        assert_eq!(sim.target, None);
    }

    #[test]
    fn roll_draws_from_the_seeded_stream_not_a_global() {
        let mut world = World::new();
        let mut a = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let mut b = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        a.tick(&mut world, &no_input(), blocks());
        b.tick(&mut world, &no_input(), blocks());
        assert_eq!(a.roll(), b.roll(), "same seed, same tick, same roll");
    }

    /// Issue #53's own "Done when": walk real generated terrain -- hills,
    /// slopes, cave mouths, ledges, not a synthetic flat floor -- for 10,000
    /// ticks and never end a tick with the collision box inside a solid
    /// voxel, and never fall through the world. Walks forward while slowly
    /// turning, so the path curves across a wide, varied stretch of terrain
    /// rather than testing one straight line (and one obstacle) repeatedly.
    #[test]
    fn walking_uneven_terrain_for_10_000_ticks_never_intersects_solid_or_falls_through() {
        let mut world = World::new();
        let ground = world
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0, blocks())
            .expect("ground below");
        let spawn = glam::vec3(0.5, ground.block[1] as f32 + 10.0, 0.5);
        let mut sim = Sim::new(7, Player::new(spawn, 0.0, 0.0));
        let wander = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            look_delta: [2.0, 0.0],
            ..InputFrame::default()
        };

        for i in 0..10_000u64 {
            sim.tick(&mut world, &wander, blocks());
            assert!(
                !physics::player_intersects_solid(&sim.player, &|x, y, z| world.is_solid_at(
                    x,
                    y,
                    z,
                    blocks()
                )),
                "tick {i}: player collision box intersects a solid voxel"
            );
            assert!(
                sim.player.pos.y > -1000.0,
                "tick {i}: fell through the world, y = {}",
                sim.player.pos.y
            );
        }
    }
}
