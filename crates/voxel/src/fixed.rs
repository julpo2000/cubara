//! Fixed-point positions: integers that behave the same on every machine.
//!
//! # Why this exists
//!
//! Two problems, one answer (`docs/RESEARCH_MULTIPLAYER.md` §4).
//!
//! **Precision.** A position was `Vec3<f32>`. An `f32` spaces its representable
//! values proportionally to magnitude, so at `y = 1,000,000` the gap between
//! neighbouring values is about 0.06 blocks, and past `y ≈ 8,400,000` it cannot
//! represent consecutive integers at all. The world has no height limit; the
//! numbers describing positions in it did.
//!
//! **Determinism, which matters more.** Floating point is *the* classic source
//! of multiplayer desync: results depend on compiler version, optimisation
//! level, and instruction selection. Cubara is in unusually good shape here —
//! CI proves macOS and Windows agree on a full world-state hash — but that is a
//! property held by testing, and it has to hold forever, on compilers nobody has
//! written yet. Integer arithmetic holds it by construction.
//!
//! `ARCHITECTURE.md` Rule 1 is the keystone rule. This is that rule applied to
//! the one part of the simulation that was still floating point.
//!
//! # The representation
//!
//! An `i64` counting **1/65536ths of a block**. That is 16 fractional bits, so:
//!
//! - the fraction is a power of two, which makes conversion to and from `f32`
//!   exact in one direction and the shift below trivial;
//! - the range is about ±140,000,000,000,000 blocks, which is not a limit
//!   anybody will meet;
//! - **precision does not vary with distance.** The gap between neighbouring
//!   values at the origin and at a million blocks down is the same 1/65536.
//!   That is the whole point, and it is what `f32` could not do.
//!
//! # The rule this type exists to enforce
//!
//! §3.5: **authority is integer, presentation may be float.** Anything that
//! crosses the wire, or that a client reconciles against, is this type.
//! Converting to `f32` for rendering is fine and expected — a wrong last bit in
//! a camera matrix is a sub-pixel difference, not a divergence.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// Fractional bits. 16 means 1/65536 of a block.
pub const FRAC_BITS: u32 = 16;

/// One whole block, in sub-units.
pub const ONE: i64 = 1 << FRAC_BITS;

/// A scalar position or distance, in 1/65536ths of a block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fixed(i64);

impl Fixed {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(ONE);

    /// From whole blocks.
    pub const fn from_blocks(blocks: i32) -> Self {
        Self((blocks as i64) << FRAC_BITS)
    }

    /// From raw sub-units.
    pub const fn from_raw(raw: i64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> i64 {
        self.0
    }

    /// From an `f32`, rounding to nearest.
    ///
    /// Only for construction and for the boundary with code that has not moved
    /// yet — never inside the simulation, or the determinism this type exists
    /// for is lost at the conversion.
    pub fn from_f32(v: f32) -> Self {
        Self((v * ONE as f32).round() as i64)
    }

    /// To `f32`, for rendering and for maths that does not decide anything
    /// (§3.5: presentation may be float).
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / ONE as f32
    }

    /// The block this position is inside — **floor**, not truncation.
    ///
    /// An arithmetic shift right rounds toward negative infinity, which is what
    /// "which block am I in" means and what truncation gets wrong: at
    /// `y = -0.5`, truncating gives block 0 and flooring gives block -1. Block
    /// -1 is correct, and getting it wrong puts the player one block off in
    /// exactly the half of the world that did not exist before this week.
    pub const fn floor_block(self) -> i32 {
        (self.0 >> FRAC_BITS) as i32
    }

    /// The fractional part, always in `0..ONE` — never negative, matching
    /// [`floor_block`](Self::floor_block).
    pub const fn fract_raw(self) -> i64 {
        self.0 & (ONE - 1)
    }

    /// Divide by a whole number, rounding toward negative infinity so that the
    /// result is independent of sign — `-1 / 60` is `-1`, not `0`.
    ///
    /// Truncating division would make a falling body and a rising one round
    /// differently, which is a small asymmetry that accumulates.
    pub fn div_floor(self, by: i64) -> Self {
        Self(self.0.div_euclid(by))
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    pub fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    pub fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }
}

impl Add for Fixed {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Fixed {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl Neg for Fixed {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

/// Scaling by a whole number. Multiplying two [`Fixed`] values would need a
/// shift to stay in units, and nothing in the simulation does it — velocities
/// scale by tick counts, which are integers.
impl Mul<i64> for Fixed {
    type Output = Self;
    fn mul(self, rhs: i64) -> Self {
        Self(self.0 * rhs)
    }
}

impl Div<i64> for Fixed {
    type Output = Self;
    fn div(self, rhs: i64) -> Self {
        self.div_floor(rhs)
    }
}

impl AddAssign for Fixed {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Fixed {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

/// A position in the world, in [`Fixed`] units on each axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FixedVec3 {
    pub x: Fixed,
    pub y: Fixed,
    pub z: Fixed,
}

impl FixedVec3 {
    pub const ZERO: Self = Self {
        x: Fixed::ZERO,
        y: Fixed::ZERO,
        z: Fixed::ZERO,
    };

    pub const fn new(x: Fixed, y: Fixed, z: Fixed) -> Self {
        Self { x, y, z }
    }

    pub fn from_blocks(x: i32, y: i32, z: i32) -> Self {
        Self::new(
            Fixed::from_blocks(x),
            Fixed::from_blocks(y),
            Fixed::from_blocks(z),
        )
    }

    pub fn from_f32(v: [f32; 3]) -> Self {
        Self::new(
            Fixed::from_f32(v[0]),
            Fixed::from_f32(v[1]),
            Fixed::from_f32(v[2]),
        )
    }

    pub fn to_f32(self) -> [f32; 3] {
        [self.x.to_f32(), self.y.to_f32(), self.z.to_f32()]
    }

    /// The block containing this position.
    pub fn floor_block(self) -> [i32; 3] {
        [
            self.x.floor_block(),
            self.y.floor_block(),
            self.z.floor_block(),
        ]
    }

    /// Squared distance, in sub-units squared.
    ///
    /// Squared rather than a real distance because a square root is either a
    /// float — which would put a float back on an authority path — or an
    /// integer approximation nobody needs. Every comparison the simulation makes
    /// works as well against a squared radius.
    pub fn distance_squared(self, other: Self) -> i128 {
        let d = |a: Fixed, b: Fixed| (a.0 - b.0) as i128;
        let (dx, dy, dz) = (d(self.x, other.x), d(self.y, other.y), d(self.z, other.z));
        dx * dx + dy * dy + dz * dz
    }
}

impl Add for FixedVec3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl Sub for FixedVec3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl AddAssign for FixedVec3 {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_blocks_round_trip() {
        for b in [-1_000_000, -1, 0, 1, 42, 1_000_000] {
            let f = Fixed::from_blocks(b);
            assert_eq!(f.floor_block(), b, "block {b}");
            assert_eq!(f.fract_raw(), 0, "block {b} has no fraction");
        }
    }

    #[test]
    fn the_block_you_are_in_is_the_floor_not_the_truncation() {
        // The bug this prevents: at y = -0.5 truncation says block 0, and the
        // correct answer is block -1. Getting it wrong puts the player one block
        // out in exactly the half of the world that only exists since the height
        // limit was removed.
        let half = Fixed::from_raw(ONE / 2);
        assert_eq!(half.floor_block(), 0);
        assert_eq!((-half).floor_block(), -1);
        assert_eq!((Fixed::from_blocks(-3) + half).floor_block(), -3);
        assert_eq!((Fixed::from_blocks(-3) - half).floor_block(), -4);
    }

    #[test]
    fn the_fraction_is_never_negative() {
        // It pairs with `floor_block`: position == block + fraction, always,
        // on both sides of zero.
        for raw in [-3 * ONE - 1, -ONE, -1, 0, 1, ONE, 3 * ONE + 7] {
            let f = Fixed::from_raw(raw);
            let rebuilt = Fixed::from_blocks(f.floor_block()) + Fixed::from_raw(f.fract_raw());
            assert!(f.fract_raw() >= 0 && f.fract_raw() < ONE, "raw {raw}");
            assert_eq!(rebuilt, f, "raw {raw} did not rebuild");
        }
    }

    #[test]
    fn precision_does_not_change_with_distance() {
        // **The direct answer to the f32 cap.** An f32 at y = 1,000,000 cannot
        // resolve better than ~0.06 blocks, and past ~8.4 million cannot count
        // whole blocks. Here the smallest step is the same everywhere.
        let smallest = Fixed::from_raw(1);
        for blocks in [0i32, 1_000, 1_000_000, 100_000_000] {
            let base = Fixed::from_blocks(blocks);
            assert_ne!(base + smallest, base, "lost resolution at {blocks} blocks");
            assert_eq!((base + smallest).raw() - base.raw(), 1, "at {blocks}");
        }
    }

    #[test]
    fn repeated_addition_is_exact_where_f32_drifts() {
        // The determinism argument, made executable rather than asserted.
        //
        // Adding the same small step 100,000 times must equal one multiplication
        // by 100,000. `f32` does not manage this -- it accumulates rounding
        // error, and two machines that round differently anywhere along the way
        // end up in different places. That is a desync.
        let step = Fixed::from_raw(ONE / 60); // one tick of a 1 block/s velocity
        let mut acc = Fixed::ZERO;
        for _ in 0..100_000 {
            acc += step;
        }
        assert_eq!(acc, step * 100_000, "fixed-point drifted");

        // And the contrast, so the reason is visible in the test rather than
        // only in a comment.
        let step_f = 1.0f32 / 60.0;
        let mut acc_f = 0.0f32;
        for _ in 0..100_000 {
            acc_f += step_f;
        }
        assert_ne!(
            acc_f,
            step_f * 100_000.0,
            "f32 accumulation was expected to drift; if this ever passes, the \
             comparison above has stopped demonstrating anything"
        );
    }

    #[test]
    fn division_rounds_the_same_way_going_up_and_down() {
        // Truncating division rounds toward zero, so a falling body and a rising
        // one would round in opposite directions -- a small asymmetry that
        // accumulates over a long fall.
        assert_eq!(Fixed::from_raw(-1).div_floor(60), Fixed::from_raw(-1));
        assert_eq!(Fixed::from_raw(59).div_floor(60), Fixed::ZERO);
        assert_eq!(Fixed::from_raw(-59).div_floor(60), Fixed::from_raw(-1));
        assert_eq!(Fixed::from_raw(-60).div_floor(60), Fixed::from_raw(-1));
    }

    #[test]
    fn f32_round_trips_within_half_a_sub_unit() {
        // The boundary with rendering and with code that has not moved yet.
        for v in [0.0f32, 0.5, -0.5, 1.62, -1234.5678, 48.0] {
            let back = Fixed::from_f32(v).to_f32();
            assert!(
                (back - v).abs() <= 1.0 / ONE as f32,
                "{v} round-tripped to {back}"
            );
        }
    }

    #[test]
    fn distance_squared_is_exact_and_survives_large_coordinates() {
        // `i128` because two positions a million blocks apart, squared, in
        // 1/65536ths, overflows i64 comfortably.
        let a = FixedVec3::from_blocks(0, 0, 0);
        let b = FixedVec3::from_blocks(3, 4, 0);
        assert_eq!(a.distance_squared(b), (5i128 * ONE as i128).pow(2));

        let far = FixedVec3::from_blocks(1_000_000, 0, 0);
        assert_eq!(
            a.distance_squared(far),
            (1_000_000i128 * ONE as i128).pow(2),
            "large coordinates overflowed or lost precision"
        );
    }

    #[test]
    fn a_vector_knows_which_block_it_is_in() {
        let p = FixedVec3::new(
            Fixed::from_blocks(5) + Fixed::from_raw(ONE / 2),
            Fixed::from_blocks(-2) - Fixed::from_raw(1),
            Fixed::from_blocks(0),
        );
        assert_eq!(p.floor_block(), [5, -3, 0]);
    }
}
