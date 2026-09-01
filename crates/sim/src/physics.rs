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

use cubara_voxel::fixed::ONE;
use cubara_voxel::{Fixed, FixedVec3};

use crate::input::InputFrame;
use crate::player::Player;

// Every distance below is [`Fixed`] -- an integer count of 1/65536ths of a
// block. Written as exact rationals of `ONE` rather than as `from_f32`, so the
// value in the binary is the value in the comment and does not depend on how a
// literal was rounded.
//
// Velocities are sub-units **per second**; a tick's displacement is
// `velocity / TICKS_PER_SECOND`, an integer division that rounds the same way
// in both directions (see `Fixed::div_floor`).

/// Half the player's horizontal footprint, blocks (full width 0.6).
const HALF_WIDTH: Fixed = Fixed::from_raw(3 * ONE / 10);
/// Collision-box height, feet to head, blocks.
const HEIGHT: Fixed = Fixed::from_raw(18 * ONE / 10);
/// How far above the feet [`Player::pos`] (the eye) sits -- the box is
/// derived from `pos` by subtracting this, so `pos` keeps meaning "the
/// camera" in both movement modes.
///
/// **The round trip is now exact**, which is why [`EPS`] is gone: subtracting
/// and re-adding an integer returns the number you started with, so a box
/// resting on a boundary arrives on the boundary rather than a few ULPs below
/// it.
const EYE_HEIGHT: Fixed = Fixed::from_raw(162 * ONE / 100);
/// The tallest ledge walking climbs without a jump.
const STEP_HEIGHT: Fixed = Fixed::ONE;
/// Ticks per second -- what a per-second velocity is divided by.
const TICKS_PER_SECOND: i64 = 60;
/// Downward acceleration, in sub-units per second **added per tick**:
/// 32 blocks/second² over one tick.
const GRAVITY_PER_TICK: Fixed = Fixed::from_raw(32 * ONE / TICKS_PER_SECOND);
/// Upward speed a jump starts at, blocks/second -- a little over one block
/// of height against gravity.
const JUMP_SPEED: Fixed = Fixed::from_raw(9 * ONE);
/// Horizontal walking speed, blocks/second.
const WALK_SPEED: Fixed = Fixed::from_raw(5 * ONE);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Aabb {
    min: FixedVec3,
    max: FixedVec3,
}

impl Aabb {
    /// A box `half` wide and `half` tall in every direction, centred on
    /// `centre` -- what a dropped item is (§10.4).
    fn cube(centre: FixedVec3, half: Fixed) -> Self {
        let corner = FixedVec3::new(half, half, half);
        Self {
            min: centre - corner,
            max: centre + corner,
        }
    }

    fn centre(&self) -> FixedVec3 {
        FixedVec3::new(
            (self.min.x + self.max.x).div_floor(2),
            (self.min.y + self.max.y).div_floor(2),
            (self.min.z + self.max.z).div_floor(2),
        )
    }

    fn from_feet(feet: FixedVec3) -> Self {
        Self {
            min: FixedVec3::new(feet.x - HALF_WIDTH, feet.y, feet.z - HALF_WIDTH),
            max: FixedVec3::new(feet.x + HALF_WIDTH, feet.y + HEIGHT, feet.z + HALF_WIDTH),
        }
    }

    fn feet(&self) -> FixedVec3 {
        FixedVec3::new(
            (self.min.x + self.max.x).div_floor(2),
            self.min.y,
            (self.min.z + self.max.z).div_floor(2),
        )
    }

    fn translated(&self, delta: FixedVec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }
}

/// Advance one fixed tick of walking physics. Turns by `input.look_delta`
/// like every mode does, applies gravity and a jump impulse, then resolves
/// movement against `is_solid` in the fixed Y, X, Z order.
/// `dt` is gone: the tick length is a constant now, and the per-tick
/// displacement is `velocity / TICKS_PER_SECOND`. Taking a float `dt` would be
/// offering a knob that must never be turned -- a variable timestep is exactly
/// what Rule 1 forbids.
pub(crate) fn step(
    player: &mut Player,
    input: &InputFrame,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) {
    player.apply_look(input);

    if input.jump && player.on_ground {
        player.velocity.y = JUMP_SPEED;
        player.on_ground = false;
    }
    player.velocity.y -= GRAVITY_PER_TICK;

    // **The one float left on this path, and it is deliberate.**
    //
    // The walk direction comes from `yaw` through `sin`/`cos`, which are not
    // portable across platforms and standard libraries. Quantising the result
    // to `Fixed` does not make it portable -- two machines that disagreed
    // before quantising can disagree after.
    //
    // It is still a large improvement, and the distinction matters: what was
    // removed is *accumulation*. A float velocity integrated into a float
    // position drifts without bound, and two machines end up in different
    // places. Here a difference in `wish` can perturb one tick's velocity by a
    // sub-unit and no more -- it cannot compound, because the position it feeds
    // is exact.
    //
    // Angles are the remaining half of the migration, named in
    // `docs/RESEARCH_MULTIPLAYER.md` §3.5 rather than left to be discovered
    // when netcode arrives.
    let (forward, right) = player.horizontal_axes();
    let wish = forward * input.move_axes[2] + right * input.move_axes[0];
    let wish = if wish != Vec3::ZERO {
        wish.normalize()
    } else {
        wish
    };
    let walk = |component: f32| Fixed::from_raw((component * WALK_SPEED.raw() as f32) as i64);
    player.velocity.x = walk(wish.x);
    player.velocity.z = walk(wish.z);

    let was_on_ground = player.on_ground;
    let feet = FixedVec3::new(player.pos.x, player.pos.y - EYE_HEIGHT, player.pos.z);
    let aabb = Aabb::from_feet(feet);

    // A tick's displacement from a per-second velocity.
    let per_tick = |v: Fixed| v.div_floor(TICKS_PER_SECOND);

    let before_y = aabb.feet().y;
    let (aabb, y_blocked, y_negative) = move_axis(aabb, 1, per_tick(player.velocity.y), &is_solid);
    player.on_ground = y_blocked && y_negative;
    if y_blocked {
        player.velocity.y = Fixed::ZERO;
    }

    // Fall damage is measured in **distance fallen**, not impact speed
    // (`docs/PHASE2_ARCHITECTURE.md` §13.3): distance is what a player can
    // judge before jumping, speed is a number they cannot see. Accumulate
    // while descending and airborne; spend it on landing.
    let dropped = before_y - aabb.feet().y;
    if !player.on_ground && dropped > Fixed::ZERO {
        player.fall_distance += dropped;
    }
    let aabb = move_axis_with_step(
        aabb,
        0,
        per_tick(player.velocity.x),
        was_on_ground,
        &is_solid,
    );
    let aabb = move_axis_with_step(
        aabb,
        2,
        per_tick(player.velocity.z),
        was_on_ground,
        &is_solid,
    );

    let feet = aabb.feet();
    player.pos = FixedVec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z);

    // **Fall damage is applied last, after the position write above.**
    //
    // `take_damage` can kill, and dying respawns -- which sets `pos` and
    // `velocity`. Anything that writes `player.pos` afterwards silently undoes
    // that, leaving a dead player standing at full health exactly where they
    // fell. This was a real bug: applying the damage during the Y phase, which
    // reads naturally, put it *before* the write and the respawn never took
    // effect. `a_lethal_fall_actually_moves_the_player_to_spawn` pins it.
    if player.on_ground {
        let damage = Player::fall_damage_for(player.fall_distance);
        player.fall_distance = Fixed::ZERO;
        player.take_damage(damage);
    }
}

/// Half-extent of a dropped item's collision box. Small enough to rest in a
/// one-block hole, big enough not to tunnel through a floor at terminal
/// velocity within one tick.
pub(crate) const ITEM_HALF: Fixed = Fixed::from_raw(ONE / 8);

/// Advance one fixed tick of a dropped item (`PHASE2_ARCHITECTURE.md` §10.4).
///
/// **Reuses [`move_axis`]**, the same swept resolution the player walks with,
/// rather than integrating separately -- Rule 5. An item is simply a smaller
/// box with no input and no step-up.
///
/// Returns whether it came to rest on the ground, which is what stops a
/// resting item from accumulating downward velocity forever.
pub(crate) fn step_item(
    pos: &mut FixedVec3,
    velocity: &mut FixedVec3,
    is_solid: impl Fn(i32, i32, i32) -> bool,
) -> bool {
    let per_tick = |v: Fixed| v.div_floor(TICKS_PER_SECOND);
    velocity.y -= GRAVITY_PER_TICK;
    let aabb = Aabb::cube(*pos, ITEM_HALF);

    let (aabb, y_blocked, y_negative) = move_axis(aabb, 1, per_tick(velocity.y), &is_solid);
    let on_ground = y_blocked && y_negative;
    if y_blocked {
        velocity.y = Fixed::ZERO;
    }
    // Horizontal drift, so an item pushed out of a broken block does not stack
    // exactly on its neighbour. No step-up: items do not climb stairs.
    let aabb = move_axis(aabb, 0, per_tick(velocity.x), &is_solid).0;
    let aabb = move_axis(aabb, 2, per_tick(velocity.z), &is_solid).0;

    if on_ground {
        // Friction, so a dropped stack settles instead of sliding forever.
        velocity.x = Fixed::ZERO;
        velocity.z = Fixed::ZERO;
    }
    *pos = aabb.centre();
    on_ground
}

/// The two axis indices other than `axis` (0 = x, 1 = y, 2 = z).
fn other_axes(axis: usize) -> [usize; 2] {
    match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    }
}

fn axis_delta(axis: usize, d: Fixed) -> FixedVec3 {
    let mut v = FixedVec3::ZERO;
    v[axis] = d;
    v
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
    delta: Fixed,
    is_solid: &impl Fn(i32, i32, i32) -> bool,
) -> (Aabb, bool, bool) {
    if delta == Fixed::ZERO {
        return (aabb, false, false);
    }
    let negative = delta < Fixed::ZERO;
    let [u, v] = other_axes(axis);
    // **The epsilon is gone, and so is the reason for it.**
    //
    // It existed because the bounds came from a `pos <-> feet` round trip
    // through `EYE_HEIGHT` in `f32`, so a box resting exactly on an integer
    // boundary could arrive a few ULPs below it and `floor()` would read solid
    // ground as open air. In integers that round trip is exact: subtract a
    // number and add it back and you have the number you started with.
    //
    // What is left is the honest rule, which the epsilon was approximating: the
    // box occupies the half-open span `[min, max)`, so the last cell it touches
    // is the one containing `max` minus one sub-unit. A box flush against a
    // face does not occupy the cell beyond it.
    let last = |f: Fixed| (f - Fixed::from_raw(1)).floor_block();
    let u_lo = aabb.min[u].floor_block();
    let u_hi = last(aabb.max[u]);
    let v_lo = aabb.min[v].floor_block();
    let v_hi = last(aabb.max[v]);

    let leading = if negative {
        aabb.min[axis]
    } else {
        aabb.max[axis]
    };
    let target = leading + delta;
    let (start_layer, end_layer, step) = if negative {
        // Moving down/left: the first cell that can stop us is the one *below*
        // the leading edge, which for an edge exactly on a boundary is one
        // lower -- `ceil - 1` says both cases at once.
        (leading.ceil_block() - 1, target.ceil_block() - 1, -1)
    } else {
        (leading.floor_block(), target.floor_block(), 1)
    };

    let mut layer = start_layer;
    while (step == 1 && layer <= end_layer) || (step == -1 && layer >= end_layer) {
        for a in u_lo..=u_hi {
            for b in v_lo..=v_hi {
                let coord = voxel_coord(axis, layer, [(u, a), (v, b)]);
                if is_solid(coord[0], coord[1], coord[2]) {
                    let contact = Fixed::from_blocks(if negative { layer + 1 } else { layer });
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
    delta: Fixed,
    can_step: bool,
    is_solid: &impl Fn(i32, i32, i32) -> bool,
) -> Aabb {
    let (flat, blocked, _) = move_axis(aabb, axis, delta, is_solid);
    if delta == Fixed::ZERO || !blocked || !can_step {
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
    let feet = FixedVec3::new(player.pos.x, player.pos.y - EYE_HEIGHT, player.pos.z);
    let aabb = Aabb::from_feet(feet);
    // The same half-open span `move_axis` uses -- see its comment. No epsilon:
    // the last cell the box touches is the one containing `max` minus one
    // sub-unit.
    let last = |f: Fixed| (f - Fixed::from_raw(1)).floor_block();
    let x_lo = aabb.min.x.floor_block();
    let x_hi = last(aabb.max.x);
    let y_lo = aabb.min.y.floor_block();
    let y_hi = last(aabb.max.y);
    let z_lo = aabb.min.z.floor_block();
    let z_hi = last(aabb.max.z);
    (x_lo..=x_hi)
        .flat_map(|x| (y_lo..=y_hi).flat_map(move |y| (z_lo..=z_hi).map(move |z| (x, y, z))))
        .any(|(x, y, z)| is_solid(x, y, z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::Angle;

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
        let mut player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 50.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        );
        step(&mut player, &no_input(), flat_floor);
        assert!(player.velocity.y < Fixed::ZERO);
        assert!(player.pos.y < Fixed::from_blocks(50));
    }

    #[test]
    fn a_player_falling_onto_a_floor_comes_to_rest_on_ground() {
        let mut player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 10.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        );
        for _ in 0..600 {
            step(&mut player, &no_input(), flat_floor);
            assert!(
                !aabb_overlaps(&player, &flat_floor),
                "tunnelled into the floor"
            );
        }
        assert!(player.on_ground);
        assert_eq!(player.velocity.y, Fixed::ZERO);
    }

    #[test]
    fn never_tunnels_through_the_floor_across_a_spread_of_velocities() {
        // Fifteen blocks up, not thirty. The subject here is the **sweep** --
        // that a fast-moving box never passes through the floor -- and speed is
        // what varies. Height only has to leave room to accelerate.
        //
        // Thirty was fine until block 2.9a made falling hurt: a 30-block drop
        // deals 26 damage against 20 health, so the player died on landing and
        // respawned back at the drop point, which is where `Player::new` put
        // their spawn. The test then looped forever without ever landing. That
        // was the test correctly objecting to something real -- see
        // `the_game_does_not_start_by_killing_the_player` -- but it is not what
        // *this* test is about.
        for speed in [1.0_f32, 10.0, 32.0, 60.0, 100.0, 250.0] {
            let mut player = Player::new(
                cubara_voxel::FixedVec3::from_f32([0.5, 15.0, 0.5]),
                Angle::ZERO,
                Angle::ZERO,
            );
            player.velocity.y = Fixed::from_f32(-speed);
            for _ in 0..600 {
                step(&mut player, &no_input(), flat_floor);
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
        let mut player = Player::new(
            FixedVec3::new(
                Fixed::from_raw(ONE / 2),
                Fixed::ONE + EYE_HEIGHT,
                Fixed::from_raw(ONE / 2),
            ),
            Angle::ZERO,
            Angle::ZERO,
        );
        // Settle onto the floor first.
        for _ in 0..60 {
            step(&mut player, &no_input(), flat_floor);
        }
        assert!(player.on_ground);

        let jump = InputFrame {
            jump: true,
            ..InputFrame::default()
        };
        step(&mut player, &jump, flat_floor);
        assert!(
            player.velocity.y > Fixed::ZERO,
            "jump applies an upward impulse from the ground"
        );
        assert!(!player.on_ground);

        // Airborne now -- a second jump edge this tick does nothing further.
        let v_after_first = player.velocity.y;
        step(&mut player, &jump, flat_floor);
        assert!(
            player.velocity.y < v_after_first,
            "no double-jump: gravity alone should reduce velocity, not another impulse"
        );
    }

    #[test]
    fn walking_forward_moves_horizontally_at_walk_speed() {
        let mut player = Player::new(
            FixedVec3::new(
                Fixed::from_raw(ONE / 2),
                Fixed::ONE + EYE_HEIGHT,
                Fixed::from_raw(ONE / 2),
            ),
            Angle::ZERO,
            Angle::ZERO,
        );
        for _ in 0..60 {
            step(&mut player, &no_input(), flat_floor);
        }
        let z_before = player.pos.z;
        let forward = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            ..InputFrame::default()
        };
        for _ in 0..60 {
            step(&mut player, &forward, flat_floor);
        }
        // Yaw 0 looks toward −Z (matching `Player::look_dir`).
        assert!(player.pos.z < z_before, "moved toward −Z");
        assert!(
            (z_before - player.pos.z - WALK_SPEED).abs() < Fixed::from_raw(ONE / 10),
            "about one second at WALK_SPEED"
        );
    }

    #[test]
    fn steps_up_a_one_block_ledge_while_walking_into_it() {
        // Yaw 0 faces −Z (`Player::look_dir`); the ledge sits at z <= −2,
        // so walking forward with no turn walks straight into it.
        let mut player = Player::new(
            FixedVec3::new(
                Fixed::from_raw(ONE / 2),
                Fixed::ONE + EYE_HEIGHT,
                Fixed::from_raw(ONE / 2),
            ),
            Angle::ZERO,
            Angle::ZERO,
        );
        for _ in 0..60 {
            step(&mut player, &no_input(), floor_with_one_block_step);
        }
        assert!(player.on_ground);

        let forward = InputFrame {
            move_axes: [0.0, 0.0, 1.0],
            ..InputFrame::default()
        };
        for _ in 0..180 {
            step(&mut player, &forward, floor_with_one_block_step);
            assert!(
                !aabb_overlaps(&player, &floor_with_one_block_step),
                "tunnelled into the step"
            );
        }
        assert!(
            player.pos.z < Fixed::from_f32(-2.5),
            "climbed onto the raised ledge: z = {:?}",
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
        let mut player = Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 0.5, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        );
        let down = InputFrame {
            move_axes: [0.0, -1.0, 0.0],
            ..InputFrame::default()
        };
        player.apply_free_fly(&down, 1.0);
        assert!(
            player.pos.y < Fixed::from_blocks(-5),
            "free-fly flew straight through the floor"
        );
    }
}

#[cfg(test)]
mod respawn_tests {
    use super::*;
    use crate::player::MAX_HEALTH;
    use crate::InputFrame;
    use cubara_voxel::Angle;

    /// Solid below y = 0.
    fn floor(_x: i32, y: i32, _z: i32) -> bool {
        y < 0
    }

    #[test]
    fn a_lethal_fall_actually_moves_the_player_to_spawn() {
        let spawn = FixedVec3::from_blocks(100, 50, 100);
        let mut p = Player::new(spawn, Angle::ZERO, Angle::ZERO);
        // Falling fast enough, far above the floor, with a killing fall banked.
        p.pos = FixedVec3::from_f32([0.0, 1.9, 0.0]);
        p.velocity = FixedVec3::from_f32([0.0, -60.0, 0.0]);
        p.fall_distance = Fixed::from_blocks(100);

        step(&mut p, &InputFrame::default(), floor);

        assert_eq!(p.health, MAX_HEALTH, "died and respawned at full health");
        assert_eq!(
            p.pos, spawn,
            "but the position was left at the death site instead of spawn"
        );
    }
}
