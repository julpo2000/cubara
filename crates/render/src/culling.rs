//! View-frustum culling.
//!
//! Chunks are culled per-frame by testing their world-space bounding box against
//! the camera's view frustum, extracted from the combined view*projection matrix
//! (Gribb/Hartmann method, adapted for wgpu's column-vector, `[0, w]` depth-range
//! clip space — matching `glam::Mat4::perspective_rh`).
//!
//! The extraction is indifferent to reversed-Z (`render::reverse_z`, which the
//! production camera uses). Flipping depth swaps which *row combination* yields
//! the near plane and which yields the far one, so the two labels below are the
//! wrong way round under reversal — but the resulting *set* of six planes is
//! identical, and [`Frustum::intersects_aabb`] only ever tests all six. Pinned
//! by `a_reversed_z_frustum_culls_identically`.

pub use cubara_voxel::Aabb;
use glam::{Mat4, Vec3, Vec4};

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

    #[test]
    fn a_reversed_z_frustum_culls_identically() {
        // The production camera reverses depth. Doing so swaps which row
        // combination yields the near plane and which yields the far one, so
        // the labels in `from_view_proj` are inverted for it -- but the set of
        // six planes is the same set, and every box must therefore classify
        // the same way. If this ever diverges, reversed-Z has quietly started
        // culling geometry the player can see (or drawing geometry it should
        // not), which no other test in this crate would notice.
        let proj = Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0), Vec3::Y);
        let standard = Frustum::from_view_proj(proj * view);
        let reversed = Frustum::from_view_proj(crate::render::reverse_z(proj) * view);

        let cases = [
            (
                "ahead",
                Vec3::new(-0.5, -0.5, -20.5),
                Vec3::new(0.5, 0.5, -19.5),
            ),
            (
                "far to the side",
                Vec3::new(100.0, -0.5, -20.5),
                Vec3::new(101.0, 0.5, -19.5),
            ),
            (
                "behind",
                Vec3::new(-0.5, -0.5, 9.0),
                Vec3::new(0.5, 0.5, 10.0),
            ),
            (
                "enclosing",
                Vec3::new(-50.0, -50.0, -50.0),
                Vec3::new(50.0, 50.0, 50.0),
            ),
            (
                "beyond far",
                Vec3::new(-0.5, -0.5, -200.5),
                Vec3::new(0.5, 0.5, -199.5),
            ),
            (
                "just inside near",
                Vec3::new(-0.01, -0.01, -0.2),
                Vec3::new(0.01, 0.01, -0.15),
            ),
        ];
        for (what, min, max) in cases {
            let b = Aabb::new(min, max);
            assert_eq!(
                standard.intersects_aabb(&b),
                reversed.intersects_aabb(&b),
                "the {what} box classifies differently under reversed-Z"
            );
        }
    }

    #[test]
    fn reverse_z_maps_near_to_one_and_far_to_zero() {
        // The property the depth compare and clear value both depend on. If
        // this flips, everything still renders -- just with the depth test
        // inverted, which looks like geometry drawn in the wrong order rather
        // than like a broken matrix.
        let proj =
            crate::render::reverse_z(Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 100.0));
        let depth_of = |z: f32| {
            let clip = proj * Vec4::new(0.0, 0.0, z, 1.0);
            clip.z / clip.w
        };
        // View space looks down -Z, so the near plane is at z = -0.1.
        assert!(
            (depth_of(-0.1) - 1.0).abs() < 1e-4,
            "near plane should be depth 1, got {}",
            depth_of(-0.1)
        );
        assert!(
            depth_of(-100.0).abs() < 1e-4,
            "far plane should be depth 0, got {}",
            depth_of(-100.0)
        );
    }
}
