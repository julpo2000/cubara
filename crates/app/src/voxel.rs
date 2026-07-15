//! Voxel data and (for now) naive meshing.
//!
//! This is the M1 baseline: a single 16³ chunk, meshed by emitting all six faces
//! of every solid block regardless of neighbours. It is deliberately unoptimized —
//! greedy meshing and hidden-face culling land in M2 and will be measured against
//! the triangle count this produces.

use crate::mesh::{Mesh, Vertex};

/// A description of one cube face: its outward normal and four corner offsets
/// (in unit-cube space, 0..1) wound so `[0,1,2, 0,2,3]` forms the quad.
struct Face {
    normal: [f32; 3],
    corners: [[f32; 3]; 4],
}

#[rustfmt::skip]
const FACES: [Face; 6] = [
    Face { normal: [ 1.0,  0.0,  0.0], corners: [[1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0]] }, // +X
    Face { normal: [-1.0,  0.0,  0.0], corners: [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0]] }, // -X
    Face { normal: [ 0.0,  1.0,  0.0], corners: [[0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]] }, // +Y
    Face { normal: [ 0.0, -1.0,  0.0], corners: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]] }, // -Y
    Face { normal: [ 0.0,  0.0,  1.0], corners: [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]] }, // +Z
    Face { normal: [ 0.0,  0.0, -1.0], corners: [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]] }, // -Z
];

/// A cubic chunk of `SIZE³` blocks. Blocks are solid/empty for now.
pub struct Chunk {
    solid: Vec<bool>,
}

impl Chunk {
    pub const SIZE: usize = 16;
    const VOLUME: usize = Self::SIZE * Self::SIZE * Self::SIZE;

    fn index(x: usize, y: usize, z: usize) -> usize {
        (z * Self::SIZE + y) * Self::SIZE + x
    }

    pub fn is_solid(&self, x: usize, y: usize, z: usize) -> bool {
        self.solid[Self::index(x, y, z)]
    }

    pub fn solid_count(&self) -> usize {
        self.solid.iter().filter(|&&s| s).count()
    }

    /// Fill the chunk with a solid sphere so the 3D structure is clearly visible.
    pub fn generate_sphere() -> Self {
        let mut solid = vec![false; Self::VOLUME];
        let center = (Self::SIZE as f32 - 1.0) * 0.5;
        let radius = Self::SIZE as f32 * 0.47;
        for z in 0..Self::SIZE {
            for y in 0..Self::SIZE {
                for x in 0..Self::SIZE {
                    let dx = x as f32 - center;
                    let dy = y as f32 - center;
                    let dz = z as f32 - center;
                    if dx * dx + dy * dy + dz * dz <= radius * radius {
                        solid[Self::index(x, y, z)] = true;
                    }
                }
            }
        }
        Self { solid }
    }

    /// Naive mesher: emit all six faces of every solid block. The chunk is centered
    /// on the origin so a model rotation spins it in place.
    pub fn build_mesh(&self) -> Mesh {
        let mut mesh = Mesh::default();
        let half = Self::SIZE as f32 * 0.5;

        for z in 0..Self::SIZE {
            for y in 0..Self::SIZE {
                for x in 0..Self::SIZE {
                    if !self.is_solid(x, y, z) {
                        continue;
                    }
                    let bx = x as f32 - half;
                    let by = y as f32 - half;
                    let bz = z as f32 - half;

                    for face in &FACES {
                        let start = mesh.vertices.len() as u32;
                        for corner in &face.corners {
                            mesh.vertices.push(Vertex {
                                position: [bx + corner[0], by + corner[1], bz + corner[2]],
                                normal: face.normal,
                            });
                        }
                        mesh.indices.extend_from_slice(&[
                            start,
                            start + 1,
                            start + 2,
                            start,
                            start + 2,
                            start + 3,
                        ]);
                    }
                }
            }
        }
        mesh
    }
}
