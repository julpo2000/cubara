//! The far terrain's GPU-free half (`docs/PROPOSAL_FAR_VIEW.md` §3.2).
//!
//! Beyond the voxel rings, the land is drawn as a height field. Which squares
//! of it -- *patches* -- are drawn around an eye, and what heights they hold,
//! are pure functions of the eye and the seed, and live here with their
//! tests. The renderer only ever sees the heights.
//!
//! A patch is a quadtree node: [`PATCH_QUADS`] quads of `2^level` blocks on a
//! side. Near the eye patches are small and their quads fine; far away they
//! are large and coarse, so that a quad stays about the same size *on screen*
//! wherever it is drawn -- the property the voxel rings have too, and the one
//! [`FarView::split_ratio`] sets.
//!
//! The heights are the terrain's own: [`WorldGen::surface_height`], averaged
//! over the ground each vertex stands for. The terrain is a height field with
//! caves beneath it (`PHASE1_ARCHITECTURE.md` §8.1), so from kilometres away
//! this is the whole of what can be seen -- and unlike a cube-shaped LOD node,
//! a height here is a number, not a cell boundary, so it does not flatten with
//! distance (`PROPOSAL_FAR_VIEW.md` §2A, table B).

use crate::WorldGen;

/// Quads along one side of a patch.
pub const PATCH_QUADS: usize = 32;

/// Heights along one side of a patch: one more than the quads.
pub const PATCH_VERTS: usize = PATCH_QUADS + 1;

/// One square of the far terrain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PatchKey {
    /// A quad is `2^level` blocks wide.
    pub level: u32,
    /// Position on this level's grid of patches.
    pub x: i32,
    pub z: i32,
}

impl PatchKey {
    pub const fn new(level: u32, x: i32, z: i32) -> Self {
        Self { level, x, z }
    }

    /// The width of one quad, in blocks.
    pub fn quad(self) -> i64 {
        1i64 << self.level
    }

    /// The width of the whole patch, in blocks.
    pub fn size(self) -> i64 {
        PATCH_QUADS as i64 * self.quad()
    }

    /// The patch's lowest `(x, z)` corner, in blocks.
    pub fn origin(self) -> [i64; 2] {
        [self.x as i64 * self.size(), self.z as i64 * self.size()]
    }

    /// The patch at `level` that holds the block column `(x, z)`.
    pub fn containing(level: u32, x: i64, z: i64) -> Self {
        let size = PATCH_QUADS as i64 * (1i64 << level);
        Self::new(level, x.div_euclid(size) as i32, z.div_euclid(size) as i32)
    }

    /// The four patches one level finer that tile this one.
    pub fn children(self) -> [PatchKey; 4] {
        let (level, x, z) = (self.level - 1, self.x * 2, self.z * 2);
        [
            Self::new(level, x, z),
            Self::new(level, x + 1, z),
            Self::new(level, x, z + 1),
            Self::new(level, x + 1, z + 1),
        ]
    }
}

/// A box the far terrain leaves to something else: the voxel rings, which
/// draw everything inside it themselves. Axis-aligned, around the eye's chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hole {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Hole {
    fn contains_box(&self, min: [f64; 3], max: [f64; 3]) -> bool {
        (0..3).all(|a| self.min[a] <= min[a] && max[a] <= self.max[a])
    }
}

/// Everything [`select`] needs: where the eye is, how finely to draw, how far,
/// and what not to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarView {
    /// The camera, in blocks.
    pub eye: [f64; 3],
    /// A patch is split into four while the eye is nearer to it than
    /// `split_ratio` times its width. A quad then never appears wider than
    /// `1 / (split_ratio * PATCH_QUADS)` radians -- see [`split_ratio_for`].
    pub split_ratio: f64,
    /// ...and while its own [`PatchHeights::error`] would appear taller than
    /// this, in radians: the *height* bound, which puts fine patches where
    /// the land is rough (mountains) rather than everywhere at that distance.
    pub max_error: f64,
    /// ...but never into quads narrower than this, in radians. Without a
    /// floor the height bound chases the terrain's one-block steps, which a
    /// smooth surface cannot follow to within a block, down to single columns
    /// wherever a pixel is about a block wide.
    pub min_quad: f64,
    /// How far the far terrain reaches, in blocks, measured across the ground.
    pub radius: f64,
    /// Left to the voxel rings.
    pub hole: Option<Hole>,
}

/// The `split_ratio` that keeps a quad at most `pixels` wide on a screen
/// `screen_height` pixels tall with a vertical field of view of `fov_y`
/// radians.
pub fn split_ratio_for(pixels: f64, screen_height: f64, fov_y: f64) -> f64 {
    let pixel = fov_y / screen_height;
    1.0 / (pixels * pixel * PATCH_QUADS as f64)
}

/// The lowest and highest surface the generator can produce -- the vertical
/// extent of a patch whose heights are not known yet.
pub fn surface_bounds() -> (f64, f64) {
    let (lo, hi) = WorldGen::surface_range();
    // `+ 1`: a surface block's top face is one above the block itself.
    (lo as f64, hi as f64 + 1.0)
}

/// The patches drawn around `view.eye`, each covering ground no other one
/// does.
///
/// A quadtree walk from roots big enough to reach `view.radius`. A patch is
/// dropped when it lies wholly beyond `radius` or wholly inside the hole. It
/// is split when the eye is within `split_ratio` of its width (measured to its
/// box, with the terrain's full height range as its vertical extent), or when
/// `error_of` knows its error and that error would appear taller than
/// `max_error`. Otherwise it is kept.
///
/// `error_of` answers only for patches already generated, so a caller streams
/// toward the answer: select, generate what is new, select again. A patch
/// whose error is not known yet is judged by distance alone.
pub fn select(view: &FarView, error_of: impl Fn(PatchKey) -> Option<f32>) -> Vec<PatchKey> {
    let root_level = root_level_for(view.radius);
    let root_size = (PATCH_QUADS as i64) << root_level;
    let reach = view.radius.ceil() as i64;
    let [ex, _, ez] = view.eye;
    let lo = |c: f64| ((c as i64) - reach).div_euclid(root_size);
    let hi = |c: f64| ((c as i64) + reach).div_euclid(root_size);
    let mut out = Vec::new();
    for x in lo(ex)..=hi(ex) {
        for z in lo(ez)..=hi(ez) {
            walk(
                view,
                &error_of,
                PatchKey::new(root_level, x as i32, z as i32),
                &mut out,
            );
        }
    }
    out
}

/// The coarsest level whose patches are at least `radius` wide, so a 3 x 3
/// block of them always covers the view.
fn root_level_for(radius: f64) -> u32 {
    let mut level = 0;
    while ((PATCH_QUADS as i64) << level) < radius.ceil() as i64 {
        level += 1;
    }
    level
}

fn walk(
    view: &FarView,
    error_of: &impl Fn(PatchKey) -> Option<f32>,
    key: PatchKey,
    out: &mut Vec<PatchKey>,
) {
    let (min, max) = patch_box(key);
    if ground_distance(view.eye, min, max) > view.radius {
        return;
    }
    if view.hole.is_some_and(|h| h.contains_box(min, max)) {
        return;
    }
    let d = distance(view.eye, min, max);
    let near = d < view.split_ratio * key.size() as f64;
    let children_wide_enough = (key.quad() as f64 / 2.0) >= view.min_quad * d;
    let rough =
        children_wide_enough && error_of(key).is_some_and(|e| e as f64 > view.max_error * d);
    if key.level > 0 && (near || rough) {
        for child in key.children() {
            walk(view, error_of, child, out);
        }
    } else {
        out.push(key);
    }
}

/// A patch's box: its square of ground, and every height the terrain can
/// reach.
fn patch_box(key: PatchKey) -> ([f64; 3], [f64; 3]) {
    let [ox, oz] = key.origin();
    let size = key.size() as f64;
    let (lo, hi) = surface_bounds();
    (
        [ox as f64, lo, oz as f64],
        [ox as f64 + size, hi, oz as f64 + size],
    )
}

/// From `p` to the nearest point of the box.
fn distance(p: [f64; 3], min: [f64; 3], max: [f64; 3]) -> f64 {
    (0..3)
        .map(|a| (min[a] - p[a]).max(p[a] - max[a]).max(0.0).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// The same, across the ground only.
fn ground_distance(p: [f64; 3], min: [f64; 3], max: [f64; 3]) -> f64 {
    [0, 2]
        .iter()
        .map(|&a| (min[a] - p[a]).max(p[a] - max[a]).max(0.0).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// A patch's heights, row by row (`z` outer, `x` inner), [`PATCH_VERTS`] on a
/// side: the top of the ground at each vertex, in blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchHeights {
    pub key: PatchKey,
    pub heights: Vec<f32>,
    pub min: f32,
    pub max: f32,
    /// How far, in blocks, this patch's surface is from the next finer one's,
    /// measured at the middle of every quad and quad edge -- where the finer
    /// patch has a vertex of its own. This is the 99th percentile of those
    /// gaps; [`error_max`](Self::error_max) is the worst. What [`select`]
    /// weighs against `max_error`. Zero at `level` 0.
    ///
    /// The 99th, not the 90th: a mountain is a few percent of a patch, and a
    /// 90th percentile let its worst columns reach 3.9 px while the patch
    /// looked fine (review of #284). Not the maximum either -- that splits
    /// to the floor almost everywhere (`PROPOSAL_FAR_VIEW.md` §3.2).
    pub error: f32,
    /// The worst of the same gaps.
    pub error_max: f32,
}

/// Samples per axis averaged into one vertex's height, at most.
const FOOTPRINT_SAMPLES: i64 = 4;

/// The heights of `key`, from the generator.
///
/// Each vertex stands for a quad's width of ground around it, and its height
/// is the mean top of that ground, sampled on a grid of at most
/// [`FOOTPRINT_SAMPLES`] a side. A single column would alias: at a 256-block
/// quad, one sample decides whether a vertex is on the peak or beside it, and
/// the answer changes as the patch moves. At `level` 0 a vertex is one
/// column, taken exactly.
pub fn generate(gen: &WorldGen, key: PatchKey) -> PatchHeights {
    let q = key.quad();
    let [ox, oz] = key.origin();
    let mut heights = Vec::with_capacity(PATCH_VERTS * PATCH_VERTS);
    for j in 0..PATCH_VERTS as i64 {
        for i in 0..PATCH_VERTS as i64 {
            heights.push(footprint_mean(gen, ox + i * q, oz + j * q, q));
        }
    }
    let min = heights.iter().copied().fold(f32::INFINITY, f32::min);
    let max = heights.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (error_max, error) = if q < 2 {
        (0.0, 0.0)
    } else {
        finer_gap(gen, &heights, ox, oz, q)
    };
    PatchHeights {
        key,
        heights,
        min,
        max,
        error,
        error_max,
    }
}

/// The mean top of the ground in a `q`-block square centred on `(x, z)`,
/// sampled on a grid of at most [`FOOTPRINT_SAMPLES`] a side.
fn footprint_mean(gen: &WorldGen, x: i64, z: i64, q: i64) -> f32 {
    let samples = q.clamp(1, FOOTPRINT_SAMPLES);
    let step = q.max(1) / samples;
    let offset = |s: i64| s * step + step / 2 - q / 2;
    let mut sum = 0i64;
    for a in 0..samples {
        for b in 0..samples {
            sum += gen.surface_height((x + offset(a)) as i32, (z + offset(b)) as i32) as i64;
        }
    }
    // `+ 1`: the top face of the surface block.
    sum as f32 / (samples * samples) as f32 + 1.0
}

/// The gaps between what this patch draws and the vertex the next finer
/// patch has there: at every vertex (where the finer one averages half the
/// ground), at the middle of every quad edge, and at the middle of every quad.
fn finer_gap(gen: &WorldGen, heights: &[f32], ox: i64, oz: i64, q: i64) -> (f32, f32) {
    let h = |i: usize, j: usize| heights[j * PATCH_VERTS + i];
    let half = q / 2;
    let mut gaps = Vec::with_capacity(3 * PATCH_VERTS * PATCH_VERTS);
    let mut check = |x: i64, z: i64, blend: f32| {
        gaps.push((footprint_mean(gen, x, z, half) - blend).abs());
    };
    for j in 0..PATCH_VERTS {
        for i in 0..PATCH_VERTS {
            let (x, z) = (ox + i as i64 * q, oz + j as i64 * q);
            // The finer patch has a vertex here too, averaged over half the
            // ground: a peak narrower than a quad shows up here, not between
            // vertices (found in review of #284 -- 3.9 times the p90 gap).
            check(x, z, h(i, j));
            if i < PATCH_QUADS {
                check(x + half, z, (h(i, j) + h(i + 1, j)) / 2.0);
            }
            if j < PATCH_QUADS {
                check(x, z + half, (h(i, j) + h(i, j + 1)) / 2.0);
            }
            if i < PATCH_QUADS && j < PATCH_QUADS {
                let centre = (h(i, j) + h(i + 1, j) + h(i, j + 1) + h(i + 1, j + 1)) / 4.0;
                check(x + half, z + half, centre);
            }
        }
    }
    gaps.sort_by(f32::total_cmp);
    (gaps[gaps.len() - 1], gaps[gaps.len() * 99 / 100])
}

/// Everything a one-shot caller needs -- a screenshot, the bench, a golden
/// test: select, generate what is new, select again, until nothing new is
/// asked for. The patches drawn, in [`select`]'s order.
///
/// On one thread. A live game streams instead, spreading the same loop over
/// frames and workers; a one-shot caller has one frame to be right in.
pub fn build(gen: &WorldGen, view: &FarView) -> Vec<PatchHeights> {
    let mut have: std::collections::HashMap<PatchKey, PatchHeights> =
        std::collections::HashMap::new();
    loop {
        let keys = select(view, |k| have.get(&k).map(|p| p.error));
        let new: Vec<PatchKey> = keys
            .iter()
            .copied()
            .filter(|k| !have.contains_key(k))
            .collect();
        if new.is_empty() {
            return keys.iter().filter_map(|k| have.remove(k)).collect();
        }
        for k in new {
            have.insert(k, generate(gen, k));
        }
    }
}

impl PatchHeights {
    /// How far the patch's skirt drops below its edge, in blocks: enough to
    /// cover the gap to a neighbour two levels coarser (a split for roughness
    /// can put one there), whose edge is a straight line across four of this
    /// patch's quads -- a slope of one block per block opens at most that --
    /// plus this patch's own worst gap to its finer self.
    pub fn skirt(&self) -> f32 {
        4.0 * self.key.quad() as f32 + self.error_max
    }

    /// The drawn surface at block column `(x, z)`: the bilinear blend of the
    /// four vertices around it, which is what the rasterizer draws between
    /// them (to within the diagonal each quad is split along).
    pub fn height_at(&self, x: f64, z: f64) -> f64 {
        let [ox, oz] = self.key.origin();
        let q = self.key.quad() as f64;
        let fx = ((x - ox as f64) / q).clamp(0.0, PATCH_QUADS as f64);
        let fz = ((z - oz as f64) / q).clamp(0.0, PATCH_QUADS as f64);
        let (i, j) = (
            (fx.floor() as usize).min(PATCH_QUADS - 1),
            (fz.floor() as usize).min(PATCH_QUADS - 1),
        );
        let (tx, tz) = (fx - i as f64, fz - j as f64);
        let h = |i: usize, j: usize| self.heights[j * PATCH_VERTS + i] as f64;
        let top = h(i, j) * (1.0 - tx) + h(i + 1, j) * tx;
        let bottom = h(i, j + 1) * (1.0 - tx) + h(i + 1, j + 1) * tx;
        top * (1.0 - tz) + bottom * tz
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    const SEED: u64 = 0x005E_ED00_00C0_FFEE;

    /// 1080p, the 60° field of view the camera uses, quads of 8 px.
    fn ratio() -> f64 {
        split_ratio_for(8.0, 1080.0, 60f64.to_radians())
    }

    fn view(eye: [f64; 3], radius: f64) -> FarView {
        FarView {
            eye,
            split_ratio: ratio(),
            max_error: PIXEL,
            min_quad: 2.0 * PIXEL,
            radius,
            hole: None,
        }
    }

    /// One pixel at 1080p with a 60° field of view, in radians.
    const PIXEL: f64 = std::f64::consts::FRAC_PI_3 / 1080.0;

    /// What a streaming caller does: select, generate what is new, select
    /// again, until nothing new is asked for.
    pub(super) fn converge(
        gen: &WorldGen,
        view: &FarView,
    ) -> (Vec<PatchKey>, HashMap<PatchKey, PatchHeights>) {
        let mut have: HashMap<PatchKey, PatchHeights> = HashMap::new();
        loop {
            let keys = select(view, |k| have.get(&k).map(|p| p.error));
            let new: Vec<PatchKey> = keys
                .iter()
                .copied()
                .filter(|k| !have.contains_key(k))
                .collect();
            if new.is_empty() {
                return (keys, have);
            }
            // A selection that never settles fails here rather than running
            // until somebody notices (a broken distance splits to level 0).
            assert!(
                have.len() < 20_000,
                "{} patches and still asking",
                have.len()
            );
            for k in new {
                have.insert(k, generate(gen, k));
            }
        }
    }

    /// Which selected patch covers each ground cell of `cell` blocks.
    fn coverage(keys: &[PatchKey], cell: i64, lo: i64, hi: i64) -> HashMap<(i64, i64), usize> {
        let mut map: HashMap<(i64, i64), usize> = HashMap::new();
        for k in keys {
            let [ox, oz] = k.origin();
            let s = k.size();
            let mut x = (ox.max(lo)).div_euclid(cell) * cell;
            while x < (ox + s).min(hi) {
                let mut z = (oz.max(lo)).div_euclid(cell) * cell;
                while z < (oz + s).min(hi) {
                    if x >= ox && z >= oz {
                        *map.entry((x, z)).or_default() += 1;
                    }
                    z += cell;
                }
                x += cell;
            }
        }
        map
    }

    #[test]
    fn a_ratio_gives_quads_of_the_asked_size() {
        // 8 px at 1080p / 60 degrees: a pixel is 0.97 mrad, a quad 7.8 mrad.
        let r = ratio();
        let quad_angle = 1.0 / (r * PATCH_QUADS as f64);
        let pixel = 60f64.to_radians() / 1080.0;
        assert!(
            (quad_angle / pixel - 8.0).abs() < 1e-9,
            "{}",
            quad_angle / pixel
        );
    }

    /// Every patch drawn, other than the finest, is at least `split_ratio`
    /// widths away -- so none of its quads looks wider than asked. This is
    /// the pixel bound the proposal's criterion rests on (§3.1).
    #[test]
    fn no_quad_is_drawn_wider_than_asked() {
        for eye in [[8.0, 40.0, 8.0], [8.0, 300.0, 8.0], [8.0, 3000.0, 8.0]] {
            let v = view(eye, 262_144.0);
            for key in select(&v, |_| None) {
                if key.level == 0 {
                    continue;
                }
                let (min, max) = patch_box(key);
                let d = distance(eye, min, max);
                let angle = key.quad() as f64 / d;
                let limit = 1.0 / (v.split_ratio * PATCH_QUADS as f64);
                assert!(
                    angle <= limit * (1.0 + 1e-9),
                    "{key:?} from {eye:?}: quad {angle} rad, limit {limit}"
                );
            }
        }
    }

    /// The patches tile the ground: every cell within the radius belongs to
    /// exactly one of them.
    #[test]
    fn the_patches_tile_the_ground_exactly_once() {
        let eye = [8.0, 300.0, 8.0];
        let radius = 20_000.0;
        let keys = select(&view(eye, radius), |_| None);
        // Cells of 512 blocks, out to where the view certainly reaches. A
        // multiple of the cell, so the cells checked are the cells counted.
        let reach = 12_288;
        let map = coverage(&keys, 512, -reach, reach);
        for x in (-reach..reach).step_by(512) {
            for z in (-reach..reach).step_by(512) {
                let n = map.get(&(x, z)).copied().unwrap_or(0);
                assert_eq!(n, 1, "cell ({x}, {z}) covered {n} times");
            }
        }
    }

    /// Neighbouring patches differ by at most one level, which is what lets
    /// a vertex on a shared edge blend toward its coarser neighbour without a
    /// crack (the geomorphing in `PROPOSAL_FAR_VIEW.md` §3.2).
    #[test]
    fn neighbours_differ_by_at_most_one_level() {
        // By distance alone. A split for roughness can put a fine patch next
        // to a coarse one two levels up; the skirts cover that edge, and the
        // error bound keeps what it hides under a pixel.
        let keys = select(&view([8.0, 40.0, 8.0], 60_000.0), |_| None);
        // Level of each 32-block cell along both axes through the eye, and a
        // diagonal line, sampled finely enough to see every boundary.
        let level_at = |x: i64, z: i64| {
            keys.iter()
                .find(|k| {
                    let [ox, oz] = k.origin();
                    (ox..ox + k.size()).contains(&x) && (oz..oz + k.size()).contains(&z)
                })
                .map(|k| i64::from(k.level))
        };
        for line in [(1, 0), (0, 1), (1, 1)] {
            let mut prev: Option<i64> = None;
            for t in (-50_000i64..50_000).step_by(32) {
                let here = level_at(t * line.0, t * line.1);
                if let (Some(a), Some(b)) = (prev, here) {
                    assert!(
                        (a - b).abs() <= 1,
                        "levels {a} and {b} meet at {t} on {line:?}"
                    );
                }
                prev = here;
            }
        }
    }

    #[test]
    fn nothing_is_selected_inside_the_hole() {
        let hole = Hole {
            min: [-1024.0, -512.0, -1024.0],
            max: [1024.0, 512.0, 1024.0],
        };
        let keys = select(
            &FarView {
                hole: Some(hole),
                ..view([8.0, 40.0, 8.0], 20_000.0)
            },
            |_| None,
        );
        assert!(!keys.is_empty());
        for k in &keys {
            let (min, max) = patch_box(*k);
            assert!(
                !hole.contains_box(min, max),
                "{k:?} lies wholly in the hole"
            );
        }
        // And something still covers the hole's edge, where the rings end.
        let map = coverage(&keys, 64, 960, 1088);
        assert!(map.contains_key(&(1024, 1024)), "a gap where the rings end");
    }

    /// From 3 km up the voxel rings reach nothing, and the far terrain is all
    /// there is below: it must reach right under the eye.
    #[test]
    fn from_high_up_the_ground_below_is_covered() {
        let keys = select(&view([8.0, 3000.0, 8.0], 262_144.0), |_| None);
        let under = keys.iter().any(|k| {
            let [ox, oz] = k.origin();
            (ox..ox + k.size()).contains(&8) && (oz..oz + k.size()).contains(&8)
        });
        assert!(under, "nothing covers the ground under the eye");
    }

    /// At level 0 a vertex is one column, exactly: the top face of its surface
    /// block. Every coarser level averages this, so an offset here is an
    /// offset everywhere -- one block, which the pixel tests below would call
    /// close enough at a kilometre.
    #[test]
    fn a_level_0_vertex_is_the_top_of_its_column() {
        let gen = WorldGen::new(SEED);
        for key in [PatchKey::new(0, 0, 0), PatchKey::new(0, -3, 7)] {
            let patch = generate(&gen, key);
            let [ox, oz] = key.origin();
            for (i, j) in [(0, 0), (5, 17), (PATCH_QUADS, PATCH_QUADS)] {
                let top = gen.surface_height((ox + i as i64) as i32, (oz + j as i64) as i32) + 1;
                assert_eq!(
                    patch.heights[j * PATCH_VERTS + i],
                    top as f32,
                    "{key:?} ({i}, {j})"
                );
            }
            assert_eq!(patch.heights.len(), (PATCH_QUADS + 1) * (PATCH_QUADS + 1));
        }
    }

    /// `error_max` is the worst gap, found independently: at every vertex,
    /// quad centre and edge midpoint, the finer footprint's mean against this
    /// patch's surface there. The skirt is sized from it, so a smaller number here is
    /// a seam that opens on the roughest edge.
    #[test]
    fn error_max_is_the_worst_gap_there_is() {
        let gen = WorldGen::new(SEED);
        // A patch in the mountains, where the gaps are uneven.
        let key = PatchKey::containing(5, -848, 4832);
        let p = generate(&gen, key);
        let [ox, oz] = key.origin();
        let q = key.quad();
        let h = |i: usize, j: usize| p.heights[j * PATCH_VERTS + i];
        let mut worst = 0f32;
        for j in 0..PATCH_VERTS {
            for i in 0..PATCH_VERTS {
                let (x, z) = (ox + i as i64 * q, oz + j as i64 * q);
                worst = worst.max((footprint_mean(&gen, x, z, q / 2) - h(i, j)).abs());
            }
        }
        for j in 0..PATCH_QUADS {
            for i in 0..PATCH_QUADS {
                let (x, z) = (ox + i as i64 * q, oz + j as i64 * q);
                let centre = (h(i, j) + h(i + 1, j) + h(i, j + 1) + h(i + 1, j + 1)) / 4.0;
                for (fx, fz, blend) in [
                    (x + q / 2, z, (h(i, j) + h(i + 1, j)) / 2.0),
                    (x, z + q / 2, (h(i, j) + h(i, j + 1)) / 2.0),
                    (x + q / 2, z + q / 2, centre),
                ] {
                    worst = worst.max((footprint_mean(&gen, fx, fz, q / 2) - blend).abs());
                }
            }
        }
        assert!(
            worst > 1.0,
            "a mountain patch with no gap to speak of: {worst}"
        );
        assert_eq!(p.error_max, worst);
        assert!(p.error <= p.error_max);
    }

    /// Two patches side by side share an edge, and must agree on it height
    /// for height, or the seam between them opens. At every level.
    #[test]
    fn neighbouring_patches_agree_along_their_shared_edge() {
        let gen = WorldGen::new(SEED);
        for level in [0, 3, 7] {
            let a = generate(&gen, PatchKey::new(level, 2, 5));
            let east = generate(&gen, PatchKey::new(level, 3, 5));
            let south = generate(&gen, PatchKey::new(level, 2, 6));
            for k in 0..PATCH_VERTS {
                let at = |p: &PatchHeights, i: usize, j: usize| p.heights[j * PATCH_VERTS + i];
                assert_eq!(
                    at(&a, PATCH_QUADS, k),
                    at(&east, 0, k),
                    "level {level}, east edge {k}"
                );
                assert_eq!(
                    at(&a, k, PATCH_QUADS),
                    at(&south, k, 0),
                    "level {level}, south edge {k}"
                );
            }
        }
    }

    /// Where the far terrain starts: the voxel rings draw everything nearer
    /// (`PROPOSAL_FAR_VIEW.md` §3.2), so no patch is ever seen from closer.
    const FAR_TERRAIN_STARTS: f64 = 1024.0;

    /// A patch whose error would show is split -- and stops splitting once
    /// its quads would get narrower than `min_quad`, however rough the land.
    #[test]
    fn rough_patches_are_split_down_to_the_floor_and_no_further() {
        let v = FarView {
            radius: 40_000.0,
            ..view([8.0, 300.0, 8.0], 40_000.0)
        };
        let flat = select(&v, |_| Some(0.0));
        let rough = select(&v, |_| Some(1.0e9));
        // Measured 5,300 against 1,690: rough land gets about three times the
        // patches, and the floor is what stops it at three rather than more.
        assert!(
            rough.len() > flat.len() * 2,
            "{} vs {}",
            rough.len(),
            flat.len()
        );
        for key in &rough {
            let (min, max) = patch_box(*key);
            let d = distance(v.eye, min, max);
            // Its children would have been narrower than the floor.
            assert!(
                key.level == 0 || (key.quad() as f64 / 2.0) < v.min_quad * d,
                "{key:?} could have been split and was not"
            );
            // And the floor is honoured by the patch itself.
            assert!(
                key.quad() as f64 >= v.min_quad * d / 2.0 || key.level == 0,
                "{key:?} split below the floor"
            );
        }
    }

    /// Per level, the far terrain's height against the ground a pixel covers,
    /// in pixels, at the nearest distance that level is drawn from: sampled
    /// over a grid of columns across flat land and mountains alike.
    fn height_error_px(gen: &WorldGen, view: &FarView) -> Vec<f64> {
        let (keys, have) = converge(gen, view);
        let mut errors = Vec::new();
        for sx in 0..24i64 {
            for sz in 0..24i64 {
                let (x, z) = (sx * 1579 - 18_000, sz * 1433 - 17_000);
                let Some(key) = keys.iter().find(|k| {
                    let [ox, oz] = k.origin();
                    (ox..ox + k.size()).contains(&x) && (oz..oz + k.size()).contains(&z)
                }) else {
                    continue;
                };
                let drawn = have[key].height_at(x as f64, z as f64);
                let d = {
                    let dx = x as f64 - view.eye[0];
                    let dz = z as f64 - view.eye[2];
                    (dx * dx + dz * dz).sqrt().max(FAR_TERRAIN_STARTS)
                };
                // The ground one pixel covers there, averaged.
                let foot = (d * PIXEL).max(1.0) as i64;
                let n = foot.min(8);
                let step = (foot / n).max(1);
                let mut sum = 0.0;
                for a in 0..n {
                    for b in 0..n {
                        sum += gen.surface_height((x + a * step) as i32, (z + b * step) as i32)
                            as f64
                            + 1.0;
                    }
                }
                errors.push((drawn - sum / (n * n) as f64).abs() / (d * PIXEL));
            }
        }
        errors.sort_by(f64::total_cmp);
        errors
    }

    #[test]
    #[ignore]
    fn print_error_per_config() {
        let gen = WorldGen::new(SEED);
        for (px, err) in [
            (32.0, f64::INFINITY),
            (32.0, 2.0),
            (16.0, f64::INFINITY),
            (16.0, 2.0),
            (16.0, 1.0),
            (8.0, f64::INFINITY),
            (8.0, 1.0),
            (4.0, f64::INFINITY),
        ] {
            let v = FarView {
                max_error: err * PIXEL,
                split_ratio: split_ratio_for(px, 1080.0, std::f64::consts::FRAC_PI_3),
                ..view([8.0, 300.0, 8.0], 25_000.0)
            };
            let e = height_error_px(&gen, &v);
            let mean = e.iter().sum::<f64>() / e.len() as f64;
            println!(
                "{px} px / {err} px: mean {mean:.2}, p90 {:.2}, p99 {:.2}, max {:.2}",
                e[e.len() * 9 / 10],
                e[e.len() * 99 / 100],
                e[e.len() - 1]
            );
        }
    }

    /// **The point of this module** (`PROPOSAL_FAR_VIEW.md` §2A, table B): a
    /// cube-shaped LOD node drew plain ground at y = 28 as 48-64 at 1-8 km --
    /// 20-36 blocks, up to 25 px -- and at y = 0 from 8 km. Here, with 16 px
    /// quads and a 1 px height bound, the drawn height is under a pixel from
    /// the ground it stands for in nine columns out of ten, out to 25 km in
    /// every direction, mountains included. With the shipped settings -- 2 px
    /// on each patch's 99th-percentile gap, quads no narrower than 4 px --
    /// measured: mean 0.39 px, p90 1.05, p99 2.72, worst 3.60.
    #[test]
    fn the_drawn_height_is_within_a_pixel_of_the_ground() {
        let gen = WorldGen::new(SEED);
        let v = FarView {
            max_error: 2.0 * PIXEL,
            min_quad: 4.0 * PIXEL,
            split_ratio: split_ratio_for(16.0, 1080.0, std::f64::consts::FRAC_PI_3),
            ..view([8.0, 300.0, 8.0], 25_000.0)
        };
        let e = height_error_px(&gen, &v);
        let mean = e.iter().sum::<f64>() / e.len() as f64;
        let p90 = e[e.len() * 9 / 10];
        assert!(e.len() > 400, "only {} columns sampled", e.len());
        let p99 = e[e.len() * 99 / 100];
        assert!(mean < 0.45, "mean {mean:.2} px");
        assert!(p90 < 1.2, "p90 {p90:.2} px");
        assert!(p99 < 3.0, "p99 {p99:.2} px");
    }

    /// And the height bound is what keeps it there: the same view with
    /// patches chosen by distance alone is measurably worse. Without this,
    /// the test above could pass with the error never consulted.
    #[test]
    fn the_height_bound_is_what_keeps_the_error_down() {
        let gen = WorldGen::new(SEED);
        let base = FarView {
            split_ratio: split_ratio_for(16.0, 1080.0, std::f64::consts::FRAC_PI_3),
            ..view([8.0, 300.0, 8.0], 25_000.0)
        };
        let bounded = height_error_px(
            &gen,
            &FarView {
                max_error: 2.0 * PIXEL,
                min_quad: 4.0 * PIXEL,
                ..base
            },
        );
        let unbounded = height_error_px(
            &gen,
            &FarView {
                max_error: f64::INFINITY,
                ..base
            },
        );
        let p90 = |e: &[f64]| e[e.len() * 9 / 10];
        // Measured 1.05 px against 1.58.
        assert!(
            p90(&unbounded) > p90(&bounded) * 1.4,
            "bounded p90 {:.2} px, unbounded {:.2} px",
            p90(&bounded),
            p90(&unbounded)
        );
    }
}

#[cfg(test)]
mod measure {
    use super::*;

    /// Measurement, not a check: what the three gate eyes select at 262 km,
    /// with the voxel rings' cube as the hole. How much of it is in view at
    /// once is the bench's to measure, with the real frustum (Rule 1 keeps
    /// trigonometry out of this crate).
    #[test]
    #[ignore]
    fn what_the_gate_eyes_select() {
        let gen = WorldGen::new(0x005E_ED00_00C0_FFEE);
        let pixel = std::f64::consts::FRAC_PI_3 / 1080.0;
        // (quad px by distance, height error px -- 0 for none)
        for (px, err) in [
            (8.0, 0.0),
            (16.0, 0.0),
            (16.0, 1.0),
            (16.0, 2.0),
            (8.0, 1.0),
        ] {
            for eye in [[8.0, 40.0, 8.0], [8.0, 300.0, 8.0], [8.0, 3000.0, 8.0]] {
                let view = FarView {
                    eye,
                    split_ratio: split_ratio_for(px, 1080.0, std::f64::consts::FRAC_PI_3),
                    max_error: if err > 0.0 {
                        err * pixel
                    } else {
                        f64::INFINITY
                    },
                    min_quad: 2.0 * pixel,
                    radius: 262_144.0,
                    hole: Some(Hole {
                        min: [-1024.0, eye[1] - 512.0, -1024.0],
                        max: [1040.0, eye[1] + 528.0, 1040.0],
                    }),
                };
                let (keys, have) = super::tests::converge(&gen, &view);
                let tris = |n: usize| (n * PATCH_QUADS * PATCH_QUADS * 2) as f64 / 1e6;
                println!(
                    "{px:>4} px, error {err} px, eye y {:>5}: {:>5} patches ({:.1}M tris); {} generated",
                    eye[1],
                    keys.len(),
                    tris(keys.len()),
                    have.len(),
                );
            }
        }
    }
}
