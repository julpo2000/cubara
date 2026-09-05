//! The player: position, look direction, and (phase 1) free-fly movement.
//!
//! Moved here from `cubara-render`'s old `FlyCamera` (block 1.6, issue #57):
//! movement is gameplay, and it now runs inside [`crate::Sim::tick`] at a
//! fixed timestep instead of once per rendered frame at a variable one
//! (`ARCHITECTURE.md` Rule 1). The renderer no longer owns any of this --
//! it receives a pose to render from, which is the boundary Rule 3 draws.

use crate::crafting::Crafting;
use crate::inventory::Inventory;
use cubara_voxel::{Angle, Fixed, FixedVec3};
use glam::Vec3;

use crate::input::InputFrame;

/// Look sensitivity, as a binary angle per pixel of mouse motion.
///
/// **Public, because it now belongs to the client.** `InputFrame::look_delta`
/// is an [`Angle`] rather than pixels (`docs/RESEARCH_MULTIPLAYER.md` §3.5:
/// nothing that crosses the wire is a float), so the pixels-to-angle
/// conversion happens before the input is sent -- which is also where it
/// belongs, since sensitivity is a setting on the machine holding the mouse.
///
/// 0.0022 radians per pixel, as a binary angle: `0.0022 / τ × 2³²`.
pub const SENSITIVITY_PER_PIXEL: i32 = 1_503_844;
/// Movement speed through the world, in blocks per second.
const SPEED: f32 = 24.0;
/// Pitch is clamped just short of straight up/down to avoid the view flipping.
/// ~88°, as a binary angle.
const PITCH_LIMIT: Angle = Angle::from_raw(1_052_690_524);

/// A stable, never-reused player identifier (block 2.10).
///
/// A counter in world state, assigned on join. **Never reused**, and never
/// derived from a position in a list — the same decision [`crate::EntityKey`]
/// already makes, for the same reason: anything depending on allocation history
/// makes two worlds that ran the same events disagree, and Rule 1 is the
/// keystone.
///
/// It is also the iteration order everything else leans on. `Sim` keeps players
/// in a `BTreeMap` keyed by this, so "which player ticked first" is a property
/// of the id rather than of a hash seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlayerId(pub u64);

impl PlayerId {
    /// The player a single-player world has, and the one a save written before
    /// block 2.10 migrates into. Named rather than assumed: code meaning "the
    /// player this client drives" should say so instead of writing `0`.
    pub const LOCAL: PlayerId = PlayerId(0);
}

/// Everything one tick of physics reads and writes about a player, as a value.
///
/// Block 2.12b: the payload of the server's correction to a client, and the
/// state a client rewinds to before replaying its unacknowledged input (block
/// 2.13). Those two uses decide the field list, and they decide it strictly:
/// **anything `Sim::tick` touches has to be in here.**
///
/// Leave `velocity` out and a falling player resumes from a standstill, so the
/// replay lands somewhere the server never went and the next correction yanks
/// them back -- a permanent twitch, which is worse than no prediction at all.
/// `fall_distance` and `ticks_since_damage` are in for the same reason: without
/// them a client mispredicts fall damage and regeneration.
///
/// It lives in this crate rather than in `cubara-server` deliberately. Partly
/// because `yaw`, `pitch` and `free_fly` are `pub(crate)` and a pose cannot be
/// rebuilt from outside. Mostly because *which fields a tick changes* is
/// knowledge belonging to the crate that owns the tick: whoever adds a physics
/// field later should be made to decide, in this file, whether it is authority.
///
/// `spawn`, `inventory` and `crafting` are **not** here. They change through
/// deliberate acts rather than through physics, so they are not part of what a
/// replay reconstructs; they reach a client by other means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerState {
    pub pos: FixedVec3,
    pub velocity: FixedVec3,
    pub yaw: Angle,
    pub pitch: Angle,
    pub on_ground: bool,
    pub free_fly: bool,
    pub fall_distance: Fixed,
    pub health: u8,
    pub ticks_since_damage: u32,
}

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
    pub pos: FixedVec3,
    /// Blocks/second. Driven by gravity and walking input in walking mode;
    /// held at zero in free-fly (there's nothing to carry across the mode
    /// switch back).
    pub velocity: FixedVec3,
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
    /// Heading around +Y. 0 looks toward −Z. `pub(crate)`, not private:
    /// `crate::hash::WorldHash` needs the raw angle (`look_dir()` only exposes
    /// the derived unit vector) to include orientation in the world-state hash
    /// (issue #90).
    ///
    /// An [`Angle`], not radians: this feeds the ray that decides which block
    /// gets broken, so it is authority, and `docs/RESEARCH_MULTIPLAYER.md` §3.5
    /// requires authority to be integers. Wrapping is exact, so turning for an
    /// hour does not drift.
    pub(crate) yaw: Angle,
    /// Up/down angle, clamped to [`PITCH_LIMIT`]. Same reason as [`Self::yaw`].
    pub(crate) pitch: Angle,
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
    pub fall_distance: Fixed,
    /// Where death returns the player to (§13.4). Set when the player is
    /// created, at the position they start from.
    pub spawn: FixedVec3,
    /// The block this player is looking at, within [`crate::REACH`] --
    /// recomputed every tick from their own raycast. `None` when nothing solid
    /// is in reach.
    ///
    /// Moved here from `Sim` in block 2.10. It was never world-level state: it
    /// is derived per player from where *that* player is looking, and a world
    /// with several players has several answers to it.
    pub target: Option<[i32; 3]>,
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
pub const SAFE_FALL: Fixed = Fixed::from_raw(3 * cubara_voxel::fixed::ONE);

/// Damage per block fallen beyond [`SAFE_FALL`]. Tuning.
pub const FALL_DAMAGE_PER_BLOCK: u8 = 1;

impl Player {
    pub fn new(pos: FixedVec3, yaw: Angle, pitch: Angle) -> Self {
        Self {
            pos,
            velocity: FixedVec3::ZERO,
            on_ground: false,
            free_fly: false,
            yaw,
            pitch: pitch.clamp(PITCH_LIMIT),
            inventory: Inventory::new(),
            crafting: Crafting::default(),
            health: MAX_HEALTH,
            ticks_since_damage: 0,
            fall_distance: Fixed::ZERO,
            spawn: pos,
            target: None,
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
        self.velocity = FixedVec3::ZERO;
        self.fall_distance = Fixed::ZERO;
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
    pub fn fall_damage_for(distance: Fixed) -> u8 {
        let beyond = (distance - SAFE_FALL).floor_block();
        if beyond <= 0 {
            return 0;
        }
        // Saturating on both the cast and the multiply: a fall from the top of
        // an unbounded world is a very large number, and `u8` is not.
        (beyond as u32)
            .saturating_mul(FALL_DAMAGE_PER_BLOCK as u32)
            .min(u8::MAX as u32) as u8
    }

    /// Whether the player is currently in free-fly (noclip) debug mode
    /// rather than walking under gravity. Read-only outside this crate --
    /// only [`InputFrame::toggle_fly`], via [`crate::Sim::tick`], changes it.
    pub fn is_free_fly(&self) -> bool {
        self.free_fly
    }

    /// Unit view direction from the current yaw/pitch, in [`Fixed`].
    ///
    /// Computed by [`cubara_voxel::angle::look_dir`]'s own integer
    /// trigonometry, not by the platform's `sin`/`cos`. That is the whole point
    /// of the migration: this vector decides which block a click breaks, and
    /// `sin` and `cos` are among the least portable functions in any standard
    /// library (§3.5).
    /// This player's physics state, as a value (block 2.12b).
    pub fn state(&self) -> PlayerState {
        PlayerState {
            pos: self.pos,
            velocity: self.velocity,
            yaw: self.yaw,
            pitch: self.pitch,
            on_ground: self.on_ground,
            free_fly: self.free_fly,
            fall_distance: self.fall_distance,
            health: self.health,
            ticks_since_damage: self.ticks_since_damage,
        }
    }

    /// Put this player back into `state`, leaving everything else alone.
    ///
    /// What a client does before replaying its unacknowledged input. Inventory,
    /// crafting and spawn survive untouched, because they are not physics and a
    /// replay must not rewind them -- an item picked up between the correction
    /// being sent and it arriving is still picked up.
    pub fn restore(&mut self, state: PlayerState) {
        self.pos = state.pos;
        self.velocity = state.velocity;
        self.yaw = state.yaw;
        self.pitch = state.pitch;
        self.on_ground = state.on_ground;
        self.free_fly = state.free_fly;
        self.fall_distance = state.fall_distance;
        self.health = state.health;
        self.ticks_since_damage = state.ticks_since_damage;
    }

    /// Where this player is looking, as the two angles themselves.
    ///
    /// The fields stay `pub(crate)` -- they are only ever *changed* through
    /// `apply_look`, so that the pitch clamp cannot be bypassed -- but reading
    /// them is another matter: block 2.11 replicates other players' poses, and
    /// `cubara-server` cannot send what it cannot see.
    pub fn yaw(&self) -> Angle {
        self.yaw
    }

    pub fn pitch(&self) -> Angle {
        self.pitch
    }

    pub fn look_dir(&self) -> [Fixed; 3] {
        cubara_voxel::angle::look_dir(self.yaw, self.pitch)
    }

    /// [`look_dir`](Self::look_dir) as floats, for the raycast and the camera.
    ///
    /// A conversion *out* of the exact representation, at the point of use.
    /// Integer-to-float is exact and deterministic; what was not deterministic
    /// was the trigonometry, and that has already happened by the time this is
    /// called.
    pub fn look_dir_f32(&self) -> Vec3 {
        let d = self.look_dir();
        Vec3::new(d[0].to_f32(), d[1].to_f32(), d[2].to_f32())
    }

    /// Turn by `input.look_delta` -- the one piece of input handling shared by
    /// every movement mode.
    ///
    /// The delta arrives already scaled by [`SENSITIVITY_PER_PIXEL`], on the
    /// client. Yaw wraps, which is exact; pitch is subtracted (screen y grows
    /// downward) and clamped, so looking up cannot go over the top and invert
    /// the view.
    ///
    /// The clamp holds for **any** delta, including one far larger than a mouse
    /// can produce -- which matters because this value will arrive from a
    /// client, and §3.4 says a client may never be believed.
    pub(crate) fn apply_look(&mut self, input: &InputFrame) {
        self.yaw = self.yaw.wrapping_add(input.look_delta[0]);
        self.pitch = self
            .pitch
            .wrapping_sub(input.look_delta[1])
            .clamp(PITCH_LIMIT);
    }

    /// Horizontal (pitch-ignored) `(forward, right)` unit vectors from the
    /// current yaw -- what walking moves along, since looking up/down must
    /// not tilt you into the ground or the sky. `right` is also what
    /// [`Self::apply_free_fly`] strafes along.
    pub(crate) fn horizontal_axes(&self) -> (Vec3, Vec3) {
        let (f, r) = cubara_voxel::angle::horizontal_axes(self.yaw);
        let v = |a: [Fixed; 3]| Vec3::new(a[0].to_f32(), a[1].to_f32(), a[2].to_f32());
        (v(f), v(r))
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

        let look = self.look_dir_f32();
        let (_, right) = self.horizontal_axes();

        let delta =
            look * input.move_axes[2] + right * input.move_axes[0] + Vec3::Y * input.move_axes[1];
        if delta != Vec3::ZERO {
            // Free-fly is a debug mode and never authoritative, so converting
            // through f32 here costs nothing that matters.
            let step = delta.normalize() * SPEED * dt;
            self.pos += FixedVec3::from_f32([step.x, step.y, step.z]);
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
            // The short way round, which wrapping gives for free -- and the one
            // place a float belongs, because this is the render camera (§9).
            yaw: self.yaw.lerp(other.yaw, t),
            pitch: self.pitch.lerp(other.pitch, t),
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
            // Discrete, like health: a block is selected or it is not, and
            // interpolating toward a different one would highlight a block the
            // simulation never targeted.
            target: other.target,
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
        let player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let d = player.look_dir_f32();
        assert!((d - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-4, "{d:?}");
    }

    #[test]
    fn yaw_ninety_degrees_looks_along_positive_x() {
        let player = Player::new(
            cubara_voxel::FixedVec3::ZERO,
            Angle::QUARTER_TURN,
            Angle::ZERO,
        );
        let d = player.look_dir_f32();
        assert!((d - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "{d:?}");
    }

    #[test]
    fn pitch_is_clamped() {
        let mut player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let look_up = InputFrame {
            // Far more than a quarter turn in one tick, which no mouse can
            // produce -- and which a lying client could send, so the clamp has
            // to hold regardless of how big the delta is.
            look_delta: [Angle::ZERO, Angle::from_raw(-Angle::QUARTER_TURN.raw())],
            ..InputFrame::default()
        };
        player.apply_free_fly(&look_up, 1.0);
        assert!(player.pitch <= PITCH_LIMIT && player.pitch >= Angle::from_raw(-PITCH_LIMIT.raw()));
        assert!(player.look_dir()[1] < Fixed::ONE, "never fully vertical");
    }

    #[test]
    fn forward_axis_moves_along_look_dir() {
        let mut player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        player.apply_free_fly(&moving([0.0, 0.0, 1.0]), 1.0);
        // One second of SPEED along −Z.
        assert!(
            player
                .pos
                .distance_squared(FixedVec3::from_f32([0.0, 0.0, -SPEED]))
                < (cubara_voxel::fixed::ONE as i128).pow(2) / 10_000,
            "{:?}",
            player.pos
        );
    }

    #[test]
    fn opposing_axes_cancel() {
        // A caller building InputFrame from held keys already cancels W+S
        // etc. into a zero axis -- this pins that a zero axis truly means no
        // movement on that axis, not that it happens to net out via floats.
        let mut player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        player.apply_free_fly(&moving([0.0, 0.0, 0.0]), 1.0);
        assert_eq!(player.pos, FixedVec3::ZERO);
    }

    #[test]
    fn zero_dt_does_not_move_even_with_input_held() {
        let mut player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        player.apply_free_fly(&moving([1.0, 1.0, 1.0]), 0.0);
        assert_eq!(player.pos, FixedVec3::ZERO);
    }

    #[test]
    fn lerp_at_zero_and_one_returns_the_endpoints() {
        let a = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let b = Player::new(
            FixedVec3::from_blocks(10, 0, 0),
            Angle::from_radians(1.0),
            Angle::from_radians(0.2),
        );

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
        let a = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let b = Player::new(
            FixedVec3::from_blocks(10, 0, 0),
            Angle::from_radians(1.0),
            Angle::from_radians(0.2),
        );
        let mid = a.lerp(&b, 0.5);
        assert_eq!(
            mid.pos,
            FixedVec3::from_blocks(5, 0, 0),
            "halfway between two whole-block positions is exact in integers"
        );
        assert!((mid.yaw.to_radians() - 0.5).abs() < 1e-5);
        assert!((mid.pitch.to_radians() - 0.1).abs() < 1e-5);
    }

    #[test]
    fn a_fall_inside_the_safe_distance_costs_nothing() {
        // The boundary, from both sides. SAFE_FALL is 3, so 3 blocks is free
        // and 4 costs one point.
        assert_eq!(Player::fall_damage_for(Fixed::from_f32(0.0)), 0);
        assert_eq!(Player::fall_damage_for(Fixed::from_f32(3.0)), 0);
        assert_eq!(Player::fall_damage_for(Fixed::from_f32(3.99)), 0);
        assert_eq!(Player::fall_damage_for(Fixed::from_f32(4.0)), 1);
        assert_eq!(Player::fall_damage_for(Fixed::from_f32(10.0)), 7);
    }

    #[test]
    fn a_long_enough_fall_kills_and_respawns() {
        let mut p = Player::new(FixedVec3::from_blocks(5, 70, 5), Angle::ZERO, Angle::ZERO);
        p.pos = FixedVec3::from_blocks(5, 2, 5);

        // 23 blocks: 20 points beyond the safe distance, which is exactly full
        // health.
        let died = p.take_damage(Player::fall_damage_for(Fixed::from_f32(23.0)));

        assert!(died);
        assert_eq!(p.health, MAX_HEALTH, "respawned at full health");
        assert_eq!(p.pos, FixedVec3::from_blocks(5, 70, 5), "back at spawn");
    }

    #[test]
    fn death_keeps_the_inventory() {
        // The owner's decision (§13.4). Worth its own test because the obvious
        // alternative -- dropping everything -- is what the genre usually does,
        // and a future change here should have to say so out loud.
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let before = p.inventory;
        p.take_damage(MAX_HEALTH);
        assert_eq!(p.health, MAX_HEALTH);
        assert_eq!(p.inventory, before, "items survive death");
    }

    #[test]
    fn a_heart_comes_back_every_interval_without_damage() {
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
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
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
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
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        p.take_damage(4);
        for _ in 0..REGEN_INTERVAL - 1 {
            p.tick_regeneration();
        }
        p.take_damage(Player::fall_damage_for(Fixed::from_f32(2.0))); // a 2-block hop: 0 points
        p.tick_regeneration();
        assert_eq!(p.health, MAX_HEALTH - 4 + HEART, "healed on schedule");
    }

    #[test]
    fn regeneration_stops_at_full_health() {
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        for _ in 0..REGEN_INTERVAL * 3 {
            p.tick_regeneration();
        }
        assert_eq!(p.health, MAX_HEALTH);
    }

    #[test]
    fn health_never_wraps_past_zero() {
        let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        // Far more than full health in one hit: saturating, then a respawn.
        p.take_damage(u8::MAX);
        assert_eq!(p.health, MAX_HEALTH);
    }

    /// The property the whole migration is for: turning for an hour and coming
    /// back lands *exactly* where it started.
    ///
    /// With radians in `f32` this is false — every `+=` rounds, and the error
    /// accumulates for as long as the session lasts. With binary angles the
    /// wrap is the integer's own, so there is nothing to accumulate.
    #[test]
    fn a_session_of_turning_returns_exactly_to_where_it_started() {
        let mut player = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
        let start = player.yaw;

        // Roughly an hour of continuous mouse movement at 60 Hz, then the same
        // again in the other direction.
        let right = InputFrame {
            look_delta: [Angle::from_raw(37 * SENSITIVITY_PER_PIXEL), Angle::ZERO],
            ..InputFrame::default()
        };
        let left = InputFrame {
            look_delta: [Angle::from_raw(-37 * SENSITIVITY_PER_PIXEL), Angle::ZERO],
            ..InputFrame::default()
        };
        for _ in 0..216_000 {
            player.apply_look(&right);
        }
        for _ in 0..216_000 {
            player.apply_look(&left);
        }

        assert_eq!(player.yaw, start, "yaw drifted over a session of turning");
    }

    /// Two players given the same look input reach the same direction, bit for
    /// bit -- which is the claim the netcode needs and the one `f32` angles
    /// could not make across compilers.
    ///
    /// Asserted on the raw integers rather than on the float view: the floats
    /// are derived, and derived values agreeing is weaker than the state
    /// agreeing.
    #[test]
    fn the_same_look_input_reaches_the_same_direction_exactly() {
        let script: Vec<i32> = (0..500).map(|i| (i * 37) % 211 - 105).collect();
        let run = || {
            let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
            for &px in &script {
                p.apply_look(&InputFrame {
                    look_delta: [
                        Angle::from_raw(px * SENSITIVITY_PER_PIXEL),
                        Angle::from_raw(-px * SENSITIVITY_PER_PIXEL),
                    ],
                    ..InputFrame::default()
                });
            }
            (p.yaw, p.pitch, p.look_dir())
        };
        assert_eq!(run(), run());
    }

    /// Pitch cannot be pushed over the top, however large the delta -- a client
    /// sends this value, and §3.4 says a client may never be believed.
    #[test]
    fn no_look_delta_can_push_pitch_past_the_limit() {
        for raw in [i32::MAX, i32::MIN, 1 << 30, -(1 << 30), 12_345_678] {
            let mut p = Player::new(cubara_voxel::FixedVec3::ZERO, Angle::ZERO, Angle::ZERO);
            for _ in 0..8 {
                p.apply_look(&InputFrame {
                    look_delta: [Angle::ZERO, Angle::from_raw(raw)],
                    ..InputFrame::default()
                });
                assert!(
                    p.pitch <= PITCH_LIMIT && p.pitch >= Angle::from_raw(-PITCH_LIMIT.raw()),
                    "delta {raw} escaped the clamp: {:?}",
                    p.pitch
                );
            }
        }
    }
}
