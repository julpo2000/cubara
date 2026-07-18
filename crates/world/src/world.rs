//! Terrain generation and the current (fixed) world grid.
//!
//! Terrain is a deterministic function of world position, so any chunk can be
//! generated on demand from its [`ChunkCoord`] alone — [`World::chunk_at`] is the
//! primitive the streaming layer builds on. [`World::generate`] still bakes a small
//! fixed grid for the window/bench/screenshot paths until the renderer streams.

use cubara_voxel::{Chunk, ChunkCoord};

const CHUNKS_X: i32 = 8;
const CHUNKS_Y: i32 = 3;
const CHUNKS_Z: i32 = 8;

/// A chunk together with its position in the chunk grid.
pub struct PlacedChunk {
    pub coord: ChunkCoord,
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
    /// Generate the chunk at `coord` straight from the terrain function, or `None`
    /// if it contains no solid blocks (nothing to mesh — e.g. fully above ground).
    /// Deterministic and self-contained: no [`World`] instance required, so the
    /// streaming layer can call it for any coordinate.
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

    pub fn generate() -> Self {
        let mut chunks = Vec::new();
        for cz in 0..CHUNKS_Z {
            for cy in 0..CHUNKS_Y {
                for cx in 0..CHUNKS_X {
                    let coord = ChunkCoord::new(cx, cy, cz);
                    if let Some(chunk) = Self::chunk_at(coord) {
                        chunks.push(PlacedChunk { coord, chunk });
                    }
                }
            }
        }

        let size = Chunk::SIZE as i32;
        let extent = [
            (CHUNKS_X * size) as f32,
            (CHUNKS_Y * size) as f32,
            (CHUNKS_Z * size) as f32,
        ];
        Self { chunks, extent }
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
