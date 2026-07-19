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
}
