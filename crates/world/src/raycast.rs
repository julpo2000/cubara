//! Voxel ray casting.
//!
//! Walks a ray through the block grid one cell at a time (the Amanatides–Woo DDA)
//! and returns the first solid block it enters, plus which face it came in through.
//! This is the primitive block *targeting* builds on: place a block against the hit
//! face, break the hit block. Solidity is supplied as a closure so the core stays
//! independent of the world source; [`World::raycast`](crate::World::raycast) wraps
//! it over the terrain. See issue #50.

/// What a [`raycast`] hit: the solid block, the face normal it was entered through
/// (unit axis vector, e.g. `[0, 1, 0]` for a top face; `[0, 0, 0]` if the ray began
/// already inside the block), and the distance along the ray to that face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub block: [i32; 3],
    pub normal: [i32; 3],
    pub distance: f32,
}

/// The block containing world position `p` (floor toward −∞, so it's correct for
/// negative coordinates).
fn block_of(p: [f32; 3]) -> [i32; 3] {
    [
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    ]
}

/// Cast a ray from `origin` along `dir` (need not be normalized) up to `max_dist`
/// world units, returning the first block for which `is_solid` is true. `None` if
/// nothing solid is hit within range (or `dir` is degenerate).
pub fn raycast(
    origin: [f32; 3],
    dir: [f32; 3],
    max_dist: f32,
    is_solid: impl Fn([i32; 3]) -> bool,
) -> Option<RayHit> {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if len < 1e-9 {
        return None;
    }
    let dir = [dir[0] / len, dir[1] / len, dir[2] / len];

    let mut voxel = block_of(origin);
    // Standing inside a solid block: report it with no face.
    if is_solid(voxel) {
        return Some(RayHit {
            block: voxel,
            normal: [0, 0, 0],
            distance: 0.0,
        });
    }

    let mut step = [0i32; 3];
    let mut t_max = [f32::INFINITY; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if dir[a] > 0.0 {
            step[a] = 1;
            t_max[a] = (voxel[a] as f32 + 1.0 - origin[a]) / dir[a];
            t_delta[a] = 1.0 / dir[a];
        } else if dir[a] < 0.0 {
            step[a] = -1;
            t_max[a] = (voxel[a] as f32 - origin[a]) / dir[a];
            t_delta[a] = -1.0 / dir[a];
        }
    }

    loop {
        // Step along whichever axis reaches its next voxel boundary first.
        let a = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };

        let t = t_max[a];
        if t > max_dist {
            return None;
        }
        voxel[a] += step[a];
        t_max[a] += t_delta[a];

        if is_solid(voxel) {
            let mut normal = [0, 0, 0];
            normal[a] = -step[a]; // the face entered points back along the step
            return Some(RayHit {
                block: voxel,
                normal,
                distance: t,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solidity of a world containing a single block at `b`.
    fn only(b: [i32; 3]) -> impl Fn([i32; 3]) -> bool {
        move |p| p == b
    }

    #[test]
    fn hits_block_straight_ahead() {
        // From z = -3 looking +z, the block at origin spans z 0..1: entered at z=0.
        let hit = raycast([0.5, 0.5, -3.0], [0.0, 0.0, 1.0], 10.0, only([0, 0, 0])).unwrap();
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [0, 0, -1]);
        assert!(
            (hit.distance - 3.0).abs() < 1e-4,
            "distance {}",
            hit.distance
        );
    }

    #[test]
    fn misses_when_ray_passes_beside_the_block() {
        // Parallel to +z but offset in x so it never enters the block at origin.
        assert!(raycast([5.5, 0.5, -3.0], [0.0, 0.0, 1.0], 100.0, only([0, 0, 0])).is_none());
    }

    #[test]
    fn respects_max_dist() {
        // The block is 3 units ahead; a 2-unit ray can't reach it.
        assert!(raycast([0.5, 0.5, -3.0], [0.0, 0.0, 1.0], 2.0, only([0, 0, 0])).is_none());
    }

    #[test]
    fn hits_top_face_when_casting_down() {
        // Downward ray onto a block: enters through its top (+Y) face.
        let hit = raycast([0.5, 5.0, 0.5], [0.0, -1.0, 0.0], 10.0, only([0, 0, 0])).unwrap();
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [0, 1, 0]);
        assert!(
            (hit.distance - 4.0).abs() < 1e-4,
            "distance {}",
            hit.distance
        );
    }

    #[test]
    fn hits_correct_side_face_from_the_right() {
        // Coming from +x toward −x: enters the block's +X face.
        let hit = raycast([5.0, 0.5, 0.5], [-1.0, 0.0, 0.0], 10.0, only([0, 0, 0])).unwrap();
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [1, 0, 0]);
    }

    #[test]
    fn origin_inside_solid_reports_that_block() {
        let hit = raycast([0.5, 0.5, 0.5], [0.0, 0.0, 1.0], 10.0, only([0, 0, 0])).unwrap();
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [0, 0, 0]);
        assert_eq!(hit.distance, 0.0);
    }

    #[test]
    fn negative_coordinates_floor_correctly() {
        // The block at [-1,-1,-1] spans [-1,0) on each axis; a ray from -3 hits it.
        let hit = raycast(
            [-0.5, -0.5, -3.0],
            [0.0, 0.0, 1.0],
            10.0,
            only([-1, -1, -1]),
        )
        .unwrap();
        assert_eq!(hit.block, [-1, -1, -1]);
        assert_eq!(hit.normal, [0, 0, -1]);
    }

    #[test]
    fn degenerate_direction_is_none() {
        // A zero-length direction can't march; even an all-solid world returns None.
        assert!(raycast([0.5, 0.5, 0.5], [0.0, 0.0, 0.0], 10.0, |_| true).is_none());
    }
}
