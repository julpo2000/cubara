//! The world's random number generator.
//!
//! `WorldRng` is explicit state, not a global, unseeded generator — the
//! whole point is that the same seed produces the same sequence forever, on
//! every platform, so a replay (block 1.8) and later a save file reproduce
//! *exactly* the same random outcomes. A fixed, well-known algorithm
//! (PCG-XSH-RR, O'Neill's PCG32), written in-crate rather than pulled in as
//! a dependency, so there is nothing here whose behaviour could change out
//! from under a save file on a version bump.

/// PCG32's fixed multiplier (Knuth's MMIX LCG constant).
const MULTIPLIER: u64 = 6364136223846793005;

/// A small, fixed-algorithm PRNG — [`WorldRng::next_u32`] is PCG-XSH-RR
/// ("PCG32"). Two `u64`s of state, deterministic across platforms (pure
/// integer arithmetic throughout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldRng {
    /// `pub(crate)`, not private: `crate::hash::WorldHash` reads this raw
    /// state directly to include "the RNG" in the world-state hash
    /// (issue #90) -- there is deliberately no other accessor.
    pub(crate) state: u64,
    /// Odd by construction (`(stream << 1) | 1`) — PCG's stream selector,
    /// each odd value producing a distinct, statistically independent
    /// sequence from the same `seed`.
    pub(crate) inc: u64,
}

impl WorldRng {
    /// A generator seeded from `seed`, on stream `stream` (pick any constant
    /// per independent use -- e.g. one stream for worldgen decisions, another
    /// for a future loot table -- so two systems drawing from the "same"
    /// seed don't correlate).
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut rng = Self {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    /// The next pseudo-random `u32`, advancing the generator's state.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        // XSH-RR: xorshift the high bits down, then a variable rotate seeded
        // by the top bits -- this is what breaks the LCG's well-known weak
        // low-order-bit periodicity.
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// A pseudo-random `f32` in `[0, 1)`. Uses the top 24 bits of
    /// [`next_u32`](Self::next_u32) so the conversion to `f32` (24 bits of
    /// mantissa) is exact, not an approximation of a wider value.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_and_stream_reproduce_the_same_sequence() {
        let mut a = WorldRng::new(42, 0);
        let mut b = WorldRng::new(42, 0);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = WorldRng::new(1, 0);
        let mut b = WorldRng::new(2, 0);
        assert_ne!(a.next_u32(), b.next_u32());
    }

    #[test]
    fn different_streams_diverge_from_the_same_seed() {
        let mut a = WorldRng::new(7, 0);
        let mut b = WorldRng::new(7, 1);
        assert_ne!(a.next_u32(), b.next_u32());
    }

    #[test]
    fn known_seed_produces_a_known_sequence() {
        // Pins the algorithm itself, not just "it's deterministic" -- catches
        // an accidental change to the constants/shift amounts that
        // `same_seed_and_stream_reproduce_the_same_sequence` alone would not
        // (that test would still pass against a *different* but still
        // self-consistent algorithm). These are this implementation's own
        // output for seed 42 / stream 54, computed once and pinned here --
        // not cross-checked against an external PCG32 implementation, since
        // this crate deliberately has no RNG dependency to check against.
        let mut rng = WorldRng::new(42, 54);
        assert_eq!(rng.next_u32(), 0xa15c02b7);
        assert_eq!(rng.next_u32(), 0x7b47f409);
        assert_eq!(rng.next_u32(), 0xba1d3330);
        assert_eq!(rng.next_u32(), 0x83d2f293);
    }

    #[test]
    fn f32_output_stays_in_unit_range() {
        let mut rng = WorldRng::new(9, 0);
        for _ in 0..1000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v), "{v} out of [0,1)");
        }
    }
}
