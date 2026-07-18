//! View-frustum culling.
//!
//! Chunks are culled per-frame by testing their world-space bounding box against
//! the camera's view frustum, extracted from the combined view*projection matrix
//! (Gribb/Hartmann method, adapted for wgpu's column-vector, `[0, w]` depth-range
//! clip space — matching `glam::Mat4::perspective_rh`).

use glam::{Mat4, Vec3, Vec4};

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
    /// most likely to remain inside the half-space `normal` points into.
    fn positive_vertex(&self, normal: Vec3) -> Vec3 {
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

/// A camera view frustum as 6 planes (`ax + by + cz + d >= 0` means "inside"),
/// extracted from a view*projection matrix.
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_proj(m: Mat4) -> Self {
        // Rows of `m`, built from its columns (glam stores Mat4 column-major).
        let row0 = Vec4::new(m.x_axis.x, m.y_axis.x, m.z_axis.x, m.w_axis.x);
        let row1 = Vec4::new(m.x_axis.y, m.y_axis.y, m.z_axis.y, m.w_axis.y);
        let row2 = Vec4::new(m.x_axis.z, m.y_axis.z, m.z_axis.z, m.w_axis.z);
        let row3 = Vec4::new(m.x_axis.w, m.y_axis.w, m.z_axis.w, m.w_axis.w);

        let mut planes = [
            row3 + row0, // left:   w + x >= 0
            row3 - row0, // right:  w - x >= 0
            row3 + row1, // bottom: w + y >= 0
            row3 - row1, // top:    w - y >= 0
            row2,        // near:   z >= 0 (wgpu clip-space depth is [0, w])
            row3 - row2, // far:    w - z >= 0
        ];
        for p in &mut planes {
            let len = Vec3::new(p.x, p.y, p.z).length();
            *p /= len;
        }
        Self { planes }
    }

    /// Conservative test: `false` only when `aabb` is fully outside at least one
    /// plane. May return `true` for a box that is actually outside (e.g. cut off by
    /// a frustum corner), but never `false` for a box that is actually visible.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        for plane in &self.planes {
            let normal = Vec3::new(plane.x, plane.y, plane.z);
            let corner = aabb.positive_vertex(normal);
            if normal.dot(corner) + plane.w < 0.0 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera at the origin looking down -Z (glam's right-handed convention),
    /// 60° vertical FOV, near 0.1 / far 100.
    fn test_frustum() -> Frustum {
        let proj = Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0), Vec3::Y);
        Frustum::from_view_proj(proj * view)
    }

    #[test]
    fn box_ahead_is_visible() {
        let f = test_frustum();
        let b = Aabb::new(Vec3::new(-0.5, -0.5, -20.5), Vec3::new(0.5, 0.5, -19.5));
        assert!(f.intersects_aabb(&b));
    }

    #[test]
    fn box_far_to_the_side_is_culled() {
        let f = test_frustum();
        let b = Aabb::new(Vec3::new(100.0, -0.5, -20.5), Vec3::new(101.0, 0.5, -19.5));
        assert!(!f.intersects_aabb(&b));
    }

    #[test]
    fn box_behind_camera_is_culled() {
        let f = test_frustum();
        let b = Aabb::new(Vec3::new(-0.5, -0.5, 9.0), Vec3::new(0.5, 0.5, 10.0));
        assert!(!f.intersects_aabb(&b));
    }

    #[test]
    fn box_enclosing_camera_is_visible() {
        let f = test_frustum();
        let b = Aabb::new(Vec3::new(-50.0, -50.0, -50.0), Vec3::new(50.0, 50.0, 50.0));
        assert!(f.intersects_aabb(&b));
    }

    #[test]
    fn box_beyond_far_plane_is_culled() {
        let f = test_frustum();
        let b = Aabb::new(Vec3::new(-0.5, -0.5, -200.5), Vec3::new(0.5, 0.5, -199.5));
        assert!(!f.intersects_aabb(&b));
    }
}
