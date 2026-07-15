//! Voxel data and greedy meshing.
//!
//! A single 16³ chunk. Meshing culls hidden faces (a face is only emitted when its
//! neighbour is empty) and then *greedily merges* adjacent coplanar faces into
//! large quads, so a flat surface becomes a handful of triangles instead of one
//! quad per block. Out-of-chunk neighbours count as empty, so the outer shell is
//! always meshed.

use crate::mesh::{Mesh, Vertex};

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

    /// Solidity with bounds handling: anything outside the chunk counts as empty.
    fn solid_at(&self, x: i32, y: i32, z: i32) -> bool {
        if x < 0 || y < 0 || z < 0 {
            return false;
        }
        let (x, y, z) = (x as usize, y as usize, z as usize);
        if x >= Self::SIZE || y >= Self::SIZE || z >= Self::SIZE {
            return false;
        }
        self.is_solid(x, y, z)
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

    /// Greedy mesher. Sweeps slices along each axis, builds a per-slice mask of
    /// visible faces (signed by which side is solid), and merges equal-mask cells
    /// into the largest possible quads. The chunk is centered on the origin.
    pub fn build_mesh(&self) -> Mesh {
        let mut mesh = Mesh::default();
        let n = Self::SIZE as i32;
        let half = Self::SIZE as f32 * 0.5;
        // `mask[v * n + u]`: 0 = no face, +1 = face toward +axis, -1 = toward -axis.
        let mut mask = vec![0i8; (n * n) as usize];

        for d in 0..3usize {
            let u = (d + 1) % 3;
            let v = (d + 2) % 3;
            let mut pos = [0i32; 3];
            let mut step = [0i32; 3];
            step[d] = 1;

            // Sweep the slice boundaries from -1 to n-1; the face plane is at pos[d]+1.
            pos[d] = -1;
            while pos[d] < n {
                // Build the mask for this slice boundary.
                let mut idx = 0usize;
                for vv in 0..n {
                    pos[v] = vv;
                    for uu in 0..n {
                        pos[u] = uu;
                        let a = self.solid_at(pos[0], pos[1], pos[2]);
                        let b = self.solid_at(pos[0] + step[0], pos[1] + step[1], pos[2] + step[2]);
                        mask[idx] = if a == b {
                            0
                        } else if a {
                            1
                        } else {
                            -1
                        };
                        idx += 1;
                    }
                }

                pos[d] += 1; // advance to the face plane

                // Greedily merge the mask into quads.
                let mut j = 0i32;
                while j < n {
                    let mut i = 0i32;
                    while i < n {
                        let m = mask[(j * n + i) as usize];
                        if m == 0 {
                            i += 1;
                            continue;
                        }

                        // Grow the quad width along u, then height along v.
                        let mut w = 1i32;
                        while i + w < n && mask[(j * n + i + w) as usize] == m {
                            w += 1;
                        }
                        let mut h = 1i32;
                        'height: while j + h < n {
                            for k in 0..w {
                                if mask[((j + h) * n + i + k) as usize] != m {
                                    break 'height;
                                }
                            }
                            h += 1;
                        }

                        self.push_quad(&mut mesh, d, u, v, pos[d], i, j, w, h, m, half);

                        // Consume the merged cells.
                        for l in 0..h {
                            for k in 0..w {
                                mask[((j + l) * n + i + k) as usize] = 0;
                            }
                        }
                        i += w;
                    }
                    j += 1;
                }
            }
        }
        mesh
    }

    /// Emit one merged quad on the plane `plane` along axis `d`, spanning `w`×`h`
    /// cells in the (`u`,`v`) axes starting at (`i`,`j`), with sign `m` (±1).
    #[allow(clippy::too_many_arguments)]
    fn push_quad(
        &self,
        mesh: &mut Mesh,
        d: usize,
        u: usize,
        v: usize,
        plane: i32,
        i: i32,
        j: i32,
        w: i32,
        h: i32,
        m: i8,
        half: f32,
    ) {
        let mut base = [0i32; 3];
        base[d] = plane;
        base[u] = i;
        base[v] = j;
        let mut du = [0i32; 3];
        du[u] = w;
        let mut dv = [0i32; 3];
        dv[v] = h;

        let mut normal = [0.0f32; 3];
        normal[d] = m as f32;

        let corner = |c: [i32; 3]| -> [f32; 3] {
            [c[0] as f32 - half, c[1] as f32 - half, c[2] as f32 - half]
        };
        let c0 = base;
        let c1 = [base[0] + du[0], base[1] + du[1], base[2] + du[2]];
        let c2 = [
            base[0] + du[0] + dv[0],
            base[1] + du[1] + dv[1],
            base[2] + du[2] + dv[2],
        ];
        let c3 = [base[0] + dv[0], base[1] + dv[1], base[2] + dv[2]];

        // (du × dv) points toward +d, so keep that order for +d faces and reverse
        // for -d faces to stay outward/CCW for back-face culling.
        let ordered = if m == 1 {
            [c0, c1, c2, c3]
        } else {
            [c0, c3, c2, c1]
        };

        let start = mesh.vertices.len() as u32;
        for c in ordered {
            mesh.vertices.push(Vertex {
                position: corner(c),
                normal,
            });
        }
        mesh.indices
            .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Chunk {
        Chunk {
            solid: vec![false; Chunk::VOLUME],
        }
    }

    fn set(chunk: &mut Chunk, x: usize, y: usize, z: usize) {
        let i = Chunk::index(x, y, z);
        chunk.solid[i] = true;
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    #[test]
    fn single_block_makes_six_quads() {
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let mesh = chunk.build_mesh();
        assert_eq!(mesh.vertices.len(), 24, "6 quads * 4 vertices");
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn full_chunk_merges_each_face_into_one_quad() {
        let chunk = Chunk {
            solid: vec![true; Chunk::VOLUME],
        };
        let mesh = chunk.build_mesh();
        // Only the 6 outer faces are visible, each merged into a single quad.
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn quads_are_wound_outward() {
        // Back-face culling relies on every quad being wound CCW/outward, i.e. the
        // geometric normal must agree with the stored normal.
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let mesh = chunk.build_mesh();
        for quad in 0..mesh.vertices.len() / 4 {
            let v0 = mesh.vertices[quad * 4].position;
            let v1 = mesh.vertices[quad * 4 + 1].position;
            let v2 = mesh.vertices[quad * 4 + 2].position;
            let geo = cross(sub(v1, v0), sub(v2, v0));
            let n = mesh.vertices[quad * 4].normal;
            let dot = geo[0] * n[0] + geo[1] * n[1] + geo[2] * n[2];
            assert!(dot > 0.0, "quad {quad} is wound inward (geo·n = {dot})");
        }
    }
}
