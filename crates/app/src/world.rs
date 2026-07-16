//! A small grid of chunks with simple heightmap terrain.
//!
//! Enough of a world for meshing, greedy merging and (next) frustum culling to act
//! on. Empty chunks (fully above the terrain) are dropped so we don't mesh air.

use crate::voxel::Chunk;

const CHUNKS_X: i32 = 8;
const CHUNKS_Y: i32 = 3;
const CHUNKS_Z: i32 = 8;

/// A chunk together with its position in the chunk grid.
pub struct PlacedChunk {
    pub coord: [i32; 3],
    pub chunk: Chunk,
}

pub struct World {
    pub chunks: Vec<PlacedChunk>,
    /// World size in blocks along each axis.
    pub extent: [f32; 3],
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
    pub fn generate() -> Self {
        let size = Chunk::SIZE as i32;
        let mut chunks = Vec::new();

        for cz in 0..CHUNKS_Z {
            for cy in 0..CHUNKS_Y {
                for cx in 0..CHUNKS_X {
                    let chunk = Chunk::from_solid_fn(|lx, ly, lz| {
                        let wx = cx * size + lx as i32;
                        let wy = cy * size + ly as i32;
                        let wz = cz * size + lz as i32;
                        wy <= terrain_height(wx, wz)
                    });
                    if !chunk.is_empty() {
                        chunks.push(PlacedChunk {
                            coord: [cx, cy, cz],
                            chunk,
                        });
                    }
                }
            }
        }

        let extent = [
            (CHUNKS_X * size) as f32,
            (CHUNKS_Y * size) as f32,
            (CHUNKS_Z * size) as f32,
        ];
        Self { chunks, extent }
    }

    /// World-space block offset of a chunk's origin corner.
    pub fn chunk_offset(coord: [i32; 3]) -> [f32; 3] {
        let size = Chunk::SIZE as f32;
        [
            coord[0] as f32 * size,
            coord[1] as f32 * size,
            coord[2] as f32 * size,
        ]
    }

    /// A pleasant point to aim the camera at (slightly below vertical middle).
    pub fn look_target(&self) -> [f32; 3] {
        [
            self.extent[0] * 0.5,
            self.extent[1] * 0.35,
            self.extent[2] * 0.5,
        ]
    }

    /// Camera orbit radius that frames the whole world.
    pub fn view_radius(&self) -> f32 {
        self.extent[0].max(self.extent[2]) * 0.9
    }
}
