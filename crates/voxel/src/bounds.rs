//! Axis-aligned world-space bounds.
//!
//! Pure geometry — no GPU, no world/streaming knowledge (`ARCHITECTURE.md` Rule
//! 4) — shared between `cubara-world` (which computes a meshed node's bounds,
//! `mesh_with_bounds` below) and `cubara-render` (which culls against them,
//! `crate::culling` there). Living here, next to `Mesh`, is what lets both
//! depend on it without either depending on the other.

use glam::Vec3;

use crate::mesh::Mesh;
use crate::voxel::{Chunk, MeshContext};

/// An axis-aligned bounding box in world space.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// The corner that extends furthest in the direction of `normal` — the corner
    /// most likely to remain inside the half-space `normal` points into. Used by
    /// `cubara-render`'s frustum test (`Frustum::intersects_aabb`).
    pub fn positive_vertex(&self, normal: Vec3) -> Vec3 {
        Vec3::new(
            if normal.x >= 0.0 {
                self.max.x
            } else {
                self.min.x
            },
            if normal.y >= 0.0 {
                self.max.y
            } else {
                self.min.y
            },
            if normal.z >= 0.0 {
                self.max.z
            } else {
                self.min.z
            },
        )
    }
}

/// Greedy-mesh `chunk` and compute its **world-space** bounds, given where this
/// chunk/node sits (`origin`, its world-space minimum corner) and how big one of
/// its lattice cells is in world units (`scale` — 1.0 for an ordinary chunk;
/// `2^level` for an LOD node, whose `16^3` lattice covers `2^level` chunks per
/// axis at the node's own coarser sample spacing). Returns `None` for a chunk
/// that produces no geometry.
///
/// This is the one shared step between two otherwise-separate meshing paths: an
/// explicit `(ChunkCoord, Chunk)` pair with no `World` involved (`cubara-render`'s
/// `render_chunks`/`upload_node`, `scale` always 1.0), and a `World`-driven
/// `NodeKey` (`cubara-world`'s `mesh::mesh_node`, `scale` from the node's own
/// level) — both just need "mesh this chunk, place it here, this big," so it
/// lives here rather than being duplicated in both crates.
pub fn build_mesh_bounded(
    chunk: &Chunk,
    ctx: &MeshContext,
    origin: [f32; 3],
    scale: f32,
) -> Option<(Mesh, Aabb)> {
    let mesh = chunk.build_mesh(ctx);
    if mesh.indices.is_empty() {
        return None;
    }
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for v in &mesh.vertices {
        let p = Vec3::new(v.x() as f32, v.y() as f32, v.z() as f32);
        min = min.min(p);
        max = max.max(p);
    }
    let offset = Vec3::from(origin);
    Some((mesh, Aabb::new(min * scale + offset, max * scale + offset)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_vertex_picks_the_far_corner_per_axis_sign() {
        let b = Aabb::new(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            b.positive_vertex(Vec3::new(1.0, 1.0, 1.0)),
            Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            b.positive_vertex(Vec3::new(-1.0, -1.0, -1.0)),
            Vec3::new(-1.0, -2.0, -3.0)
        );
    }
}
