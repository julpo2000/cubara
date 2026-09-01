//! Directions as integers, so two machines cannot disagree about where a
//! player is looking.
//!
//! # Why this exists
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §3.5 named this before the netcode block
//! started, precisely so it would not be discovered during it:
//!
//! > Angles are the awkward ones: they feed `look_dir()` through `sin`/`cos`,
//! > and the resulting ray decides **which block gets mined** — an edit, and
//! > therefore authority. `sin` and `cos` are among the least portable
//! > functions in any standard library.
//!
//! Positions moved to [`Fixed`] in block 2.x. This is the other half: the last
//! floats in the authority hash, and the last call into a platform's libm on a
//! path that decides what happens to the world.
//!
//! # Binary angles
//!
//! A full turn is 2³². That single choice does most of the work:
//!
//! - **Wrapping is free and exact.** Turning right past north is
//!   `wrapping_add`; there is no `% 2π` to get subtly wrong, and no angle that
//!   drifts after ten million mouse movements.
//! - **Every angle is representable.** An `i32` covers exactly one turn, so
//!   there is no unrepresentable region and no clamping at the type's edge.
//! - **Comparison is integer comparison**, which is what a clamp on pitch
//!   needs.
//!
//! The resolution is a turn / 2³² ≈ 1.5 × 10⁻⁹ of a turn. For scale, at the
//! 5-block reach that decides which block a click breaks, one unit of angle
//! moves the far end of the ray by about 5 × 10⁻⁸ blocks.
//!
//! # Why not radians in [`Fixed`]
//!
//! Radians need π, which is not representable, so wrapping would accumulate
//! error — the exact failure this type exists to prevent. Turns have no such
//! constant.

use crate::fixed::Fixed;

/// A direction, as a binary angle: a full turn is 2³², so an `i32` covers
/// exactly one turn and wrapping is exact.
///
/// Zero looks toward −Z, matching the yaw convention the camera already used.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Angle(i32);

/// A quarter turn: straight up, and the limit pitch clamps against.
///
/// `1 << 30`, which is why the quarter turn is the largest angle that is
/// comfortably positive in an `i32` — a half turn is `1 << 31`, which is
/// `i32::MIN` when written as one. Nothing needs to name a positive half turn,
/// and wrapping arithmetic handles crossing it.
const QUARTER: i32 = 1 << 30;

impl Angle {
    pub const ZERO: Self = Self(0);
    /// A quarter turn — straight up, before pitch clamping.
    pub const QUARTER_TURN: Self = Self(QUARTER);

    /// From the raw binary angle. The inverse of [`raw`](Self::raw).
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    /// The raw binary angle — what gets hashed and what will cross the wire.
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// From turns: `0.25` is a quarter turn.
    ///
    /// Rounds to the nearest representable angle. A construction helper for
    /// tests and for start-up values written by a person; the simulation never
    /// calls it, because the simulation never has a float to convert.
    pub fn from_turns(turns: f32) -> Self {
        // Through f64 and wrapping, so a value outside one turn folds into
        // range rather than saturating at `i32::MAX` -- `from_turns(1.25)` is a
        // quarter turn, which is the only reading that matches the wrapping the
        // rest of the type does.
        let scaled = (turns as f64 * 4_294_967_296.0).round();
        Self(scaled.rem_euclid(4_294_967_296.0) as u32 as i32)
    }

    /// From radians. The bridge for values that were radians before this type
    /// existed, and for a person writing an angle in a test.
    pub fn from_radians(radians: f32) -> Self {
        Self::from_turns(radians / std::f32::consts::TAU)
    }

    /// In radians, for presentation and for tests.
    ///
    /// **Never feed this back into the simulation.** It is a lossy conversion
    /// out of the exact representation, and the point of the exact
    /// representation is that authority never leaves it.
    pub fn to_radians(self) -> f32 {
        self.0 as f32 / 4_294_967_296.0 * std::f32::consts::TAU
    }

    /// Turn by `delta`, wrapping. The whole reason a turn is 2³².
    pub fn wrapping_add(self, delta: Self) -> Self {
        Self(self.0.wrapping_add(delta.0))
    }

    /// Turn by `-delta`, wrapping.
    pub fn wrapping_sub(self, delta: Self) -> Self {
        Self(self.0.wrapping_sub(delta.0))
    }

    /// Clamp to ±`limit`. What pitch uses, so looking up cannot go over the
    /// top and invert the view.
    pub fn clamp(self, limit: Self) -> Self {
        Self(self.0.clamp(-limit.0, limit.0))
    }

    /// Interpolate toward `other` by `t`, taking the **short way round**.
    ///
    /// Presentation only — this is what smooths the camera between two ticks,
    /// and it is the one place a float belongs, because a wrong last bit here
    /// is a sub-pixel difference rather than a divergence
    /// (`PHASE1_ARCHITECTURE.md` §9 draws the same line for position).
    ///
    /// The short way round is what wrapping gives for free: the difference of
    /// two binary angles, as a wrapped `i32`, *is* the signed shortest path.
    /// Turning past north interpolates through north rather than sweeping all
    /// the way back around, with no special case for the seam.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let delta = other.0.wrapping_sub(self.0);
        Self(self.0.wrapping_add((delta as f32 * t) as i32))
    }

    /// Sine, as a [`Fixed`] in `-1..=1`.
    pub fn sin(self) -> Fixed {
        Fixed::from_raw(sin_raw(self.0))
    }

    /// Cosine, as a [`Fixed`] in `-1..=1`.
    pub fn cos(self) -> Fixed {
        // cos θ = sin(θ + quarter turn), wrapping.
        Fixed::from_raw(sin_raw(self.0.wrapping_add(QUARTER)))
    }

    /// Both at once, which is how they are almost always wanted.
    pub fn sin_cos(self) -> (Fixed, Fixed) {
        (self.sin(), self.cos())
    }
}

/// Working precision for the polynomial: 30 fractional bits, so a value in
/// `-1..=1` uses most of an `i64` and the intermediate powers keep their bits.
/// Results are rounded down to [`Fixed`]'s 16 at the end, once.
const P_BITS: u32 = 30;
const P_ONE: i64 = 1 << P_BITS;

/// Least-squares coefficients for `sin(π/2 · z)` on `z ∈ [0, 1]`, as an odd
/// polynomial `a₁z + a₃z³ + a₅z⁵ + a₇z⁷`, in [`P_BITS`] fixed point.
///
/// **Fitted, not truncated.** The Taylor series to the same degree is off by 11
/// [`Fixed`] ULP — it is built to be exact near zero, and spends its accuracy
/// there instead of where it is needed. Fitting across the whole quarter turn
/// gets to 1 ULP with the same four multiplies; Taylor needs five terms to
/// match it.
///
/// One ULP is the floor worth aiming at: below it the extra precision is
/// rounded away by [`Fixed`] itself. `sine_matches_the_reference_within_one_ulp`
/// is the check, over 200,000 angles spread across the turn.
const A1: i64 = 1_686_625_474; // 1.570792378916
const A3: i64 = -693_536_295; // -0.645906007763
const A5: i64 = 85_324_728; // 0.079464845055
const A7: i64 = -4_673_781; // -0.004352797793

/// Sine of a binary angle, as a raw [`Fixed`].
///
/// Folds the full turn into the first quarter and evaluates the polynomial
/// there — the standard reduction, and the reason only one quarter needs
/// coefficients. The fold is pure integer masking, so it introduces no error of
/// its own: a quarter turn is a power of two.
fn sin_raw(angle: i32) -> i64 {
    // Which quadrant, and how far into it. `as u32` first, so the arithmetic is
    // a plain unsigned split rather than something that has to think about
    // negative numbers.
    let a = angle as u32;
    let quadrant = a >> 30;
    let within = (a & (QUARTER as u32 - 1)) as i64;

    // `z` is the position within the quadrant, 0..1 at P_BITS. Both ends are
    // representable because a quarter turn is exactly 2^30 and so is P_ONE.
    let z = match quadrant {
        // Rising through the first and falling through the second: the second
        // quadrant is the first, mirrored.
        0 | 2 => within,
        _ => P_ONE - within,
    };

    let s = poly_sin(z);
    // Quadrants 2 and 3 are the negative half.
    let s = if quadrant >= 2 { -s } else { s };

    // Down to Fixed's 16 fractional bits, rounding to nearest rather than
    // truncating -- truncation would bias every angle toward zero, which over a
    // whole turn is a systematic error rather than noise.
    let shift = P_BITS - crate::fixed::FRAC_BITS;
    let half = 1i64 << (shift - 1);
    (s + half) >> shift
}

/// `sin(π/2 · z)` for `z ∈ [0, 1]` at [`P_BITS`], by Horner's method on `z²`.
///
/// `i128` intermediates rather than careful `i64` shifting: the widening is
/// free on both target platforms, and the alternative is four places where an
/// overflow would be silent and only visible as a direction that is subtly
/// wrong at one angle.
fn poly_sin(z: i64) -> i64 {
    let mul = |a: i64, b: i64| -> i64 { ((a as i128 * b as i128) >> P_BITS) as i64 };
    let z2 = mul(z, z);
    let mut acc = A7;
    acc = mul(acc, z2) + A5;
    acc = mul(acc, z2) + A3;
    acc = mul(acc, z2) + A1;
    mul(acc, z)
}

/// The unit view direction for a yaw and pitch, in [`Fixed`].
///
/// Here rather than on `Player` because it is trigonometry, and because the
/// simulation crate should be able to ask for a direction without owning the
/// maths that produces one.
///
/// Zero yaw looks toward −Z; positive pitch looks up.
pub fn look_dir(yaw: Angle, pitch: Angle) -> [Fixed; 3] {
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let mul = |a: Fixed, b: Fixed| Fixed::from_raw((a.raw() * b.raw()) >> crate::fixed::FRAC_BITS);
    [mul(cp, sy), sp, Fixed::from_raw(-mul(cp, cy).raw())]
}

/// The horizontal `(forward, right)` unit vectors for a yaw, ignoring pitch —
/// what walking moves along, since looking up must not tilt you into the sky.
pub fn horizontal_axes(yaw: Angle) -> ([Fixed; 3], [Fixed; 3]) {
    let (sy, cy) = yaw.sin_cos();
    (
        [sy, Fixed::ZERO, Fixed::from_raw(-cy.raw())],
        [cy, Fixed::ZERO, sy],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::ONE;

    /// One `Fixed` ULP. Every accuracy claim below is stated against this,
    /// because being more accurate than the type can hold is not a property
    /// worth asserting.
    const ULP: i64 = 1;

    #[test]
    fn a_full_turn_is_exactly_representable() {
        // The property the whole representation rests on: adding four quarter
        // turns is the identity, with no accumulated error at all.
        let start = Angle::from_raw(12_345_678);
        let mut a = start;
        for _ in 0..4 {
            a = a.wrapping_add(Angle::QUARTER_TURN);
        }
        assert_eq!(a, start, "four quarter turns is exactly where it started");
    }

    /// Ten million mouse movements must not drift, which is the thing radians
    /// in floating point cannot promise.
    #[test]
    fn turning_forever_does_not_drift() {
        let step = Angle::from_raw(7919); // a prime, so it lands everywhere
        let mut a = Angle::ZERO;
        for _ in 0..10_000_000 {
            a = a.wrapping_add(step);
        }
        let expected = Angle::from_raw((7919i64 * 10_000_000i64) as u32 as i32);
        assert_eq!(a, expected, "exact after ten million turns");
    }

    #[test]
    fn the_cardinal_directions_are_exact() {
        assert_eq!(Angle::ZERO.sin(), Fixed::ZERO);
        assert_eq!(Angle::ZERO.cos(), Fixed::ONE);
        assert_eq!(Angle::QUARTER_TURN.sin(), Fixed::ONE);
        assert_eq!(Angle::QUARTER_TURN.cos(), Fixed::ZERO);
        let half = Angle::from_raw(i32::MIN); // a half turn
        assert_eq!(half.sin(), Fixed::ZERO);
        assert_eq!(half.cos(), Fixed::from_raw(-ONE));
    }

    /// The accuracy claim, over the whole turn rather than at a few nice
    /// angles: our integer sine and the platform's agree to within what
    /// [`Fixed`] can represent.
    ///
    /// This is the test that would catch a wrong coefficient, and the one that
    /// says the polynomial is good enough to replace `f32::sin` outright rather
    /// than merely being close.
    #[test]
    fn sine_matches_the_reference_within_one_ulp() {
        let mut worst = 0i64;
        // A prime step, so the samples are spread over the turn rather than
        // landing on the quadrant boundaries the reduction handles specially.
        let mut a: i32 = i32::MIN;
        for _ in 0..200_000 {
            let want = ((a as f64 / 4_294_967_296.0 * std::f64::consts::TAU).sin() * ONE as f64)
                .round() as i64;
            let got = Angle::from_raw(a).sin().raw();
            worst = worst.max((want - got).abs());
            a = a.wrapping_add(21_487);
        }
        assert!(
            worst <= ULP,
            "integer sine is off the reference by {worst} ULP"
        );
    }

    #[test]
    fn cosine_is_sine_a_quarter_turn_along() {
        for raw in [-1_000_000_000, -7, 0, 12_345, 900_000_000] {
            let a = Angle::from_raw(raw);
            assert_eq!(a.cos(), a.wrapping_add(Angle::QUARTER_TURN).sin());
        }
    }

    /// `sin² + cos² = 1`, which is the property a raycast actually depends on:
    /// a direction that is not a unit vector makes reach mean different things
    /// in different directions.
    #[test]
    fn every_direction_is_a_unit_vector() {
        let mut a: i32 = 0;
        for _ in 0..20_000 {
            let (s, c) = Angle::from_raw(a).sin_cos();
            let sum = (s.raw() * s.raw() + c.raw() * c.raw()) >> crate::fixed::FRAC_BITS;
            assert!(
                (sum - ONE).abs() <= 2,
                "not a unit vector at {a}: {sum} vs {ONE}"
            );
            a = a.wrapping_add(214_871);
        }
    }

    #[test]
    fn pitch_clamps_symmetrically() {
        let limit = Angle::from_turns(0.24);
        assert_eq!(Angle::QUARTER_TURN.clamp(limit), limit);
        assert_eq!(
            Angle::from_raw(-QUARTER).clamp(limit),
            Angle::from_raw(-limit.raw())
        );
        assert_eq!(Angle::ZERO.clamp(limit), Angle::ZERO);
    }

    /// Interpolation takes the short way round, including across the seam where
    /// a naive lerp would sweep the long way.
    #[test]
    fn interpolation_crosses_the_seam_the_short_way() {
        // Just before and just after the wrap point.
        let a = Angle::from_raw(i32::MAX - 100);
        let b = Angle::from_raw(i32::MIN + 100);
        let mid = a.lerp(b, 0.5);
        // The short way is 201 units; the midpoint is ~100 units from each end.
        let from_a = mid.raw().wrapping_sub(a.raw()).abs();
        assert!(from_a < 200, "swept the long way round: {from_a} units");
    }

    #[test]
    fn turns_and_radians_round_trip() {
        for turns in [0.0f32, 0.125, 0.25, -0.3, 0.499] {
            let a = Angle::from_turns(turns);
            assert!(
                (a.to_radians() - turns * std::f32::consts::TAU).abs() < 1e-4,
                "{turns} turns did not round-trip"
            );
        }
        // Outside one turn folds in, rather than saturating.
        assert_eq!(Angle::from_turns(1.25), Angle::QUARTER_TURN);
    }

    #[test]
    fn look_dir_is_a_unit_vector_and_points_where_it_should() {
        // Zero yaw, level: straight down -Z.
        let d = look_dir(Angle::ZERO, Angle::ZERO);
        assert_eq!(d[0], Fixed::ZERO);
        assert_eq!(d[1], Fixed::ZERO);
        assert_eq!(d[2], Fixed::from_raw(-ONE));

        // A quarter turn of yaw: along +X, which is what the old f32 test
        // `yaw_ninety_degrees_looks_along_positive_x` asserted.
        let d = look_dir(Angle::QUARTER_TURN, Angle::ZERO);
        assert_eq!(d[0], Fixed::ONE);
        assert!(d[2].raw().abs() <= 1);

        // Straight up.
        let d = look_dir(Angle::ZERO, Angle::QUARTER_TURN);
        assert_eq!(d[1], Fixed::ONE);
    }
}
