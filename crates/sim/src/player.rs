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
    /// Health in **points**, `0..=MAX_HEALTH` (`PHASE2_ARCHITECTURE.md` §13.1).
    ///
    /// Points, not hearts: a heart is two points, so half-hearts are
    /// representable. Nothing in the simulation counts in hearts -- that is the
    /// HUD's job, and it is given a points value.
    pub health: u8,
    /// Ticks since the last damage, against [`REGEN_INTERVAL`] (§13.2).
    /// Reset to zero by any damage, so sustained damage means no healing at all
    /// rather than slow healing.
    pub ticks_since_damage: u32,
    /// How far the player has fallen since last touching ground, in blocks
    /// (§13.3). Spent on landing.
    ///
    /// **Not saved.** It is transient and derived: a loaded world starts the
    /// player on the ground, and carrying a half-completed fall across a reload
    /// would be a fall the player never made.
    pub fall_distance: f32,
    /// Where death returns the player to (§13.4). Set when the player is
    /// created, at the position they start from.
    pub spawn: Vec3,
}

/// Full health, in points. Ten hearts of two points each (§13.1).
pub const MAX_HEALTH: u8 = 20;

/// One heart, in points.
pub const HEART: u8 = 2;

/// Ticks of no damage before a heart comes back: 5 seconds at 60 Hz (§13.2).
///
/// Ticks rather than seconds, and compared against a counter rather than a
/// wall-clock instant -- Rule 1, the same reason mining is tick-counted.
pub const REGEN_INTERVAL: u32 = 300;

/// Blocks you may fall without being hurt (§13.3). Tuning.
pub const SAFE_FALL: f32 = 3.0;

/// Damage per block fallen beyond [`SAFE_FALL`]. Tuning.
pub const FALL_DAMAGE_PER_BLOCK: u8 = 1;

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
            health: MAX_HEALTH,
            ticks_since_damage: 0,
            fall_distance: 0.0,
            spawn: pos,
        }
    }

    /// Take `points` of damage, dying if it reaches zero (§13.4).
    ///
    /// Returns whether this killed the player, so the caller can react without
    /// re-deriving it. Damage of zero is *not* damage: it does not reset the
    /// regeneration counter, so a fall inside the safe distance does not
    /// interrupt healing.
    pub fn take_damage(&mut self, points: u8) -> bool {
        if points == 0 {
            return false;
        }
        self.ticks_since_damage = 0;
        self.health = self.health.saturating_sub(points);
        if self.health == 0 {
            self.respawn();
            return true;
        }
        false
    }

    /// Return to spawn at full health, **keeping the inventory** (§13.4, the
    /// owner's decision).
    ///
    /// Velocity is cleared and the fall distance with it, or the player would
    /// arrive at spawn still carrying the fall that killed them and die again
    /// on landing.
    pub fn respawn(&mut self) {
        self.pos = self.spawn;
        self.velocity = Vec3::ZERO;
        self.fall_distance = 0.0;
        self.on_ground = false;
        self.health = MAX_HEALTH;
        self.ticks_since_damage = 0;
    }

    /// One tick of regeneration (§13.2): a heart per [`REGEN_INTERVAL`] ticks
    /// without damage, capped at [`MAX_HEALTH`].
    ///
    /// Not gated on food, because there is no food -- gating it now would mean
    /// inventing hunger, which the owner deferred. When hunger arrives it
    /// becomes the gate, which is one condition rather than a redesign.
    pub fn tick_regeneration(&mut self) {
        if self.health >= MAX_HEALTH {
            // Already full: hold the counter at zero so healing always takes a
            // full interval from the moment it is actually needed.
            self.ticks_since_damage = 0;
            return;
        }
        self.ticks_since_damage += 1;
        if self.ticks_since_damage >= REGEN_INTERVAL {
            self.ticks_since_damage = 0;
            self.health = (self.health + HEART).min(MAX_HEALTH);
        }
    }

    /// How many points a fall of `blocks` deals (§13.3).
    pub fn fall_damage_for(blocks: f32) -> u8 {
        let beyond = (blocks - SAFE_FALL).floor();
        if beyond <= 0.0 {
            return 0;
        }
        (beyond as u32).min(u8::MAX as u32) as u8 * FALL_DAMAGE_PER_BLOCK
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
            // Same: health is a discrete point count, so interpolating it
            // would invent half-points the sim never had.
            health: other.health,
            ticks_since_damage: other.ticks_since_damage,
            fall_distance: other.fall_distance,
            spawn: other.spawn,
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

        // The *interpolated* fields hit the endpoints. Whole-struct equality is
        // deliberately not asserted: `lerp` documents that fields which are not
        // meaningfully interpolatable pass through from `other` (the current
        // tick), so `lerp(.., 0.0)` is only equal to `a` when the two agree on
        // all of them. That happened to be true until block 2.9a gave
        // `Player::new` a `spawn` derived from its position -- at which point
        // the assertion was testing a coincidence, not the contract.
        for (t, want) in [(0.0, &a), (1.0, &b)] {
            let got = a.lerp(&b, t);
            assert_eq!(got.pos, want.pos, "pos at t={t}");
            assert_eq!(got.yaw, want.yaw, "yaw at t={t}");
            assert_eq!(got.pitch, want.pitch, "pitch at t={t}");
        }

        // And the pass-through half of the contract, asserted rather than
        // assumed: the current tick's values, at either end.
        assert_eq!(a.lerp(&b, 0.0).spawn, b.spawn);
        assert_eq!(a.lerp(&b, 0.0).health, b.health);
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

    #[test]
    fn a_fall_inside_the_safe_distance_costs_nothing() {
        // The boundary, from both sides. SAFE_FALL is 3, so 3 blocks is free
        // and 4 costs one point.
        assert_eq!(Player::fall_damage_for(0.0), 0);
        assert_eq!(Player::fall_damage_for(3.0), 0);
        assert_eq!(Player::fall_damage_for(3.99), 0);
        assert_eq!(Player::fall_damage_for(4.0), 1);
        assert_eq!(Player::fall_damage_for(10.0), 7);
    }

    #[test]
    fn a_long_enough_fall_kills_and_respawns() {
        let mut p = Player::new(Vec3::new(5.0, 70.0, 5.0), 0.0, 0.0);
        p.pos = Vec3::new(5.0, 2.0, 5.0);

        // 23 blocks: 20 points beyond the safe distance, which is exactly full
        // health.
        let died = p.take_damage(Player::fall_damage_for(23.0));

        assert!(died);
        assert_eq!(p.health, MAX_HEALTH, "respawned at full health");
        assert_eq!(p.pos, Vec3::new(5.0, 70.0, 5.0), "back at spawn");
    }

    #[test]
    fn death_keeps_the_inventory() {
        // The owner's decision (§13.4). Worth its own test because the obvious
        // alternative -- dropping everything -- is what the genre usually does,
        // and a future change here should have to say so out loud.
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        let before = p.inventory;
        p.take_damage(MAX_HEALTH);
        assert_eq!(p.health, MAX_HEALTH);
        assert_eq!(p.inventory, before, "items survive death");
    }

    #[test]
    fn a_heart_comes_back_every_interval_without_damage() {
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        p.take_damage(6);
        assert_eq!(p.health, 14);

        // One tick short of the interval: still hurt.
        for _ in 0..REGEN_INTERVAL - 1 {
            p.tick_regeneration();
        }
        assert_eq!(p.health, 14, "healed early");

        p.tick_regeneration();
        assert_eq!(p.health, 14 + HEART, "one heart, on the interval tick");

        // And again, for the next one.
        for _ in 0..REGEN_INTERVAL {
            p.tick_regeneration();
        }
        assert_eq!(p.health, 14 + HEART * 2);
    }

    #[test]
    fn damage_restarts_the_regeneration_clock() {
        // Sustained damage means no healing at all, rather than slow healing.
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        p.take_damage(10);
        for _ in 0..REGEN_INTERVAL - 1 {
            p.tick_regeneration();
        }
        p.take_damage(1);
        for _ in 0..REGEN_INTERVAL - 1 {
            p.tick_regeneration();
        }
        assert_eq!(p.health, MAX_HEALTH - 11, "the clock restarted");
    }

    #[test]
    fn a_harmless_fall_does_not_interrupt_healing() {
        // Zero damage is not damage: landing inside the safe distance must not
        // reset the counter, or a player hopping around would never heal.
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        p.take_damage(4);
        for _ in 0..REGEN_INTERVAL - 1 {
            p.tick_regeneration();
        }
        p.take_damage(Player::fall_damage_for(2.0)); // a 2-block hop: 0 points
        p.tick_regeneration();
        assert_eq!(p.health, MAX_HEALTH - 4 + HEART, "healed on schedule");
    }

    #[test]
    fn regeneration_stops_at_full_health() {
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        for _ in 0..REGEN_INTERVAL * 3 {
            p.tick_regeneration();
        }
        assert_eq!(p.health, MAX_HEALTH);
    }

    #[test]
    fn health_never_wraps_past_zero() {
        let mut p = Player::new(Vec3::ZERO, 0.0, 0.0);
        // Far more than full health in one hit: saturating, then a respawn.
        p.take_damage(u8::MAX);
        assert_eq!(p.health, MAX_HEALTH);
    }
}
