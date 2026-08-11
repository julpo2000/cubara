//! The player: position, look direction, and (phase 1) free-fly movement.
//!
//! Moved here from `cubara-render`'s old `FlyCamera` (block 1.6, issue #57):
//! movement is gameplay, and it now runs inside [`crate::Sim::tick`] at a
//! fixed timestep instead of once per rendered frame at a variable one
//! (`ARCHITECTURE.md` Rule 1). The renderer no longer owns any of this --
//! it receives a pose to render from, which is the boundary Rule 3 draws.

use glam::Vec3;

use crate::input::InputFrame;

/// Look sensitivity, radians of turn per pixel of mouse motion.
const SENSITIVITY: f32 = 0.0022;
/// Movement speed through the world, in blocks per second.
const SPEED: f32 = 24.0;
/// Pitch is clamped just short of straight up/down to avoid the view flipping.
const PITCH_LIMIT: f32 = 1.54; // ~88°

/// Where the player is and which way they're looking. Free-fly is phase 1's
/// only movement mode (real physics is block 1.7a) -- it survives past that
/// as a debug mode *inside* this same type, not a second implementation on
/// the side (Rule 5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    pub pos: Vec3,
    /// Heading around +Y, radians. 0 looks toward −Z.
    yaw: f32,
    /// Up/down angle, radians, clamped to [`PITCH_LIMIT`].
    pitch: f32,
}

impl Player {
    pub fn new(pos: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            pos,
            yaw,
            pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT),
        }
    }

    /// Unit view direction from the current yaw/pitch.
    pub fn look_dir(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * sy, sp, -cp * cy)
    }

    /// Apply one fixed tick's worth of free-fly movement: turn by
    /// `input.look_delta` (scaled by [`SENSITIVITY`]), then move by
    /// `input.move_axes` at [`SPEED`] blocks/second, `dt` seconds' worth.
    /// `input.move_axes[2]` (forward) moves along the full look direction --
    /// fly toward where you look -- `[0]` (strafe) along the horizontal
    /// right vector, `[1]` (vertical) along world up.
    pub fn apply_free_fly(&mut self, input: &InputFrame, dt: f32) {
        self.yaw += input.look_delta[0] * SENSITIVITY;
        self.pitch =
            (self.pitch - input.look_delta[1] * SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);

        let look = self.look_dir();
        let (sy, cy) = self.yaw.sin_cos();
        let right = Vec3::new(cy, 0.0, sy); // horizontal, = cross(horizontal forward, +Y)

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
    /// back into the sim (Rule 3).
    pub fn lerp(&self, other: &Player, t: f32) -> Player {
        Player {
            pos: self.pos.lerp(other.pos, t),
            yaw: self.yaw + (other.yaw - self.yaw) * t,
            pitch: self.pitch + (other.pitch - self.pitch) * t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(axes: [f32; 3]) -> InputFrame {
        InputFrame {
            move_axes: axes,
            look_delta: [0.0, 0.0],
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
            move_axes: [0.0, 0.0, 0.0],
            look_delta: [0.0, -100_000.0],
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
