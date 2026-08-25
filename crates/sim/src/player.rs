//! The player: position, look direction, and (phase 1) free-fly movement.
//!
//! Moved here from `cubara-render`'s old `FlyCamera` (block 1.6, issue #57):
//! movement is gameplay, and it now runs inside [`crate::Sim::tick`] at a
//! fixed timestep instead of once per rendered frame at a variable one
//! (`ARCHITECTURE.md` Rule 1). The renderer no longer owns any of this --
//! it receives a pose to render from, which is the boundary Rule 3 draws.

use crate::crafting::Crafting;
use crate::inventory::Inventory;
use glam::Vec3;

use crate::input::InputFrame;

/// Look sensitivity, radians of turn per pixel of mouse motion.
const SENSITIVITY: f32 = 0.0022;
/// Movement speed through the world, in blocks per second.
const SPEED: f32 = 24.0;
/// Pitch is clamped just short of straight up/down to avoid the view flipping.
const PITCH_LIMIT: f32 = 1.54; // ~88°

/// Where the player is, which way they're looking, and (block 1.7a) their
/// walking-physics state. Free-fly and walking are two *modes* of this one
/// type, not two competing implementations (Rule 5) -- [`Self::free_fly`]
/// picks which of [`Self::apply_free_fly`] / `crate::physics::step` runs each
/// tick, decided in [`crate::Sim::tick`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    /// The eye/camera position. Walking physics derives its collision box
    /// from this by subtracting a fixed eye height (`crate::physics`), so
    /// this field's meaning doesn't change between modes.
    pub pos: Vec3,
    /// Blocks/second. Driven by gravity and walking input in walking mode;
    /// held at zero in free-fly (there's nothing to carry across the mode
    /// switch back).
    pub velocity: Vec3,
    /// Whether the last walking-physics tick ended standing on solid ground.
    /// Always `false` in free-fly (not meaningful there -- nothing reads it
    /// as "falling").
    pub on_ground: bool,
    /// Free-fly (noclip) debug mode vs. real walking physics. Not `pub`:
    /// toggled only via [`InputFrame::toggle_fly`], consumed inside
    /// [`crate::Sim::tick`], so it stays driven by the recorded input stream
    /// a replay (block 1.8) needs to reproduce -- not a side channel outside
    /// it.
    pub(crate) free_fly: bool,
    /// Heading around +Y, radians. 0 looks toward −Z. `pub(crate)`, not
    /// private: `crate::hash::WorldHash` needs the raw angle (`look_dir()`
    /// only exposes the derived unit vector) to include orientation in the
    /// world-state hash (issue #90).
    pub(crate) yaw: f32,
    /// Up/down angle, radians, clamped to [`PITCH_LIMIT`]. Same reason as
    /// [`Self::yaw`].
    pub(crate) pitch: f32,
    /// What the player is carrying. Part of world state, so it is hashed with
    /// everything else (`crate::hash`) -- block 2.9's survival replay test
    /// asserts on the result.
    pub inventory: Inventory,
    /// The crafting grid and what the cursor is holding.
    ///
    /// World state, not screen state: a scripted run that puts planks in a grid
    /// reaches a different inventory than one that does not, so block 2.9's
    /// survival replay has to see it. It is hashed and (from block 2.8) saved
    /// like anything else the player carries.
    pub crafting: Crafting,
}

impl Player {
    pub fn new(pos: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            pos,
            velocity: Vec3::ZERO,
            on_ground: false,
            free_fly: false,
            yaw,
            pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT),
            inventory: Inventory::new(),
            crafting: Crafting::default(),
        }
    }

    /// Whether the player is currently in free-fly (noclip) debug mode
    /// rather than walking under gravity. Read-only outside this crate --
    /// only [`InputFrame::toggle_fly`], via [`crate::Sim::tick`], changes it.
    pub fn is_free_fly(&self) -> bool {
        self.free_fly
    }

    /// Unit view direction from the current yaw/pitch.
    pub fn look_dir(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * sy, sp, -cp * cy)
    }

    /// Turn by `input.look_delta`, scaled by [`SENSITIVITY`] -- the one piece
    /// of input handling shared by every movement mode.
    pub(crate) fn apply_look(&mut self, input: &InputFrame) {
        self.yaw += input.look_delta[0] * SENSITIVITY;
        self.pitch =
            (self.pitch - input.look_delta[1] * SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Horizontal (pitch-ignored) `(forward, right)` unit vectors from the
    /// current yaw -- what walking moves along, since looking up/down must
    /// not tilt you into the ground or the sky. `right` is also what
    /// [`Self::apply_free_fly`] strafes along.
    pub(crate) fn horizontal_axes(&self) -> (Vec3, Vec3) {
        let (sy, cy) = self.yaw.sin_cos();
        (Vec3::new(sy, 0.0, -cy), Vec3::new(cy, 0.0, sy))
    }

    /// Apply one fixed tick's worth of free-fly movement: turn (see
    /// [`Self::apply_look`]), then move by `input.move_axes` at [`SPEED`]
    /// blocks/second, `dt` seconds' worth. `input.move_axes[2]` (forward)
    /// moves along the full look direction -- fly toward where you look --
    /// `[0]` (strafe) along the horizontal right vector, `[1]` (vertical)
    /// along world up. Ignores collision entirely -- that's the point of a
    /// noclip debug mode.
    pub fn apply_free_fly(&mut self, input: &InputFrame, dt: f32) {
        self.apply_look(input);

        let look = self.look_dir();
        let (_, right) = self.horizontal_axes();

        let delta =
            look * input.move_axes[2] + right * input.move_axes[0] + Vec3::Y * input.move_axes[1];
        if delta != Vec3::ZERO {
            self.pos += delta.normalize() * SPEED * dt;
        }
    }

    /// Linearly interpolate between two ticks' poses by `t` in `0.0..=1.0` --
    /// what lets the renderer show a smooth pose between two 60 Hz sim
    /// states at whatever frame rate it's actually running
    /// (`docs/PHASE1_ARCHITECTURE.md` §9). Render-side only; never written
    /// back into the sim (Rule 3). `velocity`/`on_ground`/`free_fly` aren't
    /// rendered, so they aren't meaningfully interpolatable -- `other`'s
    /// (the current tick's) values pass through unchanged.
    pub fn lerp(&self, other: &Player, t: f32) -> Player {
        Player {
            pos: self.pos.lerp(other.pos, t),
            velocity: other.velocity,
            on_ground: other.on_ground,
            free_fly: other.free_fly,
            yaw: self.yaw + (other.yaw - self.yaw) * t,
            pitch: self.pitch + (other.pitch - self.pitch) * t,
            // Not interpolatable and not rendered from here: the current
            // tick's inventory passes through, like velocity above.
            inventory: other.inventory,
            crafting: other.crafting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(axes: [f32; 3]) -> InputFrame {
        InputFrame {
            move_axes: axes,
            ..InputFrame::default()
        }
    }

    #[test]
    fn default_orientation_looks_along_negative_z() {
        let player = Player::new(Vec3::ZERO, 0.0, 0.0);
        let d = player.look_dir();
        assert!((d - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5, "{d:?}");
    }

    #[test]
    fn yaw_ninety_degrees_looks_along_positive_x() {
        let player = Player::new(Vec3::ZERO, std::f32::consts::FRAC_PI_2, 0.0);
        let d = player.look_dir();
        assert!((d - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    }

    #[test]
    fn pitch_is_clamped() {
        let mut player = Player::new(Vec3::ZERO, 0.0, 0.0);
        let look_up = InputFrame {
            look_delta: [0.0, -100_000.0],
            ..InputFrame::default()
        };
        player.apply_free_fly(&look_up, 1.0);
        assert!(player.pitch <= PITCH_LIMIT && player.pitch >= -PITCH_LIMIT);
        assert!(player.look_dir().y < 1.0, "never fully vertical");
    }

    #[test]
    fn forward_axis_moves_along_look_dir() {
        let mut player = Player::new(Vec3::ZERO, 0.0, 0.0);
        player.apply_free_fly(&moving([0.0, 0.0, 1.0]), 1.0);
        // One second of SPEED along −Z.
        assert!(
            (player.pos - Vec3::new(0.0, 0.0, -SPEED)).length() < 1e-4,
            "{:?}",
            player.pos
        );
    }

    #[test]
    fn opposing_axes_cancel() {
        // A caller building InputFrame from held keys already cancels W+S
        // etc. into a zero axis -- this pins that a zero axis truly means no
        // movement on that axis, not that it happens to net out via floats.
        let mut player = Player::new(Vec3::ZERO, 0.0, 0.0);
        player.apply_free_fly(&moving([0.0, 0.0, 0.0]), 1.0);
        assert_eq!(player.pos, Vec3::ZERO);
    }

    #[test]
    fn zero_dt_does_not_move_even_with_input_held() {
        let mut player = Player::new(Vec3::ZERO, 0.0, 0.0);
        player.apply_free_fly(&moving([1.0, 1.0, 1.0]), 0.0);
        assert_eq!(player.pos, Vec3::ZERO);
    }

    #[test]
    fn lerp_at_zero_and_one_returns_the_endpoints() {
        let a = Player::new(Vec3::ZERO, 0.0, 0.0);
        let b = Player::new(Vec3::new(10.0, 0.0, 0.0), 1.0, 0.2);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
    }

    #[test]
    fn lerp_at_half_is_halfway_between() {
        let a = Player::new(Vec3::ZERO, 0.0, 0.0);
        let b = Player::new(Vec3::new(10.0, 0.0, 0.0), 1.0, 0.2);
        let mid = a.lerp(&b, 0.5);
        assert!((mid.pos - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-5);
        assert!((mid.yaw - 0.5).abs() < 1e-5);
        assert!((mid.pitch - 0.1).abs() < 1e-5);
    }
}
