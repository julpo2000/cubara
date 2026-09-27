//! Block-aggregate shading: what a pixel covering many blocks of a slope
//! should show (`docs/PROPOSAL_FAR_VIEW.md` §3.3). The CPU reference that
//! `far.wgsl`'s fragment shader is ported from (block F4), with the
//! brute-force checks that keep it honest (§7, "Colour").
//!
//! A smooth slope built from blocks is a staircase. Per unit of ground, a
//! slope of gradient `(gx, gz)` blocks per block carries one unit of **top**
//! face (normal up) and `|gx|` and `|gz|` units of **riser**, facing down the
//! slope along x and along z. From far away one pixel covers many steps, and
//! the colour it shows is those faces' colours, each weighted by how much of
//! the pixel it covers ([`FaceWeights`]).
//!
//! **The weights are not the faces' projected areas.** Each face covers its
//! area times the cosine to the eye only if nothing stands in front of it.
//! That holds when every riser faces the eye, and it fails badly when the
//! risers along one axis face the eye and those along the other face away.
//! The steps along the second axis then drop away from the eye and hide what
//! lies behind them, and what they hide depends on how the steps along the
//! two axes interleave, not on areas alone. The simple area-weighted mix
//! calls a gentle hill seen side-on at 2° two-thirds soil, when it is 95%
//! grass. That is off by up to 0.66 of the pixel, and it is the most common
//! far-away view there is.
//!
//! So there are three layers here:
//!
//! - [`reference_weights`]: **the definition.** It walks lines across a real
//!   staircase of whole blocks and adds up what the eye sees along them,
//!   exactly. It is too slow for a shader, and it is checked against casting
//!   rays at the blocks one by one (in the tests).
//! - [`MaskingTable`]: the one case that hides anything, precomputed from the
//!   reference over a 16⁴ grid, 64 KB. It is committed as `aggregate.table`
//!   and becomes a texture in F4.
//! - [`visible_weights`] and [`aggregate_colour`]: what the shader will do,
//!   written for the CPU. The exact formula applies where nothing is hidden
//!   and the table where something is, and then `mesh.wgsl`'s lighting is
//!   applied to each face.
//!
//! **Left out, on purpose:** baked ambient occlusion (the crease where a riser
//! meets the top below it is darker on a real block), and shadows (the voxel
//! renderer has none). Texture detail within a face belongs to the caller:
//! `top` and `side` are the faces' mean colours.

use glam::Vec3;

use crate::Lighting;

/// How much of a pixel each kind of face covers. The three add up to one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceWeights {
    /// Top faces, normal `+y`.
    pub top: f32,
    /// Risers along x, normal [`riser_normals`]`.0`.
    pub x: f32,
    /// Risers along z, normal [`riser_normals`]`.1`.
    pub z: f32,
}

/// The outward normals of a slope's risers along x and along z. A riser
/// faces down the slope: a slope rising toward `+x` has risers facing `-x`.
/// A slope that is flat along an axis has no risers along it, and the normal
/// returned for that axis is then never weighted.
pub fn riser_normals(gradient: [f32; 2]) -> (Vec3, Vec3) {
    let facing = |g: f32| if g > 0.0 { -1.0 } else { 1.0 };
    (
        Vec3::new(facing(gradient[0]), 0.0, 0.0),
        Vec3::new(0.0, 0.0, facing(gradient[1])),
    )
}

// ---------------------------------------------------------------------------
// The definition
// ---------------------------------------------------------------------------

/// A slope built from whole blocks. Column `(i, j)` is solid up to
/// `floor(gx (i + ½) + gz (j + ½) + φ)`: the smooth slope rounded down to
/// whole blocks at each column, which is what the world does to a smooth
/// surface. The phase `φ` is where the rounding falls, and a real hillside
/// has every phase somewhere, so what is measured is averaged over it.
struct Staircase {
    gradient: [f64; 2],
}

/// A phase that is not a round number, so no column top sits exactly on a
/// step of the rounding.
const PHASE: f64 = 0.371;

impl Staircase {
    fn new(gradient: [f64; 2]) -> Self {
        Self { gradient }
    }

    fn top(&self, i: i64, j: i64, phase: f64) -> f64 {
        (self.gradient[0] * (i as f64 + 0.5) + self.gradient[1] * (j as f64 + 0.5) + phase).floor()
    }

    /// How much of the screen tops, x-risers and z-risers cover, measured
    /// exactly along `lines` parallel lines of `crossings` column crossings
    /// each.
    ///
    /// Rays in one vertical plane all meet the ground along the same line of
    /// columns, so walking that line once answers all of them. Tilt the frame
    /// so the rays are level: a column top at height `h`, a distance `s` along
    /// the line, sits at `r = h + s tan(elevation)`. A ray at level `r` then
    /// stops at the first place the profile reaches `r`, so what the eye sees
    /// is wherever the profile's running maximum grows. It grows along a top
    /// face (which rises at `tan(elevation)` in this frame), or up a riser. An
    /// increase in `r` is an increase in screen height (by a constant
    /// factor), so the increases, summed by kind of face, are the weights.
    fn scan(&self, view: [f64; 3], lines: usize, crossings: usize) -> [f64; 3] {
        let [vx, vy, vz] = view;
        let flat = (vx * vx + vz * vz).sqrt();
        if flat < 1e-9 {
            // Straight down: risers are edge-on.
            return [1.0, 0.0, 0.0];
        }
        let dir = [-vx / flat, -vz / flat];
        let t = vy / flat;
        let across = [-dir[1], dir[0]];
        // The first stretch only sets the running maximum. Counting from the
        // very start would count the edge of a slope with nothing in front.
        let warmup = crossings / 8;
        let mut sums = [0.0f64; 3];
        for k in 0..lines {
            // Spread across and along by irrational steps, each at its own
            // phase of the rounding. The phase is what matters most: a line
            // along a contour never leaves its phase, so without this a view
            // along the contours would see only the phases the lines happened
            // to start on.
            let a = k as f64 * 0.618_033_988_75 * 7.0;
            let b = k as f64 * 0.414_213_562_37 * 5.0;
            let phase = (PHASE + k as f64 * 0.618_033_988_75).fract();
            let start = [
                a * across[0] + b * dir[0] + 0.5,
                a * across[1] + b * dir[1] + 0.5,
            ];
            let mut cell = [start[0].floor() as i64, start[1].floor() as i64];
            let step = dir.map(|v| if v > 0.0 { 1 } else { -1 });
            let delta = dir.map(|v| {
                if v == 0.0 {
                    f64::INFINITY
                } else {
                    1.0 / v.abs()
                }
            });
            let mut next = [0, 1].map(|i| {
                if dir[i] > 0.0 {
                    (cell[i] as f64 + 1.0 - start[i]) / dir[i]
                } else if dir[i] < 0.0 {
                    (start[i] - cell[i] as f64) / -dir[i]
                } else {
                    f64::INFINITY
                }
            });
            let mut s = 0.0;
            let mut h = self.top(cell[0], cell[1], phase);
            let mut record = f64::NEG_INFINITY;
            for n in 0..crossings {
                let counting = n >= warmup;
                let i = if next[0] < next[1] { 0 } else { 1 };
                let exit = next[i];
                // Along the top of this column.
                let (lo, hi) = (h + s * t, h + exit * t);
                if hi > record {
                    if counting {
                        sums[0] += hi - record.max(lo);
                    }
                    record = hi;
                }
                cell[i] += step[i];
                next[i] += delta[i];
                s = exit;
                // Up the riser into the next column, if it is taller.
                let up = self.top(cell[0], cell[1], phase);
                if up > h {
                    let (lo, hi) = (h + s * t, up + s * t);
                    if hi > record {
                        if counting {
                            sums[1 + i] += hi - record.max(lo);
                        }
                        record = hi;
                    }
                }
                h = up;
            }
        }
        let total: f64 = sums.iter().sum();
        sums.map(|v| v / total)
    }
}

/// How much of a pixel the tops and the risers of a slope cover, seen along
/// `view` (unit, from the ground toward the eye): **the definition** the
/// rest of this module approximates. `None` when the slope itself faces away
/// from the eye and so covers no pixel at all.
///
/// Exact up to the length of the lines it walks: about 0.004 against rays
/// cast one by one. It takes about a millisecond, so it is for tests and for
/// building [`MaskingTable`], not for a frame.
pub fn reference_weights(gradient: [f32; 2], view: Vec3) -> Option<FaceWeights> {
    if faces_away(gradient, view) {
        return None;
    }
    let w = Staircase::new([gradient[0] as f64, gradient[1] as f64]).scan(
        [view.x as f64, view.y as f64, view.z as f64],
        REFERENCE_LINES,
        REFERENCE_CROSSINGS,
    );
    Some(FaceWeights {
        top: w[0] as f32,
        x: w[1] as f32,
        z: w[2] as f32,
    })
}

/// Lines [`reference_weights`] walks, and column crossings along each.
const REFERENCE_LINES: usize = 64;
const REFERENCE_CROSSINGS: usize = 3000;

/// Whether the slope as a whole faces away from `view`: the dot of `view`
/// with the sum of all its faces' area vectors, `(-gx, 1, -gz)` per unit of
/// ground, is its projected area.
fn faces_away(gradient: [f32; 2], view: Vec3) -> bool {
    view.y - gradient[0] * view.x - gradient[1] * view.z <= 0.0
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Samples along each of the table's four axes.
pub const TABLE_SIZE: usize = 16;

/// The steepest slope angle the table covers. Steeper is looked up as this.
pub const TABLE_MAX_SLOPE_DEG: f32 = 80.0;

/// The committed table, as [`MaskingTable::generate`] makes it.
const COMMITTED_TABLE: &[u8] = include_bytes!("aggregate.table");

/// The top fraction of the one arrangement that hides anything: the risers
/// along one axis (**A**) face the eye, and those along the other (**B**)
/// face away. The A-risers take the rest of the pixel.
///
/// Four axes of [`TABLE_SIZE`] samples each, stored as `u8`, with the first
/// axis varying fastest:
///
/// 0. the slope's steepness `θ = atan |g|`, stored as `sqrt(θ / θ_max)` up to
///    [`TABLE_MAX_SLOPE_DEG`], so the gentle slopes most land has get most of
///    the samples;
/// 1. how it leans between the axes, `atan2(|g_B|, |g_A|)`, 0° to 90°;
/// 2. the facing risers' share if nothing were hidden, `a / (v_y + a)`, with
///    `a = |g_A| |v_A|` their projected area and `v_y` the tops';
/// 3. how much the hiding axis hides, `h = b / (v_y + a)`, with
///    `b = |g_B| |v_B|`, stored as `1 - sqrt(1 - h)`. The slope faces away
///    from the eye at `h = 1`, and the split changes fastest just short of
///    it, so that is where the samples crowd.
///
/// The last two carry the area-weighted split itself: at axis 3's zero
/// nothing is hidden and the top fraction is exactly `1 - share`. What varies
/// fast (a riser turning edge-on, the eye dropping to the horizon) is in the
/// coordinates, so the table only has to hold what hiding changes, and that
/// changes slowly. The first attempt indexed by elevation and azimuth; that
/// was off by 0.6 at grazing angles, where the split changes fastest.
#[derive(Clone, PartialEq, Eq)]
pub struct MaskingTable {
    texels: Vec<u8>,
}

impl std::fmt::Debug for MaskingTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MaskingTable({} texels)", self.texels.len())
    }
}

impl MaskingTable {
    /// The committed table.
    pub fn committed() -> Self {
        Self::from_bytes(COMMITTED_TABLE.to_vec()).expect("aggregate.table is TABLE_SIZE^4 bytes")
    }

    /// A table from its bytes, if there are the right number of them.
    pub fn from_bytes(texels: Vec<u8>) -> Option<Self> {
        (texels.len() == TABLE_SIZE.pow(4)).then_some(Self { texels })
    }

    /// The table's bytes, as a texture would take them.
    pub fn bytes(&self) -> &[u8] {
        &self.texels
    }

    /// Build the table from [`reference_weights`]. Seconds in a release
    /// build, on every core. Each texel depends only on its own index, so the
    /// result does not depend on how the work is split.
    pub fn generate() -> Self {
        let n = TABLE_SIZE.pow(4);
        let threads = std::thread::available_parallelism().map_or(1, |p| p.get());
        let chunk = n.div_ceil(threads);
        let mut texels = vec![0u8; n];
        std::thread::scope(|scope| {
            for (c, out) in texels.chunks_mut(chunk).enumerate() {
                scope.spawn(move || {
                    for (k, texel) in out.iter_mut().enumerate() {
                        *texel = Self::texel(c * chunk + k);
                    }
                });
            }
        });
        Self { texels }
    }

    /// One texel, computed from the reference.
    fn texel(index: usize) -> u8 {
        let axis = |k: usize| ((index / TABLE_SIZE.pow(k as u32)) % TABLE_SIZE) as f64;
        let last = (TABLE_SIZE - 1) as f64;
        let max_slope = (TABLE_MAX_SLOPE_DEG as f64).to_radians();
        // The corners of the grid are limits no slope reaches exactly (a
        // hiding axis with no steps, a slope of nothing), so those texels are
        // measured just inside them, where the reference still has steps to
        // walk.
        let steepness = ((axis(0) / last).powi(2) * max_slope).max(0.2f64.to_radians());
        let lean = (axis(1) / last * 90.0).clamp(0.5, 89.5).to_radians();
        let share = axis(2) / last;
        let hiding = (1.0 - (1.0 - axis(3) / last).powi(2)).min(0.998);
        let slope = steepness.tan();
        let (ga, gb) = (slope * lean.cos(), slope * lean.sin());
        // Solve for the view: with `V = v_y + a`, `a = share V`,
        // `b = hiding V` and `v_y = (1 - share) V`, and the view is unit.
        let v = 1.0 / ((share / ga).powi(2) + (hiding / gb).powi(2) + (1.0 - share).powi(2)).sqrt();
        let (ua, ub, vy) = (share * v / ga, hiding * v / gb, (1.0 - share) * v);
        // Canonical: A is x, rising toward +x with the eye at -x; B is z,
        // rising toward +z with the eye at +z.
        let [top, ..] =
            Staircase::new([ga, gb]).scan([-ua, vy, ub], REFERENCE_LINES, REFERENCE_CROSSINGS);
        (top * 255.0).round().clamp(0.0, 255.0) as u8
    }

    /// The top fraction at table coordinates (each in `0..=TABLE_SIZE - 1`),
    /// interpolated between the sixteen texels around it, as a 3D texture's
    /// linear filtering plus one blend along the fourth axis will do.
    fn sample(&self, at: [f32; 4]) -> f32 {
        let last = (TABLE_SIZE - 1) as f32;
        let at = at.map(|c| c.clamp(0.0, last));
        let base = at.map(|c| (c.floor() as usize).min(TABLE_SIZE - 2));
        let frac: [f32; 4] = std::array::from_fn(|k| at[k] - base[k] as f32);
        let mut sum = 0.0;
        for corner in 0..16 {
            let mut weight = 1.0;
            let mut index = 0;
            for k in 0..4 {
                let bit = (corner >> k) & 1;
                weight *= if bit == 1 { frac[k] } else { 1.0 - frac[k] };
                index += (base[k] + bit) * TABLE_SIZE.pow(k as u32);
            }
            sum += weight * self.texels[index] as f32;
        }
        sum / 255.0
    }

    /// The top fraction for a facing axis of slope `g_a`, a hiding axis of
    /// slope `g_b` (both magnitudes), and the eye's components along them and
    /// up (all magnitudes, `v_a² + v_b² + v_y² = 1`).
    fn top_fraction(&self, g_a: f32, g_b: f32, v_a: f32, v_b: f32, v_y: f32) -> f32 {
        let last = (TABLE_SIZE - 1) as f32;
        let steepness = (g_a * g_a + g_b * g_b).sqrt().atan().to_degrees();
        let lean = g_b.atan2(g_a).to_degrees();
        let seen = v_y + g_a * v_a;
        self.sample([
            (steepness / TABLE_MAX_SLOPE_DEG).sqrt() * last,
            lean / 90.0 * last,
            g_a * v_a / seen * last,
            (1.0 - (1.0 - (g_b * v_b / seen).min(1.0)).sqrt()) * last,
        ])
    }
}

// ---------------------------------------------------------------------------
// What the shader does
// ---------------------------------------------------------------------------

/// How much of a pixel the tops and the risers of a slope cover, seen along
/// `view` (unit, from the ground toward the eye), as the far terrain's
/// shader will compute it. `None` when the slope faces away from the eye.
///
/// - **Every riser faces the eye** (or lies flat): nothing is hidden. The sum
///   of the faces' projected areas is then exactly the slope's own, and each
///   face's share is its projected area's.
/// - **No riser faces the eye:** only tops are seen.
/// - **One axis faces the eye and the other does not:** the steps along the
///   second axis hide part of what is behind them. The top fraction comes
///   from `table`, and the facing risers take the rest.
pub fn visible_weights(
    table: &MaskingTable,
    gradient: [f32; 2],
    view: Vec3,
) -> Option<FaceWeights> {
    if faces_away(gradient, view) {
        return None;
    }
    let [gx, gz] = gradient;
    // A riser faces the eye when the eye is on its downhill side.
    let x_faces = gx * view.x < 0.0;
    let z_faces = gz * view.z < 0.0;
    let x_hides = gx * view.x > 0.0;
    let z_hides = gz * view.z > 0.0;
    if x_faces && z_hides {
        let top = table.top_fraction(gx.abs(), gz.abs(), view.x.abs(), view.z.abs(), view.y);
        return Some(FaceWeights {
            top,
            x: 1.0 - top,
            z: 0.0,
        });
    }
    if z_faces && x_hides {
        let top = table.top_fraction(gz.abs(), gx.abs(), view.z.abs(), view.x.abs(), view.y);
        return Some(FaceWeights {
            top,
            x: 0.0,
            z: 1.0 - top,
        });
    }
    // Nothing hides: each face's share of the projected area.
    let top = view.y.max(0.0);
    let x = if x_faces {
        gx.abs() * view.x.abs()
    } else {
        0.0
    };
    let z = if z_faces {
        gz.abs() * view.z.abs()
    } else {
        0.0
    };
    let sum = top + x + z;
    Some(FaceWeights {
        top: top / sum,
        x: x / sum,
        z: z / sum,
    })
}

/// How brightly `mesh.wgsl` lights a face of normal `n`, before its texture
/// and its baked ambient occlusion: the sun's colour times hemispheric
/// ambient plus the sun's diffuse term. `fs_main` there is the definition;
/// this is the same expression, for the CPU.
pub fn face_light(n: Vec3, lighting: &Lighting) -> Vec3 {
    let diffuse = n.dot(lighting.sun_dir).max(0.0) * lighting.diffuse_weight;
    let ambient =
        lighting.ambient_low + (lighting.ambient_high - lighting.ambient_low) * (n.y * 0.5 + 0.5);
    lighting.sun_color * (ambient + diffuse)
}

/// The mean colours of a slope's faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Materials {
    /// The top of a block (grass, snow, sand...).
    pub top: Vec3,
    /// The side of a block, as a riser shows it.
    pub side: Vec3,
}

/// The colour a pixel covering many blocks of a slope shows: each face's lit
/// colour, weighted by how much of the pixel it covers. `None` when the slope
/// faces away from the eye.
pub fn aggregate_colour(
    table: &MaskingTable,
    gradient: [f32; 2],
    view: Vec3,
    lighting: &Lighting,
    materials: &Materials,
) -> Option<Vec3> {
    let w = visible_weights(table, gradient, view)?;
    Some(lit(w, gradient, lighting, materials))
}

/// Faces' lit colours, mixed by `w`.
fn lit(w: FaceWeights, gradient: [f32; 2], lighting: &Lighting, materials: &Materials) -> Vec3 {
    let (nx, nz) = riser_normals(gradient);
    materials.top * face_light(Vec3::Y, lighting) * w.top
        + materials.side * (face_light(nx, lighting) * w.x + face_light(nz, lighting) * w.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which face a ray met.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Hit {
        Top,
        /// A riser, by axis (0 = x, 1 = z) and whether its normal points
        /// along `+axis`.
        Side {
            axis: usize,
            positive: bool,
        },
    }

    /// **The brute force**, independent of [`Staircase::scan`]: parallel rays
    /// cast at the blocks one by one, counting which face each meets first.
    ///
    /// The rays land uniformly on a 64 x 64 square of the slope, which is
    /// uniform on screen too, because a plane projects affinely. Each is
    /// followed from four blocks above the slope, column by column, so a ray
    /// that meets a step in front of its target counts there, as the eye would
    /// see it.
    fn cast_rays(gradient: [f32; 2], view: Vec3, rays: usize) -> [f64; 3] {
        let stairs = Staircase::new([gradient[0] as f64, gradient[1] as f64]);
        let [gx, gz] = stairs.gradient;
        let (vx, vy, vz) = (view.x as f64, view.y as f64, view.z as f64);
        let m = vy - gx * vx - gz * vz;
        assert!(m > 0.0, "the slope faces away from the eye");
        let plane = |x: f64, z: f64| gx * x + gz * z + PHASE;
        let mut counts = [0usize; 3];
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut unit = || {
            // xorshift64*, enough for placing rays.
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            (rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
        };
        let side = (rays as f64).sqrt().ceil() as usize;
        for n in 0..rays {
            let (a, b) = (n % side, n / side);
            let tx = (a as f64 + unit()) / side as f64 * 64.0;
            let tz = (b as f64 + unit()) / side as f64 * 64.0;
            let back = 4.0 / m;
            let o = [tx + vx * back, plane(tx, tz) + vy * back, tz + vz * back];
            match cast(&stairs, o, [-vx, -vy, -vz]) {
                Hit::Top => counts[0] += 1,
                Hit::Side { axis, positive } => {
                    let g = stairs.gradient[axis];
                    assert_eq!(positive, g < 0.0, "a riser facing away was hit");
                    counts[1 + axis] += 1;
                }
            }
        }
        counts.map(|c| c as f64 / rays as f64)
    }

    /// Walk a ray from `o` along `d` column by column, and return the first
    /// face it meets.
    fn cast(stairs: &Staircase, o: [f64; 3], d: [f64; 3]) -> Hit {
        let origin = [o[0], o[2]];
        let dir = [d[0], d[2]];
        let mut cell = origin.map(|c| c.floor() as i64);
        let step = dir.map(|v| if v > 0.0 { 1 } else { -1 });
        let delta = dir.map(|v| {
            if v == 0.0 {
                f64::INFINITY
            } else {
                1.0 / v.abs()
            }
        });
        let mut next = [0, 1].map(|a| {
            if dir[a] > 0.0 {
                (cell[a] as f64 + 1.0 - origin[a]) / dir[a]
            } else if dir[a] < 0.0 {
                (origin[a] - cell[a] as f64) / -dir[a]
            } else {
                f64::INFINITY
            }
        });
        let mut entered = 0.0;
        let mut through = None;
        for _ in 0..1_000_000 {
            let top = stairs.top(cell[0], cell[1], PHASE);
            if o[1] + d[1] * entered < top {
                return through.expect("the ray starts above every step");
            }
            let a = if next[0] < next[1] { 0 } else { 1 };
            if o[1] + d[1] * next[a] <= top {
                return Hit::Top;
            }
            entered = next[a];
            next[a] += delta[a];
            cell[a] += step[a];
            // Entering while moving +x meets the column's -x face.
            through = Some(Hit::Side {
                axis: a,
                positive: step[a] < 0,
            });
        }
        panic!("a ray walked a million columns without meeting the ground");
    }

    fn view(elevation_deg: f32, azimuth_deg: f32) -> Vec3 {
        let (e, a) = (elevation_deg.to_radians(), azimuth_deg.to_radians());
        Vec3::new(e.cos() * a.cos(), e.sin(), e.cos() * a.sin())
    }

    fn as_array(w: FaceWeights) -> [f64; 3] {
        [w.top as f64, w.x as f64, w.z as f64]
    }

    fn worst(a: [f64; 3], b: [f64; 3]) -> f64 {
        (0..3).fold(0.0, |m, i| m.max((a[i] - b[i]).abs()))
    }

    /// Gradients from flat to cliff, both signs, both axes: with the
    /// azimuths below, every arrangement of facing and hiding.
    const GRADIENTS: [[f32; 2]; 9] = [
        [0.0, 0.0],
        [0.3, 0.0],
        [0.0, -0.3],
        [2.5, 0.0],
        [0.1, 0.07],
        [0.5, 0.5],
        [1.0, -0.4],
        [-2.0, 1.5],
        [3.0, 3.0],
    ];

    /// Every visible `(gradient, view)` of the grid. The azimuths are offset
    /// by 7° so none looks exactly along an axis.
    fn grid(elevations: &[f32], azimuth_step: usize) -> Vec<([f32; 2], Vec3)> {
        let mut out = Vec::new();
        for g in GRADIENTS {
            for &e in elevations {
                for azimuth in (0..360).step_by(azimuth_step) {
                    let v = view(e, azimuth as f32 + 7.0);
                    if !faces_away(g, v) {
                        out.push((g, v));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn the_reference_agrees_with_rays_cast_one_by_one() {
        let mut worst_seen = 0.0f64;
        for (g, v) in grid(&[2.0, 15.0, 60.0], 45) {
            let reference = as_array(reference_weights(g, v).unwrap());
            let rays = cast_rays(g, v, 10_000);
            let e = worst(reference, rays);
            // 10,000 rays have a standard error of about 0.005.
            assert!(
                e < 0.025,
                "g {g:?} view {v}: reference {reference:.3?}, rays {rays:.3?}"
            );
            worst_seen = worst_seen.max(e);
        }
        assert!(
            worst_seen > 0.0,
            "the two never differed at all: are they the same code?"
        );
    }

    /// A case worked out by hand: a slope of `g` along both axes, seen
    /// straight along its contour lines from just above the ground.
    ///
    /// Along a contour, the columns alternate between one on the line and one
    /// beside it, which is a step up on a fraction `g` of the lines and level
    /// on the rest. A ray skimming the ground stops at the first of the
    /// highest columns it reaches. On a line with steps, half the line is on
    /// them, so half the rays land on a step's top and half on the riser in
    /// front of it. On a line without steps every ray lands on a top. The
    /// risers' share is therefore `g / 2`.
    #[test]
    fn a_slope_seen_along_its_contours_matches_the_hand_count() {
        for g in [0.1f32, 0.3, 0.5] {
            let w = reference_weights([g, g], view(0.5, 135.0)).unwrap();
            assert!(
                (w.x - g / 2.0).abs() < 0.02,
                "g {g}: risers {:.3}, by hand {:.3}",
                w.x,
                g / 2.0
            );
        }
    }

    /// Random slopes and views, the same ones every run: slopes mostly
    /// gentle, as land is, and views mostly grazing, as the far terrain is
    /// seen. The gradients are not round numbers. An exactly rational slope
    /// is a periodic staircase whose steps line up in a way no real hillside
    /// holds across a pixel, and its narrow resonances are what a 16⁴ table
    /// cannot hold.
    fn random_views(n: usize) -> Vec<([f32; 2], Vec3)> {
        let mut rng = 0x1234_5678_9abc_def0u64;
        let mut unit = || {
            rng = rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (rng >> 11) as f32 / (1u64 << 53) as f32
        };
        let mut out = Vec::new();
        while out.len() < n {
            let g = [
                (unit() - 0.5) * 2.0 * (unit() * 1.5).powi(2),
                (unit() - 0.5) * 2.0 * (unit() * 1.5).powi(2),
            ];
            let v = view(unit().powi(2) * 90.0, unit() * 360.0);
            if !faces_away(g, v) {
                out.push((g, v));
            }
        }
        out
    }

    /// Mean, 95th percentile and worst of `errors`.
    fn spread(mut errors: Vec<f64>) -> (f64, f64, f64) {
        errors.sort_by(f64::total_cmp);
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        (
            mean,
            errors[errors.len() * 95 / 100],
            errors[errors.len() - 1],
        )
    }

    /// Where nothing hides, the shader's split is the exact one: projected
    /// areas, or tops alone.
    #[test]
    fn where_nothing_hides_the_split_is_exact() {
        let table = MaskingTable::committed();
        let mut checked = 0;
        for (g, v) in grid(&[1.0, 4.0, 12.0, 30.0, 55.0, 80.0], 20) {
            let hides = |gi: f32, vi: f32| gi * vi > 0.0;
            let faces = |gi: f32, vi: f32| gi * vi < 0.0;
            let mixed =
                (faces(g[0], v.x) && hides(g[1], v.z)) || (faces(g[1], v.z) && hides(g[0], v.x));
            if mixed {
                continue;
            }
            let shader = as_array(visible_weights(&table, g, v).unwrap());
            let reference = as_array(reference_weights(g, v).unwrap());
            assert!(
                worst(shader, reference) < 0.01,
                "g {g:?} view {v}: shader {shader:.3?}, reference {reference:.3?}"
            );
            checked += 1;
        }
        assert!(checked > 100, "only {checked} views had nothing hidden");
    }

    /// The shader's weights, table and all, against the definition.
    ///
    /// Measured over 2,000 of these views: mean 0.0012, 95th percentile about
    /// 0.005, worst 0.18. The worst are slopes seen almost edge-on, which
    /// cover few pixels.
    #[test]
    fn the_shaders_weights_match_the_reference() {
        let table = MaskingTable::committed();
        let errors = random_views(400)
            .into_iter()
            .map(|(g, v)| {
                let shader = as_array(visible_weights(&table, g, v).unwrap());
                worst(shader, as_array(reference_weights(g, v).unwrap()))
            })
            .collect();
        let (mean, p95, max) = spread(errors);
        assert!(mean < 0.005, "mean error {mean:.4}");
        assert!(p95 < 0.02, "95th percentile {p95:.4}");
        assert!(max < 0.25, "worst error {max:.4}");
    }

    /// How far the shader's weights are from the reference over more random
    /// views than a debug test can afford, for choosing the bounds above.
    #[test]
    #[ignore = "a measurement: prints the error distribution"]
    fn survey_the_shaders_error() {
        let table = MaskingTable::committed();
        let mut errors = Vec::new();
        for (g, v) in random_views(2000) {
            let shader = visible_weights(&table, g, v).unwrap();
            let reference = reference_weights(g, v).unwrap();
            let e = worst(as_array(shader), as_array(reference));
            if e > 0.05 {
                println!("g {g:?} view {v}: shader {shader:?} reference {reference:?}");
            }
            errors.push(e);
        }
        let (mean, p95, max) = spread(errors);
        println!("mean {mean:.4} p95 {p95:.4} max {max:.4}");
    }

    /// The test above can fail: the area-weighted mix, which leaves out what
    /// the hiding steps hide, fails every one of its bounds. It is the model
    /// this module started from.
    #[test]
    fn the_area_weighted_mix_fails_the_same_check() {
        let errors = random_views(400)
            .into_iter()
            .map(|(g, v)| {
                let (nx, nz) = riser_normals(g);
                let top = v.y;
                let x = g[0].abs() * nx.dot(v).max(0.0);
                let z = g[1].abs() * nz.dot(v).max(0.0);
                let sum = top + x + z;
                let naive = [(top / sum) as f64, (x / sum) as f64, (z / sum) as f64];
                worst(naive, as_array(reference_weights(g, v).unwrap()))
            })
            .collect();
        let (mean, p95, max) = spread(errors);
        assert!(
            mean > 0.005 && p95 > 0.02 && max > 0.25,
            "the naive mix passed a bound: mean {mean:.4}, p95 {p95:.4}, worst {max:.4}"
        );
    }

    #[test]
    fn weights_add_up_to_one_and_hidden_risers_get_none() {
        let table = MaskingTable::committed();
        for (g, v) in grid(&[1.0, 20.0, 70.0], 30) {
            let w = visible_weights(&table, g, v).unwrap();
            assert!(
                (w.top + w.x + w.z - 1.0).abs() < 1e-5,
                "g {g:?} view {v}: {w:?}"
            );
            let (nx, nz) = riser_normals(g);
            if nx.dot(v) <= 0.0 {
                assert_eq!(w.x, 0.0, "g {g:?} view {v}: x-risers face away");
            }
            if nz.dot(v) <= 0.0 {
                assert_eq!(w.z, 0.0, "g {g:?} view {v}: z-risers face away");
            }
        }
    }

    #[test]
    fn a_slope_facing_away_covers_nothing() {
        let table = MaskingTable::committed();
        // Rising toward +x at 45°, seen from +x and 10° up: its back.
        let v = view(10.0, 0.0);
        assert_eq!(visible_weights(&table, [1.0, 0.0], v), None);
        assert_eq!(reference_weights([1.0, 0.0], v), None);
    }

    /// §7's colour check: the shaded colour against rays that each take the
    /// lit colour of the face they meet, under several suns.
    #[test]
    fn the_colour_matches_counting_lit_faces() {
        let table = MaskingTable::committed();
        let materials = Materials {
            top: Vec3::new(0.35, 0.60, 0.20),
            side: Vec3::new(0.45, 0.33, 0.22),
        };
        for sun in [
            Vec3::new(0.4, 1.0, 0.3),
            Vec3::new(-0.8, 0.5, 0.1),
            Vec3::new(0.1, 0.3, -0.9),
        ] {
            let lighting = Lighting {
                sun_dir: sun.normalize(),
                ..Lighting::default()
            };
            for (g, v) in random_views(30) {
                let shaded = aggregate_colour(&table, g, v, &lighting, &materials).unwrap();
                let [top, x, z] = cast_rays(g, v, 10_000);
                let counted = lit(
                    FaceWeights {
                        top: top as f32,
                        x: x as f32,
                        z: z as f32,
                    },
                    g,
                    &lighting,
                    &materials,
                );
                let e = (shaded - counted).abs().max_element();
                assert!(
                    e < 0.04,
                    "sun {sun} g {g:?} view {v}: shaded {shaded}, counted {counted}"
                );
            }
        }
    }

    /// `face_light` is `mesh.wgsl`'s `fs_main`, worked by hand for the
    /// default sun: ambient mixed by `n.y`, plus the diffuse term.
    #[test]
    fn face_light_is_the_mesh_shaders_lighting() {
        let l = Lighting::default();
        let up = face_light(Vec3::Y, &l);
        assert!((up.x - (l.ambient_high + l.diffuse_weight * l.sun_dir.y)).abs() < 1e-6);
        let away = face_light(-l.sun_dir.with_y(0.0).normalize(), &l);
        let mid = (l.ambient_low + l.ambient_high) / 2.0;
        assert!(
            (away.x - mid).abs() < 1e-6,
            "a side facing away from the sun gets ambient only"
        );
    }

    /// The committed table is what `generate` makes. Rebuilding all of it
    /// takes seconds in release and minutes in a debug test, so this checks
    /// a spread of texels: every 997th, which lands on every axis position.
    /// One step of slack, for a last bit of trigonometry that rounds
    /// differently on another machine.
    #[test]
    fn the_committed_table_is_what_generate_makes() {
        let table = MaskingTable::committed();
        for index in (0..TABLE_SIZE.pow(4)).step_by(997) {
            let fresh = MaskingTable::texel(index);
            let committed = table.bytes()[index];
            assert!(
                fresh.abs_diff(committed) <= 1,
                "texel {index}: committed {committed}, generated {fresh}"
            );
        }
    }

    /// Writes `aggregate.table`. Run it after changing the reference or the
    /// table's layout:
    /// `cargo test --release -p cubara-render --lib write_the_table -- --ignored`
    #[test]
    #[ignore = "writes a file; run by hand when the table has to change"]
    fn write_the_table() {
        let table = MaskingTable::generate();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/aggregate.table");
        std::fs::write(path, table.bytes()).expect("write aggregate.table");
    }
}
