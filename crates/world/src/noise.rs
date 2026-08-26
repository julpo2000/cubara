//! Positional hash and value noise -- the only source of randomness worldgen
//! uses (`docs/PHASE1_ARCHITECTURE.md` §8.2): a pure function of a seed and
//! an integer position, never a stateful stream, so nothing about generation
//! order can leak into the result (§8.1).

/// A fast, well-distributed hash of `seed` and an integer position. Pure
/// integer arithmetic throughout (no floats), so it produces the same bits
/// on every platform -- the foundation the §8.5 cross-platform determinism
/// guarantee is built on.
pub(crate) fn hash(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut h = seed;
    h ^= (x as u32 as u64).wrapping_mul(0x9E3779B97F4A7C15);
    h ^= (y as u32 as u64).wrapping_mul(0xC2B2AE3D27D4EB4F);
    h ^= (z as u32 as u64).wrapping_mul(0x165667B19E3779F9);
    // splitmix64's finalizer -- a well-studied bit mixer, chosen so nearby
    // inputs (adjacent lattice points, which worldgen samples constantly)
    // don't produce correlated outputs.
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D049BB133111EB);
    h ^= h >> 31;
    h
}

/// `hash`, remapped to `[-1, 1)` -- what every lattice corner below draws
/// its value from. Uses the top 24 bits so the `f32` conversion is exact
/// (an `f32` mantissa holds 24 bits), not an approximation of a wider hash.
fn lattice(seed: u64, x: i32, y: i32, z: i32) -> f32 {
    let bits = (hash(seed, x, y, z) >> 40) as u32;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Smoothstep -- zero first derivative at both ends, so neighbouring lattice
/// cells' interpolation curves meet with no visible seam at the boundary.
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// 2D value noise in roughly `[-1, 1]`: hash the 4 lattice corners around
/// `(x, z)`, smoothstep-interpolate between them. The terrain height field
/// is built from this (worldgen's `y` axis is height, so shape lives in the
/// other two).
pub fn value2(seed: u64, x: f32, z: f32) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let (ix0, iz0) = (x0 as i32, z0 as i32);
    let tx = smooth(x - x0);
    let tz = smooth(z - z0);
    let c00 = lattice(seed, ix0, 0, iz0);
    let c10 = lattice(seed, ix0 + 1, 0, iz0);
    let c01 = lattice(seed, ix0, 0, iz0 + 1);
    let c11 = lattice(seed, ix0 + 1, 0, iz0 + 1);
    lerp(lerp(c00, c10, tx), lerp(c01, c11, tx), tz)
}

/// 3D value noise in roughly `[-1, 1]`: hash the 8 lattice corners around
/// `(x, y, z)`, smoothstep-interpolate between them. Caves are built from
/// this -- true 3D shape is the whole point (§8.3).
pub fn value3(seed: u64, x: f32, y: f32, z: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let z0 = z.floor();
    let (ix0, iy0, iz0) = (x0 as i32, y0 as i32, z0 as i32);
    let tx = smooth(x - x0);
    let ty = smooth(y - y0);
    let tz = smooth(z - z0);
    let c000 = lattice(seed, ix0, iy0, iz0);
    let c100 = lattice(seed, ix0 + 1, iy0, iz0);
    let c010 = lattice(seed, ix0, iy0 + 1, iz0);
    let c110 = lattice(seed, ix0 + 1, iy0 + 1, iz0);
    let c001 = lattice(seed, ix0, iy0, iz0 + 1);
    let c101 = lattice(seed, ix0 + 1, iy0, iz0 + 1);
    let c011 = lattice(seed, ix0, iy0 + 1, iz0 + 1);
    let c111 = lattice(seed, ix0 + 1, iy0 + 1, iz0 + 1);
    let x00 = lerp(c000, c100, tx);
    let x10 = lerp(c010, c110, tx);
    let x01 = lerp(c001, c101, tx);
    let x11 = lerp(c011, c111, tx);
    let y0v = lerp(x00, x10, ty);
    let y1v = lerp(x01, x11, ty);
    lerp(y0v, y1v, tz)
}

/// Fractal Brownian motion: `octaves` layers of [`value2`] at doubling
/// frequency and halving amplitude (by default), normalised back to roughly
/// `[-1, 1]`. Each octave draws from its own seed offset so it isn't just
/// the same pattern repeated at a different scale.
pub fn fbm2(seed: u64, x: f32, z: f32, octaves: u32, lacunarity: f32, gain: f32) -> f32 {
    let mut freq = 1.0;
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        let octave_seed = seed.wrapping_add((o as u64).wrapping_mul(0x9E3779B97F4A7C15));
        sum += value2(octave_seed, x * freq, z * freq) * amp;
        norm += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    sum / norm
}

/// The 3D counterpart of [`fbm2`], layering [`value3`] instead.
pub fn fbm3(seed: u64, x: f32, y: f32, z: f32, octaves: u32, lacunarity: f32, gain: f32) -> f32 {
    let mut freq = 1.0;
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        let octave_seed = seed.wrapping_add((o as u64).wrapping_mul(0x9E3779B97F4A7C15));
        sum += value3(octave_seed, x * freq, y * freq, z * freq) * amp;
        norm += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash(42, 1, 2, 3), hash(42, 1, 2, 3));
    }

    #[test]
    fn hash_distinguishes_seed_and_position() {
        assert_ne!(hash(42, 1, 2, 3), hash(43, 1, 2, 3), "different seed");
        assert_ne!(hash(42, 1, 2, 3), hash(42, 1, 2, 4), "different position");
    }

    #[test]
    fn hash_handles_negative_and_extreme_coordinates_without_panicking() {
        let _ = hash(0, -1000, -1000, -1000);
        let _ = hash(u64::MAX, i32::MIN, i32::MAX, 0);
    }

    #[test]
    fn lattice_output_stays_in_range() {
        for i in -50..50 {
            let v = lattice(7, i, -i, i * 2);
            assert!((-1.0..1.0).contains(&v), "{v} out of range at {i}");
        }
    }

    #[test]
    fn value_noise_is_continuous_at_lattice_boundaries() {
        // Sampling just below and just above an integer boundary should be
        // close, not a discontinuous jump -- proves interpolation (not just
        // the raw per-lattice-point hash) is what's actually sampled.
        let a = value2(1, 4.999, 4.999);
        let b = value2(1, 5.001, 5.001);
        assert!(
            (a - b).abs() < 0.05,
            "discontinuity at lattice boundary: {a} vs {b}"
        );
        let a3 = value3(1, 4.999, 4.999, 4.999);
        let b3 = value3(1, 5.001, 5.001, 5.001);
        assert!(
            (a3 - b3).abs() < 0.05,
            "3D discontinuity at lattice boundary: {a3} vs {b3}"
        );
    }

    #[test]
    fn value_noise_matches_lattice_hash_at_integer_coordinates() {
        // At an exact lattice point, interpolation's t=0 must reduce to
        // exactly the underlying corner hash.
        assert_eq!(value2(9, 3.0, -2.0), lattice(9, 3, 0, -2));
        assert_eq!(value3(9, 3.0, 1.0, -2.0), lattice(9, 3, 1, -2));
    }

    #[test]
    fn fbm_stays_roughly_in_unit_range() {
        for i in 0..20 {
            let v = fbm2(3, i as f32 * 1.3, -i as f32 * 0.7, 4, 2.0, 0.5);
            assert!((-1.2..1.2).contains(&v), "{v} out of expected range");
            let v3 = fbm3(
                3,
                i as f32 * 1.3,
                i as f32 * 0.4,
                -i as f32 * 0.7,
                4,
                2.0,
                0.5,
            );
            assert!((-1.2..1.2).contains(&v3), "{v3} out of expected range");
        }
    }
}
