//! Terrain generation.
//!
//! Terrain is a deterministic function of world position, so any chunk is
//! generated on demand from its [`ChunkCoord`] alone. [`World::chunk_at`] is the
//! primitive the streaming layer builds on — there is no persistent world grid;
//! the renderer streams chunks in and out around the camera (see the
//! [`streaming`](crate::streaming) policy).

use cubara_voxel::{Chunk, ChunkCoord};

/// Stateless terrain source: chunks are generated on demand, nothing is stored.
pub struct World;

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
    /// Whether the block at world coordinates `(x, y, z)` is solid — the terrain
    /// function sampled at a single block, without generating a whole chunk. The
    /// primitive [`raycast`](crate::raycast) queries the world through this.
    pub fn is_solid_at(x: i32, y: i32, z: i32) -> bool {
        y <= terrain_height(x, z)
    }

    /// Cast a ray through the terrain and return the first solid block hit (see
    /// [`raycast`](crate::raycast)) — the basis for targeting a block to break/place.
    pub fn raycast(origin: [f32; 3], dir: [f32; 3], max_dist: f32) -> Option<crate::RayHit> {
        crate::raycast(origin, dir, max_dist, |b| {
            Self::is_solid_at(b[0], b[1], b[2])
        })
    }

    /// Generate the chunk at `coord` straight from the terrain function, or `None`
    /// if it contains no solid blocks (nothing to mesh — e.g. fully above ground).
    /// Deterministic and self-contained, so the streaming layer can call it for any
    /// coordinate without any world state.
    pub fn chunk_at(coord: ChunkCoord) -> Option<Chunk> {
        let size = Chunk::SIZE as i32;
        let chunk = Chunk::from_solid_fn(|lx, ly, lz| {
            let wx = coord.x * size + lx as i32;
            let wy = coord.y * size + ly as i32;
            let wz = coord.z * size + lz as i32;
            wy <= terrain_height(wx, wz)
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
}
