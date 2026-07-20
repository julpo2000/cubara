//! First-person free-fly camera.
//!
//! A yaw/pitch camera driven by keyboard + mouse: WASD moves in the look plane,
//! Space/Shift fly up/down, and mouse motion turns. It owns just enough input state
//! (which movement keys are held) to advance itself by `dt` each frame. The renderer
//! feeds it raw input events and reads back a view-projection matrix; headless paths
//! (bench/screenshot) use their own scripted camera instead. See issue #49.

use glam::{Mat4, Vec3};
use winit::keyboard::KeyCode;

/// Look sensitivity, radians of turn per pixel of mouse motion.
const SENSITIVITY: f32 = 0.0022;
/// Movement speed through the world, in blocks per second.
const SPEED: f32 = 24.0;
/// Pitch is clamped just short of straight up/down to avoid the view flipping.
const PITCH_LIMIT: f32 = 1.54; // ~88°

/// A free-fly first-person camera.
pub struct FlyCamera {
    pub pos: Vec3,
    /// Heading around +Y, radians. 0 looks toward −Z.
    yaw: f32,
    /// Up/down angle, radians, clamped to [`PITCH_LIMIT`].
    pitch: f32,
    /// Which movement keys are currently held.
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

impl FlyCamera {
    pub fn new(pos: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            pos,
            yaw,
            pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT),
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
        }
    }

    /// Unit view direction from the current yaw/pitch.
    pub fn look_dir(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * sy, sp, -cp * cy)
    }

    /// Record a movement key going down/up. Unmapped keys are ignored (returns
    /// whether the key was one the camera cares about).
    pub fn key(&mut self, key: KeyCode, pressed: bool) -> bool {
        let slot = match key {
            KeyCode::KeyW | KeyCode::ArrowUp => &mut self.forward,
            KeyCode::KeyS | KeyCode::ArrowDown => &mut self.back,
            KeyCode::KeyA | KeyCode::ArrowLeft => &mut self.left,
            KeyCode::KeyD | KeyCode::ArrowRight => &mut self.right,
            KeyCode::Space => &mut self.up,
            KeyCode::ShiftLeft | KeyCode::ShiftRight | KeyCode::ControlLeft => &mut self.down,
            _ => return false,
        };
        *slot = pressed;
        true
    }

    /// Turn the camera by a mouse motion delta (pixels). Right/down are positive.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * SENSITIVITY;
        self.pitch = (self.pitch - dy * SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Advance the position by the held movement keys over `dt` seconds. W/S move
    /// along the full look direction (fly toward where you look), A/D along the
    /// horizontal right vector, Space/Shift along world up.
    pub fn update(&mut self, dt: f32) {
        let look = self.look_dir();
        let (sy, cy) = self.yaw.sin_cos();
        let right = Vec3::new(cy, 0.0, sy); // horizontal, = cross(horizontal forward, +Y)

        let mut delta = Vec3::ZERO;
        if self.forward {
            delta += look;
        }
        if self.back {
            delta -= look;
        }
        if self.right {
            delta += right;
        }
        if self.left {
            delta -= right;
        }
        if self.up {
            delta += Vec3::Y;
        }
        if self.down {
            delta -= Vec3::Y;
        }
        if delta != Vec3::ZERO {
            self.pos += delta.normalize() * SPEED * dt;
        }
    }

    /// The view-projection matrix for the current pose at the given aspect ratio.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = Mat4::perspective_rh(60f32.to_radians(), aspect, 0.1, 2000.0);
        let view = Mat4::look_at_rh(self.pos, self.pos + self.look_dir(), Vec3::Y);
        proj * view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_orientation_looks_along_negative_z() {
        let cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        let d = cam.look_dir();
        assert!((d - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5, "{d:?}");
    }

    #[test]
    fn yaw_ninety_degrees_looks_along_positive_x() {
        let cam = FlyCamera::new(Vec3::ZERO, std::f32::consts::FRAC_PI_2, 0.0);
        let d = cam.look_dir();
        assert!((d - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    }

    #[test]
    fn pitch_is_clamped() {
        let mut cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        cam.mouse_look(0.0, -100000.0); // slam the look up
        assert!(cam.pitch <= PITCH_LIMIT && cam.pitch >= -PITCH_LIMIT);
        assert!(cam.look_dir().y < 1.0, "never fully vertical");
    }

    #[test]
    fn forward_key_moves_along_look_dir() {
        let mut cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        cam.key(KeyCode::KeyW, true);
        cam.update(1.0);
        // One second of SPEED along −Z.
        assert!(
            (cam.pos - Vec3::new(0.0, 0.0, -SPEED)).length() < 1e-4,
            "{:?}",
            cam.pos
        );
    }

    #[test]
    fn opposing_keys_cancel() {
        let mut cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        cam.key(KeyCode::KeyW, true);
        cam.key(KeyCode::KeyS, true);
        cam.key(KeyCode::KeyA, true);
        cam.key(KeyCode::KeyD, true);
        cam.update(1.0);
        assert_eq!(cam.pos, Vec3::ZERO);
    }

    #[test]
    fn releasing_a_key_stops_movement() {
        let mut cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        cam.key(KeyCode::KeyW, true);
        cam.key(KeyCode::KeyW, false);
        cam.update(1.0);
        assert_eq!(cam.pos, Vec3::ZERO);
    }

    #[test]
    fn unmapped_key_is_ignored() {
        let mut cam = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        assert!(!cam.key(KeyCode::KeyP, true));
    }
}
