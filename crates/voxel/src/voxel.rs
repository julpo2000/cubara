//! Voxel data and greedy meshing.
//!
//! A single 16³ chunk. Meshing culls hidden faces (a face is only emitted when its
//! neighbour is empty) and then *greedily merges* adjacent coplanar faces into
//! large quads, so a flat surface becomes a handful of triangles instead of one
//! quad per block. Out-of-chunk neighbours count as empty, so the outer shell is
//! always meshed.

use crate::mesh::{Mesh, Vertex};

/// One cell of a greedy-mesh slice: the face orientation plus its four corners'
/// ambient-occlusion levels. Two cells only merge into one quad when they are
/// *equal* — same orientation **and** same AO — so AO discontinuities (near edges
/// and crevices) correctly split the merge instead of interpolating wrong.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Face {
    /// 0 = no face here, +1 = faces toward +axis, -1 = toward -axis.
    sign: i8,
    /// Per-corner occlusion 0..=3 (3 = unoccluded/bright), in corner order c0..c3.
    ao: [u8; 4],
}

const NO_FACE: Face = Face {
    sign: 0,
    ao: [0; 4],
};

/// Standard voxel ambient occlusion for one face corner from its three neighbouring
/// voxels on the air side (the two edge voxels + the diagonal). Two solid edges fully
/// occlude the corner; otherwise each solid neighbour darkens it one step.
fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - (side1 as u8 + side2 as u8 + corner as u8)
    }
}

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

    /// Build a chunk by asking `f` whether each local block is solid.
    pub fn from_solid_fn(mut f: impl FnMut(usize, usize, usize) -> bool) -> Self {
        let mut solid = vec![false; Self::VOLUME];
        for z in 0..Self::SIZE {
            for y in 0..Self::SIZE {
                for x in 0..Self::SIZE {
                    if f(x, y, z) {
                        solid[Self::index(x, y, z)] = true;
                    }
                }
            }
        }
        Self { solid }
    }

    /// Whether the chunk has no solid blocks (nothing to mesh).
    pub fn is_empty(&self) -> bool {
        !self.solid.iter().any(|&s| s)
    }

    /// Greedy mesher. Sweeps slices along each axis, builds a per-slice mask of
    /// visible faces (signed by which side is solid), and merges equal-mask cells
    /// into the largest possible quads. Vertices are in local chunk space (0..SIZE);
    /// callers place the chunk in the world by offsetting positions.
    pub fn build_mesh(&self) -> Mesh {
        let mut mesh = Mesh::default();
        let n = Self::SIZE as i32;
        // `mask[v * n + u]`: the face (orientation + per-corner AO) at each slice cell.
        let mut mask = vec![NO_FACE; (n * n) as usize];

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
                        let sign = if a == b {
                            0
                        } else if a {
                            1
                        } else {
                            -1
                        };
                        mask[idx] = if sign == 0 {
                            NO_FACE
                        } else {
                            // Occluders sit on the empty side of the face plane. pos[d]
                            // is still the pre-advance boundary here.
                            let air_d = if sign == 1 { pos[d] + 1 } else { pos[d] };
                            Face {
                                sign,
                                ao: self.face_ao(d, u, v, air_d, uu, vv),
                            }
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
                        if m.sign == 0 {
                            i += 1;
                            continue;
                        }

                        // Grow the quad width along u, then height along v — only over
                        // cells with an identical face (same orientation *and* AO).
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

                        push_quad(&mut mesh, d, u, v, pos[d], i, j, w, h, m.sign, m.ao);

                        // Consume the merged cells.
                        for l in 0..h {
                            for k in 0..w {
                                mask[((j + l) * n + i + k) as usize] = NO_FACE;
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

    /// The four corners' AO for the face cell at (`uu`,`vv`) whose empty side is the
    /// `air_d` layer along axis `d`. Corner order matches `push_quad`'s c0..c3:
    /// (0,0), (1,0), (1,1), (0,1) in the (`u`,`v`) axes.
    fn face_ao(&self, d: usize, u: usize, v: usize, air_d: i32, uu: i32, vv: i32) -> [u8; 4] {
        let occluded = |du: i32, dv: i32| -> bool {
            let mut p = [0i32; 3];
            p[d] = air_d;
            p[u] = uu + du;
            p[v] = vv + dv;
            self.solid_at(p[0], p[1], p[2])
        };
        let mut ao = [3u8; 4];
        for (k, (cu, cv)) in [(0, 0), (1, 0), (1, 1), (0, 1)].iter().enumerate() {
            let su = if *cu == 1 { 1 } else { -1 };
            let sv = if *cv == 1 { 1 } else { -1 };
            ao[k] = vertex_ao(occluded(su, 0), occluded(0, sv), occluded(su, sv));
        }
        ao
    }
}

/// Emit one merged quad on the plane `plane` along axis `d`, spanning `w`×`h` cells
/// in the (`u`,`v`) axes starting at (`i`,`j`), with orientation `sign` (±1) and
/// per-corner AO `ao` (levels 0..=3, corner order c0..c3).
#[allow(clippy::too_many_arguments)]
fn push_quad(
    mesh: &mut Mesh,
    d: usize,
    u: usize,
    v: usize,
    plane: i32,
    i: i32,
    j: i32,
    w: i32,
    h: i32,
    sign: i8,
    ao: [u8; 4],
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
    normal[d] = sign as f32;

    let corner = |c: [i32; 3]| -> [f32; 3] { [c[0] as f32, c[1] as f32, c[2] as f32] };
    let c0 = base;
    let c1 = [base[0] + du[0], base[1] + du[1], base[2] + du[2]];
    let c2 = [
        base[0] + du[0] + dv[0],
        base[1] + du[1] + dv[1],
        base[2] + du[2] + dv[2],
    ];
    let c3 = [base[0] + dv[0], base[1] + dv[1], base[2] + dv[2]];

    // (du × dv) points toward +d, so keep that order for +d faces and reverse for -d
    // faces to stay outward/CCW for back-face culling. AO follows the same reorder.
    let (verts, vao) = if sign == 1 {
        ([c0, c1, c2, c3], [ao[0], ao[1], ao[2], ao[3]])
    } else {
        ([c0, c3, c2, c1], [ao[0], ao[3], ao[2], ao[1]])
    };

    let start = mesh.vertices.len() as u32;
    for (c, a) in verts.iter().zip(vao) {
        mesh.vertices.push(Vertex {
            position: corner(*c),
            normal,
            ao: a as f32 / 3.0,
        });
    }

    // Pick the diagonal that connects the two most-similar corners, so the darkened
    // corner doesn't bleed across the quad (the classic AO "flip" fix). Without it,
    // opposite-corner occlusion interpolates as an ugly gradient seam.
    if vao[0] as u16 + vao[2] as u16 > vao[1] as u16 + vao[3] as u16 {
        mesh.indices.extend_from_slice(&[
            start + 1,
            start + 2,
            start + 3,
            start + 1,
            start + 3,
            start,
        ]);
    } else {
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
    fn vertex_ao_levels() {
        assert_eq!(
            vertex_ao(false, false, false),
            3,
            "no occluders → brightest"
        );
        assert_eq!(
            vertex_ao(true, false, false),
            2,
            "one side occludes one step"
        );
        assert_eq!(
            vertex_ao(false, true, true),
            1,
            "side + corner occlude two steps"
        );
        assert_eq!(
            vertex_ao(true, true, false),
            0,
            "two solid sides fully occlude"
        );
    }

    #[test]
    fn lone_block_is_fully_lit() {
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        let mesh = chunk.build_mesh();
        assert!(
            mesh.vertices.iter().all(|v| v.ao == 1.0),
            "a block with no neighbours has no occluded corners"
        );
    }

    #[test]
    fn neighbour_darkens_a_corner() {
        // Two diagonally-touching blocks: each occludes a corner of the other's faces.
        let mut chunk = empty();
        set(&mut chunk, 5, 5, 5);
        set(&mut chunk, 6, 6, 5);
        let mesh = chunk.build_mesh();
        assert!(
            mesh.vertices.iter().any(|v| v.ao < 1.0),
            "a diagonal neighbour should occlude at least one corner"
        );
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
