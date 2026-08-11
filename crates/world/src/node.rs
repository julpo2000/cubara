//! LOD node addressing and node-based streaming policy
//! (`docs/PHASE1_ARCHITECTURE.md` §6.1, §6.3, issue #38 sub-issue #105).
//!
//! An LOD node is one mesh, one arena allocation, and one draw, covering
//! `2^level × 2^level × 2^level` chunks — level 0 is a single chunk, level 3
//! covers 512 chunks in one draw. That is where the ~25× draw-count
//! reduction radius 64 needs comes from (§2: draws, not triangles, are the
//! binding constraint; today's per-chunk LOD only reduces triangles while
//! still issuing one draw per chunk).
//!
//! This module is addressing and streaming policy only — pure book-keeping,
//! no generation, no meshing, no GPU, mirroring [`crate::streaming`]'s own
//! shape exactly. It does not yet drive the live renderer: the existing
//! chunk-based [`crate::streaming::desired_chunks`]/`lod_for`/`plan_updates`
//! keep doing that until a later sub-issue in the #38 arc supersedes them.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use cubara_voxel::{Chunk, ChunkCoord};

/// The address of one LOD node: `level` (0 = one chunk, doubling in extent
/// each level up) and `pos`, the node's own position on *that level's*
/// grid — not a chunk coordinate. A level-`L` node's chunk-space footprint
/// is `pos * 2^L .. pos*2^L + 2^L` on each axis.
///
/// `Ord` is derived (fields compared in declaration order: `level` then
/// `pos`) — total and fixed, which is what node meshing (a later sub-issue)
/// needs for a merge order that never depends on which worker finishes
/// first (issue #83).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeKey {
    pub level: u32,
    pub pos: [i32; 3],
}

impl NodeKey {
    pub const fn new(level: u32, pos: [i32; 3]) -> Self {
        Self { level, pos }
    }

    /// How many chunks this node spans on each axis: `2^level`.
    pub fn extent_chunks(self) -> i32 {
        1 << self.level
    }

    /// The node's minimum-corner chunk.
    pub fn chunk_origin(self) -> ChunkCoord {
        let extent = self.extent_chunks();
        ChunkCoord::new(
            self.pos[0] * extent,
            self.pos[1] * extent,
            self.pos[2] * extent,
        )
    }

    /// The node's minimum-corner world-block coordinate — what
    /// `WorldGen::generate`'s `origin` parameter wants (a later sub-issue).
    pub fn world_origin(self) -> [i32; 3] {
        let size = Chunk::SIZE as i32;
        let c = self.chunk_origin();
        [c.x * size, c.y * size, c.z * size]
    }

    /// The node at `level` that contains `chunk` — floor division
    /// (`div_euclid`, correct for negative coordinates) by `2^level`, the
    /// same convention `ChunkCoord::from_block`/`cubara_world::region`
    /// already established for block 1.9's save/load.
    pub fn containing(chunk: ChunkCoord, level: u32) -> Self {
        let extent = 1i32 << level;
        Self {
            level,
            pos: [
                chunk.x.div_euclid(extent),
                chunk.y.div_euclid(extent),
                chunk.z.div_euclid(extent),
            ],
        }
    }
}

/// The load/unload deltas needed to turn a resident node set into the
/// desired one — the node-keyed equivalent of
/// [`crate::streaming::StreamUpdates`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NodeStreamUpdates {
    pub to_load: Vec<NodeKey>,
    pub to_unload: Vec<NodeKey>,
}

/// A `(level, outer_radius_in_chunks)` ring: everything out to
/// `outer_radius_in_chunks` (Chebyshev, horizontal) is at least this
/// detailed. Radii must increase with each successive entry; levels need
/// not be contiguous, though every current use is (§6.3's example: level 0
/// to 8, level 1 to 16, level 2 to 32, level 3 to 64).
pub type RingSchedule = [(u32, i32)];

/// §6.3's own illustrative starting table. **Not yet tuned** — issue #38's
/// sub-issue #109 is where these numbers get measured against the real
/// `<2,000`-drawn-nodes-at-radius-64 budget (§2); this table only needs to
/// exercise the ring mechanism correctly.
pub const DEFAULT_RING_SCHEDULE: &[(u32, i32)] = &[(0, 8), (1, 16), (2, 32), (3, 64)];

/// Every node that should be resident around `center`, across the vertical
/// band `y_range` (in chunks, same convention as
/// [`crate::streaming::desired_chunks`]'s own `y_range`), per `schedule`.
///
/// Each ring is resolved on **its own level's node grid**, snapped to a
/// center node via [`NodeKey::containing`] — not by testing a node's raw
/// chunk-space footprint against the ring boundary directly. A moving
/// `center` is essentially never grid-aligned to every level's `2^L`
/// spacing at once, so a footprint-based test would let some nodes straddle
/// a ring boundary; resolving in node-grid units keeps each level's own
/// node set simple and self-consistent, at the cost of ring boundaries
/// being approximate (within about one node width) rather than an exact
/// chunk-radius circle — acceptable here since `schedule` is a hand-tuned
/// constant table (§6.3), not a promise of a precise geometric radius.
pub fn desired_nodes(
    center: ChunkCoord,
    y_range: RangeInclusive<i32>,
    schedule: &RingSchedule,
) -> Vec<NodeKey> {
    let mut nodes = Vec::new();
    // `None` until the first ring has run: there is no "already covered by
    // a finer ring" yet, so the very first ring (typically level 0) must
    // exclude nothing, including the center node itself at distance 0 --
    // not the same thing as "covered out to radius 0."
    let mut inner_radius_chunks: Option<i32> = None;

    for &(level, outer_radius_chunks) in schedule {
        let extent = 1i32 << level;
        let center_node = NodeKey::containing(center, level);
        // Non-negative `i32::div_ceil` isn't stable on this toolchain --
        // `outer_radius_chunks` is always >= 0 by construction, so the
        // plain unsigned-style formula is safe here.
        let outer_node_radius = ((outer_radius_chunks + extent - 1) / extent).max(0);
        // The previous (finer) ring's coverage, expressed in *this* level's
        // node units -- nodes within it are already resident at finer
        // detail and are skipped here.
        let inner_node_radius = inner_radius_chunks.map(|r| r / extent);

        let y_min_node = (*y_range.start()).div_euclid(extent);
        let y_max_node = (*y_range.end()).div_euclid(extent);

        for nx in
            (center_node.pos[0] - outer_node_radius)..=(center_node.pos[0] + outer_node_radius)
        {
            for nz in
                (center_node.pos[2] - outer_node_radius)..=(center_node.pos[2] + outer_node_radius)
            {
                let dist = (nx - center_node.pos[0])
                    .abs()
                    .max((nz - center_node.pos[2]).abs());
                if inner_node_radius.is_some_and(|inner| dist <= inner) {
                    continue; // already covered by a finer, inner ring
                }
                for ny in y_min_node..=y_max_node {
                    nodes.push(NodeKey::new(level, [nx, ny, nz]));
                }
            }
        }

        inner_radius_chunks = Some(outer_radius_chunks);
    }

    nodes
}

/// Compute the [`NodeStreamUpdates`] that move `resident` to exactly the
/// set [`desired_nodes`] wants around `center`. `to_load` is nearest-first
/// (Chebyshev, in that node's own level-scaled units) so nodes around the
/// camera stream in before the fringe -- mirrors
/// [`crate::streaming::plan_updates`] exactly.
pub fn plan_node_updates(
    resident: &HashSet<NodeKey>,
    center: ChunkCoord,
    y_range: RangeInclusive<i32>,
    schedule: &RingSchedule,
) -> NodeStreamUpdates {
    let desired = desired_nodes(center, y_range, schedule);
    let desired_set: HashSet<NodeKey> = desired.iter().copied().collect();

    let mut to_load: Vec<NodeKey> = desired
        .into_iter()
        .filter(|n| !resident.contains(n))
        .collect();
    to_load.sort_by_key(|n| node_dist_sq(*n, center));

    let to_unload: Vec<NodeKey> = resident
        .iter()
        .filter(|n| !desired_set.contains(n))
        .copied()
        .collect();

    NodeStreamUpdates { to_load, to_unload }
}

/// Squared horizontal chunk-space distance from a node's own center to the
/// center of `center` (the player's chunk, not its minimum corner) -- the
/// sort key [`plan_node_updates`] loads nearest-first by. Both centers are
/// computed in the same doubled-integer units (`origin*2 + extent`, so a
/// half-chunk offset stays exact without floats); comparing a node's real
/// center against `center`'s own *corner* instead would make the two chunks
/// straddling that corner tie in distance and sort arbitrarily.
fn node_dist_sq(node: NodeKey, center: ChunkCoord) -> i64 {
    let extent = node.extent_chunks() as i64;
    let origin = node.chunk_origin();
    let node_center_x = origin.x as i64 * 2 + extent;
    let node_center_z = origin.z as i64 * 2 + extent;
    let center_x = center.x as i64 * 2 + 1;
    let center_z = center.z as i64 * 2 + 1;
    let dx = node_center_x - center_x;
    let dz = node_center_z - center_z;
    dx * dx + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extent_chunks_doubles_per_level() {
        assert_eq!(NodeKey::new(0, [0, 0, 0]).extent_chunks(), 1);
        assert_eq!(NodeKey::new(1, [0, 0, 0]).extent_chunks(), 2);
        assert_eq!(NodeKey::new(3, [0, 0, 0]).extent_chunks(), 8);
    }

    #[test]
    fn chunk_origin_scales_pos_by_extent() {
        assert_eq!(
            NodeKey::new(2, [1, -1, 3]).chunk_origin(),
            ChunkCoord::new(4, -4, 12)
        );
        assert_eq!(
            NodeKey::new(0, [5, 6, 7]).chunk_origin(),
            ChunkCoord::new(5, 6, 7),
            "level 0 is exactly a chunk coordinate"
        );
    }

    #[test]
    fn world_origin_scales_by_chunk_size_too() {
        assert_eq!(
            NodeKey::new(1, [1, 0, -1]).world_origin(),
            [32, 0, -32] // chunk (2, 0, -2) * 16
        );
    }

    #[test]
    fn containing_floors_toward_negative_infinity() {
        // Level 1 (extent 2): chunk -1 belongs to node covering chunks [-2, -1].
        assert_eq!(
            NodeKey::containing(ChunkCoord::new(-1, 0, 0), 1),
            NodeKey::new(1, [-1, 0, 0])
        );
        // Chunk -2 starts a new node.
        assert_eq!(
            NodeKey::containing(ChunkCoord::new(-2, 0, 0), 1),
            NodeKey::new(1, [-1, 0, 0])
        );
        assert_eq!(
            NodeKey::containing(ChunkCoord::new(-3, 0, 0), 1),
            NodeKey::new(1, [-2, 0, 0])
        );
    }

    #[test]
    fn containing_is_the_inverse_of_chunk_origin_at_the_corner() {
        for level in 0..4u32 {
            for &c in &[-40, -17, -1, 0, 1, 16, 39] {
                let key = NodeKey::containing(ChunkCoord::new(c, c, c), level);
                let origin = key.chunk_origin();
                let extent = key.extent_chunks();
                assert!(
                    (origin.x..origin.x + extent).contains(&c),
                    "level {level} c {c}: origin {origin:?} extent {extent}"
                );
            }
        }
    }

    #[test]
    fn level_0_desired_nodes_matches_chunk_streaming_shape() {
        // At level 0 only (a single-ring schedule), node == chunk 1:1, so
        // this should match `streaming::desired_chunks`'s own count exactly.
        let schedule = [(0u32, 2i32)];
        let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=1, &schedule);
        assert_eq!(nodes.len(), 5 * 5 * 2, "(2*2+1)^2 columns * 2 y layers");
        for n in &nodes {
            assert_eq!(n.level, 0);
        }
    }

    #[test]
    fn rings_do_not_double_count_the_same_node() {
        let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=0, DEFAULT_RING_SCHEDULE);
        let unique: HashSet<NodeKey> = nodes.iter().copied().collect();
        assert_eq!(nodes.len(), unique.len(), "no node emitted by two rings");
    }

    #[test]
    fn every_ring_beyond_the_first_contains_at_least_one_node() {
        // Sanity: a schedule with strictly increasing radii must actually
        // grow the resident set at each level, not silently contribute
        // nothing (which would mean the exclusion math ate the whole ring).
        for level in 1..DEFAULT_RING_SCHEDULE.len() {
            let schedule = &DEFAULT_RING_SCHEDULE[..=level];
            let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=0, schedule);
            let at_this_level = nodes.iter().filter(|n| n.level as usize == level).count();
            assert!(at_this_level > 0, "level {level} contributed no nodes");
        }
    }

    #[test]
    fn coarser_rings_cover_further_out_than_finer_ones() {
        let nodes = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=0, DEFAULT_RING_SCHEDULE);
        let center = ChunkCoord::new(0, 0, 0);
        // Every level-0 node's chunk origin is within the first ring's own
        // outer radius; every level-3 node reaches meaningfully further out
        // than every level-0 node does.
        let max_chunk_dist = |level: u32| -> i32 {
            nodes
                .iter()
                .filter(|n| n.level == level)
                .map(|n| {
                    let o = n.chunk_origin();
                    (o.x - center.x).abs().max((o.z - center.z).abs())
                })
                .max()
                .unwrap()
        };
        assert!(max_chunk_dist(3) > max_chunk_dist(0));
    }

    #[test]
    fn staying_put_is_a_no_op() {
        let nodes = desired_nodes(ChunkCoord::new(5, 0, 5), 0..=1, DEFAULT_RING_SCHEDULE);
        let resident: HashSet<NodeKey> = nodes.into_iter().collect();
        let updates = plan_node_updates(
            &resident,
            ChunkCoord::new(5, 0, 5),
            0..=1,
            DEFAULT_RING_SCHEDULE,
        );
        assert_eq!(updates, NodeStreamUpdates::default());
    }

    #[test]
    fn from_empty_everything_loads_nothing_unloads() {
        let schedule = [(0u32, 1i32)];
        let updates =
            plan_node_updates(&HashSet::new(), ChunkCoord::new(0, 0, 0), 0..=0, &schedule);
        assert_eq!(updates.to_load.len(), 3 * 3);
        assert!(updates.to_unload.is_empty());
        // Nearest-first: the center node loads before its neighbours.
        assert_eq!(updates.to_load[0], NodeKey::new(0, [0, 0, 0]));
    }

    #[test]
    fn moving_unloads_nodes_no_longer_desired() {
        let schedule = [(0u32, 1i32)];
        let resident: HashSet<NodeKey> = desired_nodes(ChunkCoord::new(0, 0, 0), 0..=0, &schedule)
            .into_iter()
            .collect();
        let updates = plan_node_updates(&resident, ChunkCoord::new(5, 0, 5), 0..=0, &schedule);
        assert!(!updates.to_unload.is_empty());
        assert!(updates.to_unload.iter().all(|n| resident.contains(n)));
    }
}
