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
pub use entity::{despawn_ticks, DroppedItem, Entities, EntityKey, PICKUP_RADIUS_SQ};
pub use hash::{hash_region, WorldHash};
pub use input::InputFrame;
pub use inventory::{Inventory, HOTBAR_WIDTH, SLOT_COUNT};
pub use player::{
    Player, PlayerId, FALL_DAMAGE_PER_BLOCK, HEART, MAX_HEALTH, REGEN_INTERVAL, SAFE_FALL,
    SENSITIVITY_PER_PIXEL,
};
pub use rng::WorldRng;
pub use save::{load_world, save_world, LoadError, SaveError, FORMAT_VERSION};

use std::collections::BTreeMap;

use cubara_world::{TerrainBlocks, World};

/// One fixed simulation step, in seconds. 60 Hz.
pub const TICK_DT: f32 = 1.0 / 60.0;

/// How far the player can target a block, in blocks -- shared by selection
/// (this block, #52) and editing (`cubara-app::Game::edit_block`), so the
/// highlighted block and the one an edit actually lands on can never drift
/// apart into two different reach distances.
pub const REACH: f32 = 6.0;

/// One tick's input, per player (block 2.10).
///
/// A map rather than a single frame because a server ticking several players
/// has several inputs, and rather than a `Vec` because iteration order is Rule
/// 1: which player steps first must be a property of the id, not of arrival
/// order over a socket.
///
/// A player with no entry this tick gets [`InputFrame::default`] -- which is
/// what a connected client that sent nothing means, and what an idle one means
/// too. Those are the same thing to the simulation, and treating them
/// differently would make the world depend on packet timing.
#[derive(Debug, Default, Clone)]
pub struct PlayerInputs(BTreeMap<PlayerId, InputFrame>);

impl PlayerInputs {
    /// The one-player case: singleplayer, and most tests.
    pub fn one(id: PlayerId, input: InputFrame) -> Self {
        let mut m = BTreeMap::new();
        m.insert(id, input);
        Self(m)
    }

    pub fn set(&mut self, id: PlayerId, input: InputFrame) {
        self.0.insert(id, input);
    }

    pub fn get(&self, id: PlayerId) -> InputFrame {
        self.0.get(&id).copied().unwrap_or_default()
    }
}

/// Everything the players *are* and *do*, plus the world's own randomness --
/// the whole of what phase 1 considers "the game state" outside `World` itself.
///
/// `tick` and `entities` are `pub`: read freely. Players are behind accessors
/// rather than a public field, because the map's invariant -- ids are assigned
/// by [`Sim::join`] and never reused -- is not something a caller should be able
/// to break by inserting into it (Rule 1).
#[derive(Debug)]
pub struct Sim {
    /// How many fixed steps have run since this `Sim` was created.
    pub tick: u64,
    rng: WorldRng,
    /// Everyone in this world, in `PlayerId` order.
    ///
    /// A `BTreeMap`, not a `HashMap` and not a `Vec`: ordered iteration is what
    /// lets the hash fold players in a fixed order and the tick step them in
    /// one, and `EntityKey` set the same precedent in `entity.rs` for the same
    /// reason.
    players: BTreeMap<PlayerId, Player>,
    /// The next id [`join`](Sim::join) will hand out. World state, so it
    /// survives a save and ids stay unique across a restart.
    next_player: u64,
    /// Everything on the floor (`PHASE2_ARCHITECTURE.md` §10). Ticked here,
    /// so dropped items fall and despawn on the same fixed clock the player
    /// walks on -- Rule 1.
    pub entities: Entities,
}

impl Sim {
    /// A world with one player, [`PlayerId::LOCAL`]. What singleplayer builds,
    /// and what every test that does not care about multiplayer wants.
    pub fn new(seed: u64, player: Player) -> Self {
        let mut sim = Self {
            tick: 0,
            rng: WorldRng::new(seed, 0),
            players: BTreeMap::new(),
            next_player: 0,
            entities: Entities::default(),
        };
        let id = sim.join(player);
        debug_assert_eq!(id, PlayerId::LOCAL);
        sim
    }

    /// Add a player, and hand back the id it will answer to forever.
    pub fn join(&mut self, player: Player) -> PlayerId {
        let id = PlayerId(self.next_player);
        self.next_player += 1;
        self.players.insert(id, player);
        id
    }

    /// Remove a player, returning what they were carrying so the caller can
    /// decide where it goes. Ids are never reused, so the slot does not come
    /// back.
    pub fn leave(&mut self, id: PlayerId) -> Option<Player> {
        self.players.remove(&id)
    }

    /// The next id that would be handed out. Save/load carries it so a restart
    /// cannot re-issue an id the world has already used.
    pub fn next_player_id(&self) -> u64 {
        self.next_player
    }

    /// One player, by id. Panics when the id is unknown, deliberately: ids come
    /// from [`join`](Sim::join) and disappear only at [`leave`](Sim::leave), so
    /// asking for one that is not there is a bug in the caller rather than a
    /// condition to handle. Use [`get`](Sim::get) where absence is expected.
    pub fn player(&self, id: PlayerId) -> &Player {
        self.get(id)
            .unwrap_or_else(|| panic!("no player {id:?} in this world"))
    }

    pub fn player_mut(&mut self, id: PlayerId) -> &mut Player {
        self.players
            .get_mut(&id)
            .unwrap_or_else(|| panic!("no player {id:?} in this world"))
    }

    pub fn get(&self, id: PlayerId) -> Option<&Player> {
        self.players.get(&id)
    }

    /// Every player, in `PlayerId` order -- the only iteration order anything
    /// outside this module may depend on.
    pub fn players(&self) -> impl Iterator<Item = (PlayerId, &Player)> + '_ {
        self.players.iter().map(|(id, p)| (*id, p))
    }

    pub fn player_ids(&self) -> Vec<PlayerId> {
        self.players.keys().copied().collect()
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
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
            .tick(items, |x, y, z| world.is_solid_at(x, y, z, blocks));
        // Pickup is per player, in id order (Rule 1): two players reaching the
        // same stack in a different order would empty the floor differently.
        for id in self.player_ids() {
            let p = self.players.get_mut(&id).expect("id came from the map");
            self.entities.collect_nearby(p.pos, &mut p.inventory, items);
        }
    }

    /// Advance the whole simulation by one fixed step of [`TICK_DT`] seconds.
    ///
    /// Every player steps, **in `PlayerId` order**, and only then does the tick
    /// counter move. Order matters even though the per-player step is mostly
    /// independent, because it will not stay independent -- and a tick whose
    /// result depends on which client's packet arrived first is not a tick this
    /// project is allowed to have (Rule 1).
    pub fn tick(&mut self, world: &mut World, inputs: &PlayerInputs, blocks: TerrainBlocks) {
        for id in self.player_ids() {
            let input = inputs.get(id);
            self.step_player(id, world, &input, blocks);
        }
        self.tick += 1;
    }

    /// One player's half of a tick. Private: the tick counter is the world's,
    /// not a player's, and letting a caller step one player without advancing
    /// the world would be a way to desynchronise them.
    fn step_player(
        &mut self,
        id: PlayerId,
        world: &mut World,
        input: &InputFrame,
        blocks: TerrainBlocks,
    ) {
        let player = self.players.get_mut(&id).expect("id came from the map");
        if input.toggle_fly {
            player.free_fly = !player.free_fly;
        }
        if player.free_fly {
            player.velocity = cubara_voxel::FixedVec3::ZERO;
            player.on_ground = false;
            // Free-fly is a debug mode and must never hurt: dropping out of it
            // should not kill you (§13.3), so the accumulated fall goes with it.
            player.fall_distance = cubara_voxel::Fixed::ZERO;
            player.apply_free_fly(input, TICK_DT);
        } else {
            physics::step(player, input, |x, y, z| world.is_solid_at(x, y, z, blocks));
        }
        // After physics, so damage taken this tick resets the counter before it
        // is incremented -- a tick you were hurt on is not a damage-free tick.
        player.tick_regeneration();
        player.target = world
            .raycast(
                // Raycasting takes floats, and its answer is a *block* -- the
                // conversion cannot move which block is hit by more than a
                // sub-unit. The ray direction is float regardless, since it
                // comes from `yaw`/`pitch` through trig; angles are the other
                // half of this migration (RESEARCH_MULTIPLAYER §3.5).
                player.pos.to_f32(),
                player.look_dir_f32().to_array(),
                REACH,
                blocks,
            )
            .map(|hit| hit.block);
    }
}

#[cfg(test)]
mod tests {
    /// The mouse motion these scripts used to be written in.
    ///
    /// `InputFrame::look_delta` is an `Angle` now (§3.5: nothing that crosses the
    /// wire is a float), and the pixels-to-angle conversion moved to the client.
    /// These scripts predate that and are written in pixels, so this is the same
    /// conversion the client does -- which is what keeps them meaning what they
    /// meant, rather than quietly turning 454 times as far.
    fn pixels(px: f32) -> Angle {
        Angle::from_raw((px * crate::SENSITIVITY_PER_PIXEL as f32) as i32)
    }

    use super::*;
    use cubara_voxel::Angle;

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

    fn no_input() -> PlayerInputs {
        PlayerInputs::default()
    }

    /// One frame, for the one player these tests have.
    fn only(input: InputFrame) -> PlayerInputs {
        PlayerInputs::one(PlayerId::LOCAL, input)
    }

    /// The player every test in this module drives.
    const P: PlayerId = PlayerId::LOCAL;

    #[test]
    fn tick_counter_advances_by_exactly_one_per_call() {
        let mut world = World::new();
        let mut sim = Sim::new(
            0,
            Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO),
        );
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
        let eye = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 3.5, 0.5]);
        let mut sim = Sim::new(
            0,
            Player::new(
                eye,
                Angle::ZERO,
                Angle::from_radians(-std::f32::consts::FRAC_PI_2),
            ),
        );
        sim.player_mut(P).free_fly = true; // hold position -- only the raycast matters here
        sim.tick(&mut world, &no_input(), blocks());
        assert_eq!(sim.player(P).target, Some(ground.block));
    }

    #[test]
    fn tick_leaves_target_none_when_nothing_is_in_reach() {
        let mut world = World::new();
        // High above the terrain, looking straight up into open sky: nothing
        // solid within REACH in either direction.
        let mut sim = Sim::new(
            0,
            Player::new(
                cubara_voxel::FixedVec3::from_f32([0.5, 500.0, 0.5]),
                Angle::ZERO,
                Angle::ZERO,
            ),
        );
        sim.player_mut(P).free_fly = true;
        sim.tick(&mut world, &no_input(), blocks());
        assert_eq!(sim.player(P).target, None);
    }

    #[test]
    fn roll_draws_from_the_seeded_stream_not_a_global() {
        let mut world = World::new();
        let mut a = Sim::new(
            1,
            Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO),
        );
        let mut b = Sim::new(
            1,
            Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO),
        );
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
        let spawn = cubara_voxel::FixedVec3::from_f32([0.5, ground.block[1] as f32 + 10.0, 0.5]);
        let mut sim = Sim::new(7, Player::new(spawn, Angle::ZERO, Angle::ZERO));
        let wander = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            look_delta: [pixels(2.0), Angle::ZERO],
            ..InputFrame::default()
        };

        for i in 0..10_000u64 {
            sim.tick(&mut world, &only(wander), blocks());
            assert!(
                !physics::player_intersects_solid(sim.player(P), &|x, y, z| world.is_solid_at(
                    x,
                    y,
                    z,
                    blocks()
                )),
                "tick {i}: player collision box intersects a solid voxel"
            );
            assert!(
                sim.player_mut(P).pos.y > cubara_voxel::Fixed::from_blocks(-1000),
                "tick {i}: fell through the world, y = {:?}",
                sim.player_mut(P).pos.y
            );
        }
    }
}
