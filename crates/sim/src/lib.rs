//! The deterministic simulation (`docs/PHASE1_ARCHITECTURE.md` §9).
//!
//! [`Sim`] is the one thing allowed to change gameplay state. It advances by
//! tick number, never by elapsed wall-clock time -- `ARCHITECTURE.md`'s
//! determinism rule. `cubara-app` owns a fixed-timestep accumulator and
//! calls [`Sim::tick`] a whole number of times per frame; this crate never
//! reads the system clock and never reaches for a global, unseeded random
//! source (`scripts/check-architecture.sh` greps for both). GPU-free and
//! window-free, so all of it is testable with no adapter and no window.

mod input;
mod player;
mod rng;

pub use input::InputFrame;
pub use player::Player;
pub use rng::WorldRng;

use cubara_world::World;

/// One fixed simulation step, in seconds. 60 Hz.
pub const TICK_DT: f32 = 1.0 / 60.0;

/// Everything the player *is* and *does*, plus the world's own randomness --
/// the whole of what phase 1 considers "the game state" outside `World`
/// itself. `tick`/`player` are `pub`: read freely (rendering interpolates
/// against `player`, a save file will want `tick`), but the only way to
/// *change* any of this is [`Sim::tick`].
pub struct Sim {
    /// How many fixed steps have run since this `Sim` was created.
    pub tick: u64,
    rng: WorldRng,
    pub player: Player,
}

impl Sim {
    pub fn new(seed: u64, player: Player) -> Self {
        Self {
            tick: 0,
            rng: WorldRng::new(seed, 0),
            player,
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
    /// seconds. Phase 1's scope: free-fly movement only (real player physics
    /// is block 1.7a) -- `world` is threaded through and part of the
    /// signature `docs/PHASE1_ARCHITECTURE.md` §9 specifies, even though
    /// this block doesn't yet have anything that reads or writes it (block
    /// 1.7a's collision is what will).
    pub fn tick(&mut self, _world: &mut World, input: &InputFrame) {
        self.player.apply_free_fly(input, TICK_DT);
        self.tick += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_input() -> InputFrame {
        InputFrame::default()
    }

    #[test]
    fn tick_counter_advances_by_exactly_one_per_call() {
        let mut world = World::new();
        let mut sim = Sim::new(0, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        for expected in 1..=100u64 {
            sim.tick(&mut world, &no_input());
            assert_eq!(sim.tick, expected);
        }
    }

    #[test]
    fn roll_draws_from_the_seeded_stream_not_a_global() {
        let mut world = World::new();
        let mut a = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let mut b = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        a.tick(&mut world, &no_input());
        b.tick(&mut world, &no_input());
        assert_eq!(a.roll(), b.roll(), "same seed, same tick, same roll");
    }
}
