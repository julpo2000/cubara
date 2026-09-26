//! Streams the far terrain around the camera (`docs/PROPOSAL_FAR_VIEW.md`,
//! block F3c): which patches the view wants ([`cubara_world::far::select`]),
//! generating the new ones on worker threads, and swapping them into the
//! renderer's [`cubara_render::FarTerrain`] without leaving holes.
//!
//! The same rule the voxel nodes learned in #262 holds here: nothing is taken
//! away before what replaces it exists. A patch the view no longer wants stays
//! drawn while any patch covering the same ground is still being generated.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};

use cubara_render::{FarParams, FarPatch, FarSlot, Renderer};
use cubara_voxel::ChunkCoord;
use cubara_world::far::{self, FarView, Hole, PatchHeights, PatchKey};
use cubara_world::node;
use cubara_world::WorldGen;

/// How far the far terrain reaches, in blocks: the owner's "262 km first"
/// (2026-09-26, `PROPOSAL_FAR_VIEW.md`).
pub const FAR_VIEW_RADIUS: f64 = 262_144.0;

/// Quads no wider than this on screen, by distance alone.
const QUAD_PX: f64 = 16.0;
/// ...and split further where a patch's 99th-percentile gap to its finer self
/// would show more than this (`PROPOSAL_FAR_VIEW.md` §3.2).
const ERROR_PX: f64 = 2.0;
/// ...but never into quads narrower than this.
const FLOOR_PX: f64 = 4.0;
/// The camera's vertical field of view (`render.rs`'s projection).
const FOV_Y: f64 = std::f64::consts::FRAC_PI_3;

/// How far the eye moves before the selection is worked out again, in blocks.
/// The finest far patches are a few hundred blocks wide, so this is well
/// inside one of them.
const RESELECT_BLOCKS: f64 = 32.0;

/// The view the far terrain is selected for: the measured settings of
/// `PROPOSAL_FAR_VIEW.md` §3.2, scaled to a screen `screen_height` pixels
/// tall, leaving `hole` to the voxels.
pub fn far_view(eye: [f64; 3], screen_height: u32, hole: Option<Hole>) -> FarView {
    let pixel = FOV_Y / screen_height.max(1) as f64;
    FarView {
        eye,
        split_ratio: far::split_ratio_for(QUAD_PX, screen_height.max(1) as f64, FOV_Y),
        max_error: ERROR_PX * pixel,
        min_quad: FLOOR_PX * pixel,
        radius: FAR_VIEW_RADIUS,
        hole,
    }
}

/// The box the voxel nodes around `eye` draw themselves, in blocks
/// ([`node::covered_box_3d`] for the game's own schedule).
pub fn voxel_hole(eye: [f32; 3]) -> Hole {
    let (min, max) =
        node::covered_box_3d(ChunkCoord::from_world_pos(eye), node::DEFAULT_RING_SCHEDULE);
    let blocks = |c: [i32; 3]| c.map(|v| (v * 16) as f64);
    Hole {
        min: blocks(min),
        max: blocks(max),
    }
}

/// The renderer's form of a patch.
pub fn to_far_patch(p: &PatchHeights) -> FarPatch<'_> {
    let [ox, oz] = p.key.origin();
    FarPatch {
        origin: [ox as f32, oz as f32],
        quad: p.key.quad() as f32,
        heights: &p.heights,
        skirt: p.skirt(),
    }
}

/// The renderer's form of a hole.
pub fn to_far_params(hole: Hole, top_color: [f32; 3]) -> FarParams {
    FarParams {
        hole: Some((hole.min.map(|v| v as f32), hole.max.map(|v| v as f32))),
        top_color,
    }
}

/// The grass colour the far terrain is drawn in: the texture's own average.
pub fn far_top_color() -> [f32; 3] {
    cubara_render::materials::mean_color("grass_top").unwrap_or([0.2, 0.4, 0.15])
}

/// Every coarser patch holding `key`, up to `top_level`.
fn ancestors(key: PatchKey, top_level: u32) -> impl Iterator<Item = PatchKey> {
    (key.level + 1..=top_level).map(move |level| {
        let shift = level - key.level;
        PatchKey::new(level, key.x >> shift, key.z >> shift)
    })
}

/// The coarsest level a selection reaches: the roots [`far::select`] starts from.
fn top_level(keys: &[PatchKey]) -> u32 {
    keys.iter().map(|k| k.level).max().unwrap_or(0)
}

/// Which patches to draw: every wanted one that is ready, and -- while any
/// wanted one is still being generated -- whatever is drawn now over the same
/// ground, so the ground never goes missing in between (#262's rule).
fn draw_set(
    wanted: &[PatchKey],
    ready: &impl Fn(PatchKey) -> bool,
    drawn: &HashSet<PatchKey>,
) -> HashSet<PatchKey> {
    let top = top_level(wanted).max(drawn.iter().map(|k| k.level).max().unwrap_or(0));
    let mut out: HashSet<PatchKey> = wanted.iter().copied().filter(|k| ready(*k)).collect();
    let missing: HashSet<PatchKey> = wanted.iter().copied().filter(|k| !ready(*k)).collect();
    if missing.is_empty() {
        return out;
    }
    // A drawn patch covers missing ground if it is a missing patch's ancestor,
    // or has a missing patch among its own ancestors.
    let above_missing: HashSet<PatchKey> =
        missing.iter().flat_map(|&m| ancestors(m, top)).collect();
    for &old in drawn {
        if out.contains(&old) {
            continue;
        }
        if above_missing.contains(&old) || ancestors(old, top).any(|a| missing.contains(&a)) {
            out.insert(old);
        }
    }
    out
}

/// Which generated patches are still of use: what is drawn, what is wanted,
/// and what decides a split -- every ancestor of a wanted patch, since its
/// error is what split it. Drop an ancestor and the next selection no longer
/// knows why its ground was split, merges it back, and then splits it again
/// once the ancestor is regenerated: a selection that never settles (review
/// of #284).
fn to_keep(wanted: &[PatchKey], draw: HashSet<PatchKey>) -> HashSet<PatchKey> {
    let top = top_level(wanted);
    let mut keep = draw;
    for &k in wanted {
        keep.insert(k);
        keep.extend(ancestors(k, top));
    }
    keep
}

/// A pool of threads generating patches. Each job carries the generation it
/// was asked for in, so a result from before a reset (a new world) is dropped.
struct Pool {
    jobs: Sender<(u64, WorldGen, PatchKey)>,
    done: Receiver<(u64, PatchHeights)>,
    _workers: Vec<std::thread::JoinHandle<()>>,
}

impl Pool {
    fn new() -> Self {
        // Half the cores: the voxel mesher has the rest, and it is the one the
        // player is standing in.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get() / 2)
            .unwrap_or(1)
            .max(1);
        let (jobs, job_rx) = std::sync::mpsc::channel::<(u64, WorldGen, PatchKey)>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let _workers = (0..workers)
            .map(|_| {
                let jobs = std::sync::Arc::clone(&job_rx);
                let done = done_tx.clone();
                std::thread::Builder::new()
                    .name("cubara-far".into())
                    .spawn(move || loop {
                        let job = jobs.lock().expect("far job lock").recv();
                        let Ok((generation, gen, key)) = job else {
                            break;
                        };
                        if done.send((generation, far::generate(&gen, key))).is_err() {
                            break;
                        }
                    })
                    .expect("spawn far-terrain worker")
            })
            .collect();
        Self {
            jobs,
            done,
            _workers,
        }
    }
}

/// The far terrain's streaming state. One per window, beside the voxel
/// [`crate::streaming::NodeStreaming`].
pub struct FarStreaming {
    gen: WorldGen,
    seed: u64,
    generation: u64,
    pool: Pool,
    /// Every patch generated and still of use: the ones drawn, and the ones
    /// whose error decides whether their area is split.
    heights: HashMap<PatchKey, PatchHeights>,
    in_flight: HashSet<PatchKey>,
    slots: HashMap<PatchKey, FarSlot>,
    /// Where the eye was when the selection was last worked out.
    selected_at: Option<[f64; 3]>,
    /// New patches arrived since then.
    stale: bool,
    top_color: [f32; 3],
    /// Said once when the renderer runs out of slots, not every frame.
    warned_full: bool,
}

impl FarStreaming {
    pub fn new(seed: u64) -> Self {
        Self {
            gen: WorldGen::new(seed),
            seed,
            generation: 0,
            pool: Pool::new(),
            heights: HashMap::new(),
            in_flight: HashSet::new(),
            slots: HashMap::new(),
            selected_at: None,
            stale: false,
            top_color: far_top_color(),
            warned_full: false,
        }
    }

    /// The seed the far terrain is generated from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// A different world -- New World, a load, joining a server: drop every
    /// patch, drawn or generating, and start again from `seed`.
    pub fn reset(&mut self, seed: u64, renderer: &mut Renderer) {
        for (_, slot) in self.slots.drain() {
            renderer.far_remove(slot);
        }
        self.heights.clear();
        self.in_flight.clear();
        self.generation += 1;
        self.gen = WorldGen::new(seed);
        self.seed = seed;
        self.selected_at = None;
        self.stale = false;
    }

    /// Bring the far terrain in line with the camera at `eye`: take in what
    /// has been generated, work the selection out again if the eye has moved
    /// or new patches arrived, and ask for what is missing.
    pub fn update(&mut self, renderer: &mut Renderer, eye: [f32; 3]) {
        while let Ok((generation, patch)) = self.pool.done.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.in_flight.remove(&patch.key);
            self.heights.insert(patch.key, patch);
            self.stale = true;
        }
        let eye64 = eye.map(|v| v as f64);
        let moved = self.selected_at.is_none_or(|at| {
            let d = (0..3)
                .map(|a| (at[a] - eye64[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            d > RESELECT_BLOCKS
        });
        let hole = voxel_hole(eye);
        renderer.set_far_params(to_far_params(hole, self.top_color));
        if !(moved || self.stale) {
            return;
        }
        self.selected_at = Some(eye64);
        self.stale = false;

        let view = far_view(eye64, renderer.size().1, Some(hole));
        let wanted = far::select(&view, |k| self.heights.get(&k).map(|p| p.error));

        // Ask for what is missing, nearest first: the ground under the eye
        // matters more than the horizon.
        let mut missing: Vec<PatchKey> = wanted
            .iter()
            .copied()
            .filter(|k| !self.heights.contains_key(k) && !self.in_flight.contains(k))
            .collect();
        missing.sort_by_key(|k| k.level);
        for key in missing {
            self.in_flight.insert(key);
            let _ = self.pool.jobs.send((self.generation, self.gen, key));
        }

        // Swap the drawn set, never leaving ground uncovered.
        let drawn: HashSet<PatchKey> = self.slots.keys().copied().collect();
        let draw = draw_set(&wanted, &|k| self.heights.contains_key(&k), &drawn);
        for key in drawn.difference(&draw) {
            if let Some(slot) = self.slots.remove(key) {
                renderer.far_remove(slot);
            }
        }
        for key in &draw {
            if self.slots.contains_key(key) {
                continue;
            }
            let Some(patch) = self.heights.get(key) else {
                continue;
            };
            match renderer.far_insert(to_far_patch(patch)) {
                Some(slot) => {
                    self.slots.insert(*key, slot);
                }
                None if !self.warned_full => {
                    log::warn!(
                        "far terrain: every slot is taken -- some distant ground is not drawn"
                    );
                    self.warned_full = true;
                }
                None => {}
            }
        }

        let keep = to_keep(&wanted, draw);
        self.heights.retain(|k, _| keep.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_climb_one_level_at_a_time() {
        let a: Vec<_> = ancestors(PatchKey::new(2, -5, 9), 5).collect();
        assert_eq!(
            a,
            vec![
                PatchKey::new(3, -3, 4),
                PatchKey::new(4, -2, 2),
                PatchKey::new(5, -1, 1),
            ]
        );
    }

    /// The camera came closer: a coarse patch is split into four children, and
    /// only two are generated yet. The parent must stay drawn until all four
    /// are -- dropping it would leave half its ground missing.
    #[test]
    fn a_coarse_patch_stays_until_every_child_has_arrived() {
        let parent = PatchKey::new(4, 2, 2);
        let kids = parent.children();
        let drawn: HashSet<_> = [parent].into();
        let ready = |k: PatchKey| k == kids[0] || k == kids[1];
        let draw = draw_set(&kids, &ready, &drawn);
        assert!(
            draw.contains(&parent),
            "the parent left before its children arrived"
        );
        assert!(draw.contains(&kids[0]) && draw.contains(&kids[1]));
        assert!(
            !draw.contains(&kids[2]),
            "a patch with no heights was drawn"
        );

        let all = |_: PatchKey| true;
        let draw = draw_set(&kids, &all, &drawn);
        assert!(
            !draw.contains(&parent),
            "the parent stayed after it was replaced"
        );
    }

    /// The camera moved away: four children merge into their parent, which is
    /// not generated yet. The children stay until it is.
    #[test]
    fn fine_patches_stay_until_the_coarse_one_replacing_them_has_arrived() {
        let parent = PatchKey::new(4, 2, 2);
        let drawn: HashSet<_> = parent.children().into_iter().collect();
        let draw = draw_set(&[parent], &|_| false, &drawn);
        assert_eq!(
            draw, drawn,
            "ground went missing while its replacement generated"
        );
        let draw = draw_set(&[parent], &|_| true, &drawn);
        assert_eq!(draw, [parent].into());
    }

    #[test]
    fn a_patch_nothing_replaces_leaves_at_once() {
        let far_away = PatchKey::new(4, 40, 40);
        let wanted = [PatchKey::new(4, 2, 2)];
        let draw = draw_set(&wanted, &|_| false, &[far_away].into());
        assert!(!draw.contains(&far_away));
    }

    /// The hole is the voxel nodes' own box, in blocks -- the far terrain must
    /// neither draw over them nor leave a gap at their edge.
    #[test]
    fn the_hole_is_the_voxel_region_in_blocks() {
        let hole = voxel_hole([8.0, 300.0, 8.0]);
        let (min, max) =
            node::covered_box_3d(ChunkCoord::new(0, 18, 0), node::DEFAULT_RING_SCHEDULE);
        assert_eq!(hole.min, min.map(|c| (c * 16) as f64));
        assert_eq!(hole.max, max.map(|c| (c * 16) as f64));
        assert!(hole.min[0] < -1000.0 && hole.max[0] > 1000.0, "{hole:?}");
    }

    /// The same numbers the far terrain module measured (`far.rs`, §3.2).
    #[test]
    fn the_view_uses_the_measured_settings() {
        let v = far_view([0.0; 3], 1080, None);
        let pixel = FOV_Y / 1080.0;
        assert!((v.max_error / pixel - ERROR_PX).abs() < 1e-9);
        assert!((v.min_quad / pixel - FLOOR_PX).abs() < 1e-9);
        let quad_angle = 1.0 / (v.split_ratio * far::PATCH_QUADS as f64);
        assert!((quad_angle / pixel - QUAD_PX).abs() < 1e-6);
        assert_eq!(v.radius, FAR_VIEW_RADIUS);
    }

    /// What is kept between selections is enough to make the same selection
    /// again -- and keeping less is not, which is what makes this a test.
    ///
    /// The cache also holds everything generated around a second eye far
    /// away, as it would after the player moved: that is what the rule has
    /// to drop, and what it must not drop with it.
    #[test]
    fn keeping_the_ancestors_keeps_the_selection_still() {
        let gen = WorldGen::new(0x005E_ED00_00C0_FFEE);
        let view_at = |eye: [f64; 3]| FarView {
            radius: 20_000.0,
            ..far_view(eye, 720, Some(voxel_hole(eye.map(|v| v as f32))))
        };
        let mut heights: HashMap<PatchKey, PatchHeights> = HashMap::new();
        let converge = |view: &FarView, heights: &mut HashMap<PatchKey, PatchHeights>| loop {
            let wanted = far::select(view, |k| heights.get(&k).map(|p| p.error));
            let new: Vec<_> = wanted
                .iter()
                .filter(|k| !heights.contains_key(k))
                .copied()
                .collect();
            if new.is_empty() {
                return wanted;
            }
            for k in new {
                heights.insert(k, far::generate(&gen, k));
            }
        };
        converge(&view_at([60_000.0, 300.0, 60_000.0]), &mut heights);
        let here = view_at([8.0, 300.0, 8.0]);
        let wanted = converge(&here, &mut heights);
        let select = |h: &HashMap<PatchKey, PatchHeights>| {
            far::select(&here, |k| h.get(&k).map(|p| p.error))
        };

        let keep = to_keep(&wanted, wanted.iter().copied().collect());
        let mut kept = heights.clone();
        kept.retain(|k, _| keep.contains(k));
        assert!(
            kept.len() < heights.len(),
            "nothing was dropped, so nothing was tested"
        );
        assert_eq!(
            select(&kept),
            wanted,
            "the selection moved with only the kept patches"
        );

        let mut bare = heights;
        bare.retain(|k, _| wanted.contains(k));
        assert_ne!(
            select(&bare),
            wanted,
            "without the ancestors it should have moved"
        );
    }

    /// Fog ends at the far terrain's edge, not past it: past it, the edge of
    /// the world would be as hard as it was before fog existed.
    #[test]
    fn fog_finishes_inside_the_far_terrain() {
        let (_, fog_end) = cubara_render::Lighting::fog_range(FAR_VIEW_RADIUS as f32);
        assert!(fog_end < FAR_VIEW_RADIUS as f32 && fog_end > 0.5 * FAR_VIEW_RADIUS as f32);
    }

    #[test]
    fn the_renderer_and_the_world_agree_on_the_patch_size() {
        assert_eq!(cubara_render::FAR_QUADS, far::PATCH_QUADS);
        assert_eq!(cubara_render::FAR_VERTS, far::PATCH_VERTS);
    }
}
