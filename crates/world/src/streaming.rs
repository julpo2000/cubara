//! Chunk streaming policy.
//!
//! Pure book-keeping: given where the camera is and how far it can see, decide
//! which chunks *should* be resident, and what to load/unload to get an existing
//! resident set there. No generation, no GPU — the renderer drives the actual work
//! from these plans, which keeps the decision testable in isolation.

use std::collections::HashSet;

use cubara_voxel::ChunkCoord;

/// The load/unload deltas needed to turn a resident set into the desired one.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StreamUpdates {
    /// Chunks to generate/mesh/upload (desired but not yet resident).
    pub to_load: Vec<ChunkCoord>,
    /// Chunks whose GPU data can be dropped (resident but no longer desired).
    pub to_unload: Vec<ChunkCoord>,
}

/// Every chunk coordinate within `radius` chunks (square/Chebyshev) horizontally of
/// `center`, across the vertical band `y_range`. Square rather than circular keeps
/// the policy trivial; frustum culling already trims what falls off-screen.
///
/// Only `center.x`/`center.z` are used — the vertical extent comes from `y_range`,
/// because the world is far thinner vertically than it is wide.
pub fn desired_chunks(
    center: ChunkCoord,
    radius: i32,
    y_range: std::ops::RangeInclusive<i32>,
) -> Vec<ChunkCoord> {
    let radius = radius.max(0);
    let mut out = Vec::new();
    for y in y_range {
        for z in (center.z - radius)..=(center.z + radius) {
            for x in (center.x - radius)..=(center.x + radius) {
                out.push(ChunkCoord::new(x, y, z));
            }
        }
    }
    out
}

/// Compute the [`StreamUpdates`] that move `resident` to exactly the set desired
/// around `center`. `to_load` is returned nearest-first so the chunks around the
/// camera stream in before the fringe.
pub fn plan_updates(
    resident: &HashSet<ChunkCoord>,
    center: ChunkCoord,
    radius: i32,
    y_range: std::ops::RangeInclusive<i32>,
) -> StreamUpdates {
    let desired = desired_chunks(center, radius, y_range);
    let desired_set: HashSet<ChunkCoord> = desired.iter().copied().collect();

    let mut to_load: Vec<ChunkCoord> = desired
        .into_iter()
        .filter(|c| !resident.contains(c))
        .collect();
    to_load.sort_by_key(|c| horizontal_dist_sq(*c, center));

    let to_unload: Vec<ChunkCoord> = resident
        .iter()
        .filter(|c| !desired_set.contains(c))
        .copied()
        .collect();

    StreamUpdates { to_load, to_unload }
}

/// Squared horizontal distance between two chunk coords (vertical ignored).
fn horizontal_dist_sq(a: ChunkCoord, b: ChunkCoord) -> i64 {
    let dx = (a.x - b.x) as i64;
    let dz = (a.z - b.z) as i64;
    dx * dx + dz * dz
}

/// Chunks per LOD ring: every `RING` chunks of horizontal distance from the camera
/// drops the detail one level. Tuned so the nearest few rings stay full-res.
const RING: i32 = 3;
/// Coarsest LOD we ever pick (caps at `Chunk::SIZE`'s log2 anyway, but this keeps
/// distant terrain from collapsing to single blocks too early).
const MAX_LOD: u32 = 4;

/// The LOD level to mesh the chunk at `coord` at, given the camera is in `center`:
/// full detail (0) nearby, one level coarser every [`RING`] chunks of horizontal
/// (Chebyshev) distance out, capped at [`MAX_LOD`]. This is the policy that turns
/// the [`build_mesh_lod`](cubara_voxel::Chunk::build_mesh_lod) primitive into a
/// render-distance win: distant chunks cost a fraction of the triangles.
pub fn lod_for(coord: ChunkCoord, center: ChunkCoord) -> u32 {
    let dist = (coord.x - center.x).abs().max((coord.z - center.z).abs());
    ((dist / RING) as u32).min(MAX_LOD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(coords: &[ChunkCoord]) -> HashSet<ChunkCoord> {
        coords.iter().copied().collect()
    }

    #[test]
    fn desired_count_is_square_times_band() {
        // (2r+1)^2 columns * number of y layers.
        let d = desired_chunks(ChunkCoord::new(0, 0, 0), 2, 0..=1);
        assert_eq!(d.len(), 5 * 5 * 2);
        // No duplicates.
        assert_eq!(set(&d).len(), d.len());
    }

    #[test]
    fn radius_zero_is_just_the_column() {
        let d = desired_chunks(ChunkCoord::new(3, 0, -4), 0, 0..=2);
        assert_eq!(
            d,
            vec![
                ChunkCoord::new(3, 0, -4),
                ChunkCoord::new(3, 1, -4),
                ChunkCoord::new(3, 2, -4),
            ]
        );
    }

    #[test]
    fn from_empty_everything_loads_nothing_unloads() {
        let updates = plan_updates(&HashSet::new(), ChunkCoord::new(0, 0, 0), 1, 0..=0);
        assert_eq!(updates.to_load.len(), 9);
        assert!(updates.to_unload.is_empty());
        // Nearest-first: the center chunk is loaded before its neighbours.
        assert_eq!(updates.to_load[0], ChunkCoord::new(0, 0, 0));
    }

    #[test]
    fn stepping_sideways_loads_leading_and_unloads_trailing_column() {
        // Start fully resident around x=0, then move the center to x=1.
        let resident = set(&desired_chunks(ChunkCoord::new(0, 0, 0), 1, 0..=0));
        let updates = plan_updates(&resident, ChunkCoord::new(1, 0, 0), 1, 0..=0);

        // A single-chunk sideways step swaps exactly one column (3 chunks) each way.
        assert_eq!(updates.to_load.len(), 3);
        assert_eq!(updates.to_unload.len(), 3);
        assert!(updates.to_load.iter().all(|c| c.x == 2));
        assert!(updates.to_unload.iter().all(|c| c.x == -1));
    }

    #[test]
    fn lod_rises_with_distance_and_caps() {
        let c = ChunkCoord::new(0, 0, 0);
        // Nearest RING chunks are full detail, then one level coarser per RING.
        assert_eq!(lod_for(ChunkCoord::new(0, 0, 0), c), 0);
        assert_eq!(lod_for(ChunkCoord::new(2, 0, 0), c), 0);
        assert_eq!(lod_for(ChunkCoord::new(3, 0, 0), c), 1);
        assert_eq!(lod_for(ChunkCoord::new(0, 0, 7), c), 2);
        // Chebyshev distance: the larger of |dx|,|dz| decides.
        assert_eq!(lod_for(ChunkCoord::new(1, 0, 6), c), 2);
        // Caps at MAX_LOD however far out.
        assert_eq!(lod_for(ChunkCoord::new(1000, 0, 0), c), 4);
    }

    #[test]
    fn staying_put_is_a_no_op() {
        let resident = set(&desired_chunks(ChunkCoord::new(5, 0, 5), 2, 0..=1));
        let updates = plan_updates(&resident, ChunkCoord::new(5, 0, 5), 2, 0..=1);
        assert_eq!(updates, StreamUpdates::default());
    }
}
