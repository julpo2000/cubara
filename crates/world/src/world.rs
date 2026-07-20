//! Terrain generation + player edits.
//!
//! The base world is a deterministic function of position, so any chunk is
//! generated on demand from its [`ChunkCoord`] alone. On top of that sits a small
//! **edit overlay** — blocks the player has placed or broken — so
//! [`World::chunk_at`] returns terrain *plus* edits. The overlay is the only
//! mutable world state today (persistence layers on it later, #60); it's a global
//! behind an `RwLock` because chunk meshing runs on worker threads that all read it.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use cubara_voxel::{Chunk, ChunkCoord};

/// Deterministic terrain source, overlaid with player [edits](World::set_block).
pub struct World;

/// The player-edit overlay: world block coord → solid?, overriding terrain. Global
/// because the meshing worker pool reads it from several threads; writes (edits) are
/// rare, reads are cheap under the shared lock.
fn edits() -> &'static RwLock<HashMap<[i32; 3], bool>> {
    static EDITS: OnceLock<RwLock<HashMap<[i32; 3], bool>>> = OnceLock::new();
    EDITS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Deterministic rolling-hills height (in blocks) for a world column.
fn terrain_height(x: i32, z: i32) -> i32 {
    let fx = x as f32;
    let fz = z as f32;
    let h = 22.0
        + 7.0 * (fx * 0.045).sin() * (fz * 0.045).cos()
        + 4.0 * (fx * 0.11 + 1.7).sin()
        + 4.0 * (fz * 0.09 + 0.5).cos();
    h.round() as i32
}

impl World {
    /// Whether the block at world coordinates `(x, y, z)` is solid — a player edit if
    /// one exists there, otherwise the terrain. Samples a single block without
    /// generating a whole chunk; the primitive [`raycast`](crate::raycast) queries
    /// the world through this.
    pub fn is_solid_at(x: i32, y: i32, z: i32) -> bool {
        if let Some(&solid) = edits().read().expect("edits lock").get(&[x, y, z]) {
            return solid;
        }
        y <= terrain_height(x, z)
    }

    /// Place (`solid = true`) or break (`false`) the block at world `(x, y, z)`,
    /// recording it in the edit overlay so future [`chunk_at`](Self::chunk_at) /
    /// [`is_solid_at`](Self::is_solid_at) reflect it. Returns the [`ChunkCoord`] that
    /// contains the block, so the caller can re-mesh it.
    pub fn set_block(x: i32, y: i32, z: i32, solid: bool) -> ChunkCoord {
        edits()
            .write()
            .expect("edits lock")
            .insert([x, y, z], solid);
        ChunkCoord::from_world_pos([x as f32, y as f32, z as f32])
    }

    /// Cast a ray through the world (terrain + edits) and return the first solid
    /// block hit (see [`raycast`](crate::raycast)) — the basis for targeting a block
    /// to break/place.
    pub fn raycast(origin: [f32; 3], dir: [f32; 3], max_dist: f32) -> Option<crate::RayHit> {
        crate::raycast(origin, dir, max_dist, |b| {
            Self::is_solid_at(b[0], b[1], b[2])
        })
    }

    /// Generate the chunk at `coord` from terrain overlaid with any player edits, or
    /// `None` if it ends up with no solid blocks. Takes the edit lock once for the
    /// whole chunk.
    pub fn chunk_at(coord: ChunkCoord) -> Option<Chunk> {
        let size = Chunk::SIZE as i32;
        let overlay = edits().read().expect("edits lock");
        let chunk = Chunk::from_solid_fn(|lx, ly, lz| {
            let wx = coord.x * size + lx as i32;
            let wy = coord.y * size + ly as i32;
            let wz = coord.z * size + lz as i32;
            match overlay.get(&[wx, wy, wz]) {
                Some(&solid) => solid,
                None => wy <= terrain_height(wx, wz),
            }
        });
        (!chunk.is_empty()).then_some(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming;

    #[test]
    fn region_mesh_output_is_stable() {
        // Deterministic regression guard (runs in CI, no GPU): worldgen + greedy
        // meshing over a fixed region must keep producing exactly this many chunks
        // and triangles. A change here means terrain or the mesher changed — fine
        // if intended, but it should never move by accident.
        //
        // Triangle count jumped from 8,482 with per-vertex ambient occlusion (#45):
        // AO-varying cells can no longer merge into the same quad, so bumpy terrain
        // splits into more, smaller quads. Expected and accepted for the AO quality.
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 2, 0..=2);
        let mut chunks = 0usize;
        let mut tris = 0usize;
        for coord in coords {
            if let Some(chunk) = World::chunk_at(coord) {
                chunks += 1;
                tris += chunk.build_mesh().triangle_count();
            }
        }
        assert_eq!((chunks, tris), (52, 14716));
    }

    #[test]
    fn raycast_down_hits_the_terrain_surface() {
        // Straight down from high above (0,0): the first solid block is the surface,
        // entered through its top face.
        let hit = World::raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
            .expect("a downward ray must hit the ground");
        assert_eq!(hit.normal, [0, 1, 0], "entered through the top face");
        let [x, y, z] = hit.block;
        assert!(World::is_solid_at(x, y, z), "hit block is solid");
        assert!(
            !World::is_solid_at(x, y + 1, z),
            "the block above it is air"
        );
    }

    #[test]
    fn raycast_up_from_underground_returns_the_containing_block() {
        // Starting inside the terrain reports that block immediately (distance 0).
        let hit = World::raycast([0.5, 0.0, 0.5], [0.0, 1.0, 0.0], 100.0)
            .expect("underground origin is inside a solid block");
        assert_eq!(hit.distance, 0.0);
        assert_eq!(hit.block, [0, 0, 0]);
    }

    // Edit tests use coordinates far from the origin so the global overlay never
    // pollutes the region regression test above (which meshes chunks near 0,0,0).

    #[test]
    fn placing_and_breaking_override_terrain() {
        // High up = terrain air; place a solid block there.
        assert!(!World::is_solid_at(500_000, 120, 500_000));
        World::set_block(500_000, 120, 500_000, true);
        assert!(World::is_solid_at(500_000, 120, 500_000));

        // At y=0 = terrain solid; break it.
        assert!(World::is_solid_at(500_016, 0, 500_016));
        World::set_block(500_016, 0, 500_016, false);
        assert!(!World::is_solid_at(500_016, 0, 500_016));
    }

    #[test]
    fn set_block_returns_containing_chunk() {
        // Block (16, 33, -1) lives in chunk (1, 2, -1) (16-block chunks).
        assert_eq!(
            World::set_block(600_016, 33, -1, true),
            ChunkCoord::new(37501, 2, -1)
        );
    }

    #[test]
    fn chunk_at_reflects_a_placed_block() {
        // A chunk high in the air is empty (None) until we place a block in it.
        let cc = ChunkCoord::new(43_750, 8, 43_750); // blocks ~700_000, y 128..143
        assert!(World::chunk_at(cc).is_none(), "air chunk starts empty");
        World::set_block(700_000, 130, 700_000, true);
        assert!(
            World::chunk_at(cc).is_some(),
            "placing a block makes the chunk non-empty"
        );
    }
}
