//! Walking physics: gravity, jump, step-up, and collision (`docs/PHASE1_ARCHITECTURE.md`
//! §10, issue #53).
//!
//! An axis-aligned box swept against solid voxels and resolved **axis by axis
//! in a fixed Y, X, Z order**, so the result never depends on iteration or
//! scheduling (`ARCHITECTURE.md` Rule 1). Swept, not discrete: each axis's
//! move walks every integer voxel layer the box's leading face crosses and
//! stops at the first solid one, so it can't tunnel through a thin wall or
//! floor at any velocity -- a "move then push out" test would tunnel exactly
//! there.
//!
//! `is_solid` is a closure, not a `cubara_world::World` reference, mirroring
//! `cubara_world::raycast`'s own convention -- it's what lets this module's
//! tests build a trivial synthetic floor instead of a real generated world.

use glam::Vec3;

use crate::input::InputFrame;
use crate::player::Player;

/// Half the player's horizontal footprint, blocks (full width 0.6).
const HALF_WIDTH: f32 = 0.3;
/// Collision-box height, feet to head, blocks.
const HEIGHT: f32 = 1.8;
/// How far above the feet [`Player::pos`] (the eye) sits -- the box is
/// derived from `pos` by subtracting this, so `pos` keeps meaning "the
/// camera" in both movement modes.
const EYE_HEIGHT: f32 = 1.62;
/// The tallest ledge walking climbs without a jump.
const STEP_HEIGHT: f32 = 1.0;
/// Downward acceleration, blocks/second².
const GRAVITY: f32 = 32.0;
/// Upward speed a jump starts at, blocks/second -- a little over one block
/// of height against [`GRAVITY`].
const JUMP_SPEED: f32 = 9.0;
/// Horizontal walking speed, blocks/second.
const WALK_SPEED: f32 = 5.0;
/// Kept off exact voxel boundaries so a box flush with a face isn't treated
/// as overlapping the cell beyond it.
const EPS: f32 = 1e-4;

#[derive(Clone, Copy)]
struct Aabb {
    min: Vec3,
    max: Vec3,
}

impl Aabb {
    fn from_feet(feet: Vec3) -> Self {
        Self {
            min: Vec3::new(feet.x - HALF_WIDTH, feet.y, feet.z - HALF_WIDTH),
            max: Vec3::new(feet.x + HALF_WIDTH, feet.y + HEIGHT, feet.z + HALF_WIDTH),
        }
    }

    fn feet(&self) -> Vec3 {
        Vec3::new(
            (self.min.x + self.max.x) * 0.5,
            self.min.y,
            (self.min.z + self.max.z) * 0.5,
        )
    }

    fn translated(&self, delta: Vec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }
}

/// Advance one fixed tick of walking physics. Turns by `input.look_delta`
/// like every mode does, applies gravity and a jump impulse, then resolves
/// movement against `is_solid` in the fixed Y, X, Z order.
pub(crate) fn step(
    player: &mut Player,
    input: &InputFrame,
    dt: f32,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) {
    player.apply_look(input);

    if input.jump && player.on_ground {
        player.velocity.y = JUMP_SPEED;
        player.on_ground = false;
    }
    player.velocity.y -= GRAVITY * dt;

    let (forward, right) = player.horizontal_axes();
    let wish = forward * input.move_axes[2] + right * input.move_axes[0];
    let wish = if wish != Vec3::ZERO {
        wish.normalize()
    } else {
        wish
    };
    player.velocity.x = wish.x * WALK_SPEED;
    player.velocity.z = wish.z * WALK_SPEED;

    let was_on_ground = player.on_ground;
    let feet = Vec3::new(player.pos.x, player.pos.y - EYE_HEIGHT, player.pos.z);
    let aabb = Aabb::from_feet(feet);

    let (aabb, y_blocked, y_negative) = move_axis(aabb, 1, player.velocity.y * dt, &is_solid);
    player.on_ground = y_blocked && y_negative;
    if y_blocked {
        player.velocity.y = 0.0;
    }

    let aabb = move_axis_with_step(aabb, 0, player.velocity.x * dt, was_on_ground, &is_solid);
    let aabb = move_axis_with_step(aabb, 2, player.velocity.z * dt, was_on_ground, &is_solid);

    let feet = aabb.feet();
    player.pos = Vec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z);
}

/// The two axis indices other than `axis` (0 = x, 1 = y, 2 = z).
fn other_axes(axis: usize) -> [usize; 2] {
    match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    }
}

fn axis_delta(axis: usize, d: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(d, 0.0, 0.0),
        1 => Vec3::new(0.0, d, 0.0),
        _ => Vec3::new(0.0, 0.0, d),
    }
}

fn voxel_coord(axis: usize, layer: i32, other: [(usize, i32); 2]) -> [i32; 3] {
    let mut c = [0i32; 3];
    c[axis] = layer;
    c[other[0].0] = other[0].1;
    c[other[1].0] = other[1].1;
    c
}

/// Sweep `aabb` by `delta` along `axis`, stopping at the first solid voxel
/// its leading face would enter. Returns the AABB after the allowed
/// (possibly clipped) displacement, whether it was clipped by a collision,
/// and whether `delta` was negative (so the caller can tell "hit a floor"
/// from "hit a ceiling").
fn move_axis(
    aabb: Aabb,
    axis: usize,
    delta: f32,
    is_solid: &impl Fn(i32, i32, i32) -> bool,
) -> (Aabb, bool, bool) {
    if delta == 0.0 {
        return (aabb, false, false);
    }
    let negative = delta < 0.0;
    let [u, v] = other_axes(axis);
    // `+ EPS` / `- EPS` on the low/high side respectively: `aabb`'s bounds
    // came from a `pos <-> feet` round-trip through `EYE_HEIGHT` (`step`,
    // and this same function's own return value feeding the next call), so
    // a box resting exactly on an integer boundary can arrive as a few ULPs
    // below it. Without this, `floor()` reads that as one layer lower and
    // wrongly reports solid ground as open air.
    let u_lo = (aabb.min[u] + EPS).floor() as i32;
    let u_hi = (aabb.max[u] - EPS).floor() as i32;
    let v_lo = (aabb.min[v] + EPS).floor() as i32;
    let v_hi = (aabb.max[v] - EPS).floor() as i32;

    let leading = if negative {
        aabb.min[axis]
    } else {
        aabb.max[axis]
    };
    let target = leading + delta;
    let (start_layer, end_layer, step) = if negative {
        (leading.ceil() as i32 - 1, target.ceil() as i32 - 1, -1)
    } else {
        (leading.floor() as i32, target.floor() as i32, 1)
    };

    let mut layer = start_layer;
    while (step == 1 && layer <= end_layer) || (step == -1 && layer >= end_layer) {
        for a in u_lo..=u_hi {
            for b in v_lo..=v_hi {
                let coord = voxel_coord(axis, layer, [(u, a), (v, b)]);
                if is_solid(coord[0], coord[1], coord[2]) {
                    let contact = if negative {
                        (layer + 1) as f32
                    } else {
                        layer as f32
                    };
                    return (
                        aabb.translated(axis_delta(axis, contact - leading)),
                        true,
                        negative,
                    );
                }
            }
        }
        layer += step;
    }
    (aabb.translated(axis_delta(axis, delta)), false, negative)
}

/// Like [`move_axis`], but if the direct move is blocked and `can_step` (the
/// player was on the ground at the start of this tick), tries lifting the
/// box by up to [`STEP_HEIGHT`], moving horizontally from there, then
/// settling back down -- classic step-up-a-ledge assist, so walking over a
/// one-block rise doesn't require a jump. Keeps whichever path made more
/// horizontal progress.
fn move_axis_with_step(
    aabb: Aabb,
    axis: usize,
    delta: f32,
    can_step: bool,
    is_solid: &impl Fn(i32, i32, i32) -> bool,
) -> Aabb {
    let (flat, blocked, _) = move_axis(aabb, axis, delta, is_solid);
    if delta == 0.0 || !blocked || !can_step {
        return flat;
    }

    let (raised, _, _) = move_axis(aabb, 1, STEP_HEIGHT, is_solid);
    let (stepped, stepped_blocked, _) = move_axis(raised, axis, delta, is_solid);
    if stepped_blocked {
        return flat;
    }
    let (settled, _, _) = move_axis(stepped, 1, -STEP_HEIGHT, is_solid);

    let flat_progress = (flat.min[axis] - aabb.min[axis]).abs();
    let stepped_progress = (settled.min[axis] - aabb.min[axis]).abs();
    if stepped_progress > flat_progress {
        settled
    } else {
        flat
    }
}

/// Whether `player`'s collision box (derived from `pos` the same way
/// [`step`] does) overlaps any solid voxel -- the invariant every test in
/// this module and [`crate`]'s own tests check after each tick. `pub(crate)`
/// and test-only: shared with `crate::tests` so the 10,000-tick walk test
/// (issue #53's "Done when") doesn't duplicate this box math against a real
/// `World`.
#[cfg(test)]
pub(crate) fn player_intersects_solid(
    player: &Player,
    is_solid: &impl Fn(i32, i32, i32) -> bool,
) -> bool {
    let feet = Vec3::new(player.pos.x, player.pos.y - EYE_HEIGHT, player.pos.z);
    let aabb = Aabb::from_feet(feet);
    // Same `+ EPS` / `- EPS` skin as `move_axis` -- see its comment.
    let x_lo = (aabb.min.x + EPS).floor() as i32;
    let x_hi = (aabb.max.x - EPS).floor() as i32;
    let y_lo = (aabb.min.y + EPS).floor() as i32;
    let y_hi = (aabb.max.y - EPS).floor() as i32;
    let z_lo = (aabb.min.z + EPS).floor() as i32;
    let z_hi = (aabb.max.z - EPS).floor() as i32;
    (x_lo..=x_hi)
        .flat_map(|x| (y_lo..=y_hi).flat_map(move |y| (z_lo..=z_hi).map(move |z| (x, y, z))))
        .any(|(x, y, z)| is_solid(x, y, z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec3;

    /// A flat floor: solid at and below `y = 0`, air above.
    fn flat_floor(x: i32, y: i32, _z: i32) -> bool {
        let _ = x;
        y <= 0
    }

    /// The same flat floor as [`flat_floor`] (solid at `y <= 0`, resting
    /// height `y = 1`), plus a single one-block-high ledge starting at
    /// `z <= -2` -- positioned along −Z, the default yaw-0 forward
    /// direction, so a test can walk into it with no input but `move_axes`.
    fn floor_with_one_block_step(x: i32, y: i32, z: i32) -> bool {
        let _ = x;
        y <= 0 || (z <= -2 && y == 1)
    }

    fn no_input() -> InputFrame {
        InputFrame::default()
    }

    fn aabb_overlaps(player: &Player, is_solid: &impl Fn(i32, i32, i32) -> bool) -> bool {
        player_intersects_solid(player, is_solid)
    }

    #[test]
    fn gravity_pulls_a_falling_player_down() {
        let mut player = Player::new(vec3(0.5, 50.0, 0.5), 0.0, 0.0);
        step(&mut player, &no_input(), 1.0 / 60.0, flat_floor);
        assert!(player.velocity.y < 0.0);
        assert!(player.pos.y < 50.0);
    }

    #[test]
    fn a_player_falling_onto_a_floor_comes_to_rest_on_ground() {
        let mut player = Player::new(vec3(0.5, 10.0, 0.5), 0.0, 0.0);
        for _ in 0..600 {
            step(&mut player, &no_input(), 1.0 / 60.0, flat_floor);
            assert!(
                !aabb_overlaps(&player, &flat_floor),
                "tunnelled into the floor"
            );
        }
        assert!(player.on_ground);
        assert_eq!(player.velocity.y, 0.0);
    }

    #[test]
    fn never_tunnels_through_the_floor_across_a_spread_of_velocities() {
        for speed in [1.0_f32, 10.0, 32.0, 60.0, 100.0, 250.0] {
            let mut player = Player::new(vec3(0.5, 30.0, 0.5), 0.0, 0.0);
            player.velocity.y = -speed;
            for _ in 0..600 {
                step(&mut player, &no_input(), 1.0 / 60.0, flat_floor);
                assert!(
                    !aabb_overlaps(&player, &flat_floor),
                    "tunnelled through the floor at {speed} blocks/s"
                );
                if player.on_ground {
                    break;
                }
            }
            assert!(player.on_ground, "never landed at {speed} blocks/s");
        }
    }

    #[test]
    fn jump_only_launches_from_the_ground() {
        let mut player = Player::new(vec3(0.5, 1.0 + EYE_HEIGHT, 0.5), 0.0, 0.0);
        // Settle onto the floor first.
        for _ in 0..60 {
            step(&mut player, &no_input(), 1.0 / 60.0, flat_floor);
        }
        assert!(player.on_ground);

        let jump = InputFrame {
            jump: true,
            ..InputFrame::default()
        };
        step(&mut player, &jump, 1.0 / 60.0, flat_floor);
        assert!(
            player.velocity.y > 0.0,
            "jump applies an upward impulse from the ground"
        );
        assert!(!player.on_ground);

        // Airborne now -- a second jump edge this tick does nothing further.
        let v_after_first = player.velocity.y;
        step(&mut player, &jump, 1.0 / 60.0, flat_floor);
        assert!(
            player.velocity.y < v_after_first,
            "no double-jump: gravity alone should reduce velocity, not another impulse"
        );
    }

    #[test]
    fn walking_forward_moves_horizontally_at_walk_speed() {
        let mut player = Player::new(vec3(0.5, 1.0 + EYE_HEIGHT, 0.5), 0.0, 0.0);
        for _ in 0..60 {
            step(&mut player, &no_input(), 1.0 / 60.0, flat_floor);
        }
        let z_before = player.pos.z;
        let forward = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            ..InputFrame::default()
        };
        for _ in 0..60 {
            step(&mut player, &forward, 1.0 / 60.0, flat_floor);
        }
        // Yaw 0 looks toward −Z (matching `Player::look_dir`).
        assert!(player.pos.z < z_before, "moved toward −Z");
        assert!(
            (z_before - player.pos.z - WALK_SPEED).abs() < 0.1,
            "about one second at WALK_SPEED"
        );
    }

    #[test]
    fn steps_up_a_one_block_ledge_while_walking_into_it() {
        // Yaw 0 faces −Z (`Player::look_dir`); the ledge sits at z <= −2,
        // so walking forward with no turn walks straight into it.
        let mut player = Player::new(vec3(0.5, 1.0 + EYE_HEIGHT, 0.5), 0.0, 0.0);
        for _ in 0..60 {
            step(
                &mut player,
                &no_input(),
                1.0 / 60.0,
                floor_with_one_block_step,
            );
        }
        assert!(player.on_ground);

        let forward = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            ..InputFrame::default()
        };
        for _ in 0..180 {
            step(&mut player, &forward, 1.0 / 60.0, floor_with_one_block_step);
            assert!(
                !aabb_overlaps(&player, &floor_with_one_block_step),
                "tunnelled into the step"
            );
        }
        assert!(
            player.pos.z < -2.5,
            "climbed onto the raised ledge: z = {}",
            player.pos.z
        );
        assert!(player.on_ground);
    }

    #[test]
    fn free_fly_ignores_collision() {
        // Not `step`'s job to test free-fly's own movement (that's
        // `player::tests`) -- this pins that free-fly is genuinely a
        // separate path with no collision, the thing that makes noclip
        // useful for debugging.
        let mut player = Player::new(vec3(0.5, 0.5, 0.5), 0.0, 0.0);
        let down = InputFrame {
            move_axes: [0.0, -1.0, 0.0],
            ..InputFrame::default()
        };
        player.apply_free_fly(&down, 1.0);
        assert!(
            player.pos.y < -5.0,
            "free-fly flew straight through the floor"
        );
    }
}
