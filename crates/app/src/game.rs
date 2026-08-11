//! The game: world state, the simulation, and what input does to them.
//!
//! Deliberately separate from the renderer (`ARCHITECTURE.md` Rule 3). The renderer
//! draws what it is given; it does not decide where the player is looking or which
//! block a click breaks. When those lived on `Renderer` it could place blocks, which
//! is the boundary error the rule names — and the pattern that makes the reference
//! anti-pattern codebase impossible to change one system at a time.
//!
//! Nothing here touches the GPU, so all of it is testable without an adapter. It does
//! own the fixed-timestep accumulator (block 1.6, issue #57): [`Game::advance`] takes
//! a wall-clock `dt` and turns it into zero or more fixed [`cubara_sim::TICK_DT`]
//! steps, but never calls `Instant::now()` itself -- that stays in `main.rs`, per
//! `docs/PHASE1_ARCHITECTURE.md` §9, so this is testable with arbitrary, scripted
//! frame times instead of real wall-clock timing.

use std::sync::Arc;

use cubara_render::CameraPose;
use cubara_sim::{InputFrame, Player, Sim, TICK_DT};
use cubara_voxel::ChunkCoord;
use cubara_world::World;

use winit::keyboard::KeyCode;

/// How far the block-editing ray reaches, in blocks.
const EDIT_REACH: f32 = 6.0;

/// Caps how many fixed steps a single [`Game::advance`] call runs -- a stalled or
/// backgrounded window can hand back a huge `dt` (seconds, not milliseconds), and
/// without a cap the sim would try to fully "catch up" by ticking hundreds of times
/// in one frame, which take longer than real time to run, which produces a bigger
/// backlog next frame: a spiral of death. Past the cap the leftover backlog is
/// dropped, not accumulated -- the sim falls behind wall-clock time rather than
/// locking up trying to chase it.
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Everything the player *is* and *does*: the world they're in and the simulation
/// running against it.
pub struct Game {
    /// The world being played. Behind an [`Arc`] so meshing jobs can carry the exact
    /// snapshot they were queued against; an edit publishes a new one.
    world: Arc<World>,
    sim: Sim,
    /// The player's pose as of the *previous* completed tick -- together with
    /// `sim.player` (the current tick), what [`Game::camera_pose`] interpolates
    /// between for smooth rendering of a 60 Hz sim at any frame rate (§9).
    prev_player: Player,
    /// Wall-clock seconds not yet consumed by a fixed tick. `f64`, not `f32`
    /// like everything else here -- this is the one value that keeps being
    /// added to across a whole play session (thousands of frames), and
    /// `f32`'s ~7 significant digits let rounding error accumulate enough to
    /// tip a close call across a tick boundary a step early or late (this
    /// is exactly what `frame_rate_independent_movement_reaches_the_same_state`
    /// caught: two runs summing to the same total elapsed time, chopped into
    /// frames differently, landed one tick apart with an `f32` accumulator).
    accumulator: f64,
    // Held movement keys, translated into `InputFrame::move_axes` once per
    // `advance` call.
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    /// Mouse motion (pixels) accumulated since the last `advance` call.
    look_delta: (f32, f32),
}

impl Game {
    /// Start above the terrain near the origin, looking out over it and slightly
    /// down (yaw ~35°, gentle downward pitch).
    pub fn new() -> Self {
        let player = Player::new(glam::vec3(0.0, 48.0, 0.0), 0.6, -0.3);
        Self {
            world: Arc::new(World::new()),
            sim: Sim::new(0, player),
            prev_player: player,
            accumulator: 0.0,
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
            look_delta: (0.0, 0.0),
        }
    }

    pub fn world(&self) -> &Arc<World> {
        &self.world
    }

    /// The camera pose to render from: the sim's player pose, interpolated
    /// between its previous and current tick by the accumulator's leftover
    /// fraction. Render-side only (§9) -- never read back into the sim.
    pub fn camera_pose(&self) -> CameraPose {
        let alpha = (self.accumulator / TICK_DT as f64).clamp(0.0, 1.0) as f32;
        let player = self.prev_player.lerp(&self.sim.player, alpha);
        CameraPose {
            eye: player.pos,
            look_dir: player.look_dir(),
        }
    }

    /// Record a movement key going down/up. Unmapped keys are ignored (returns
    /// whether the key was one the game cares about).
    pub fn key_input(&mut self, key: KeyCode, pressed: bool) -> bool {
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

    /// Feed a raw mouse-motion delta (pixels) toward the next tick's look input.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        self.look_delta.0 += dx;
        self.look_delta.1 += dy;
    }

    /// Advance the simulation by `dt` wall-clock seconds: zero or more fixed
    /// [`TICK_DT`] steps, capped at [`MAX_TICKS_PER_FRAME`]. Input is sampled
    /// once (currently-held keys + mouse motion since the last call) and that
    /// same [`InputFrame`] drives every step this call runs -- ticks never read
    /// live input themselves, so a scripted/replayed input sequence (block 1.8)
    /// reproduces exactly.
    pub fn advance(&mut self, dt: f32) {
        let input = InputFrame {
            move_axes: [
                (self.right as i32 - self.left as i32) as f32,
                (self.up as i32 - self.down as i32) as f32,
                (self.forward as i32 - self.back as i32) as f32,
            ],
            look_delta: [self.look_delta.0, self.look_delta.1],
        };
        self.look_delta = (0.0, 0.0);

        self.accumulator += dt as f64;
        let mut ticks = 0;
        while self.accumulator >= TICK_DT as f64 {
            self.prev_player = self.sim.player;
            self.sim.tick(Arc::make_mut(&mut self.world), &input);
            self.accumulator -= TICK_DT as f64;
            ticks += 1;
            if ticks >= MAX_TICKS_PER_FRAME {
                // Spiral-of-death guard: fall behind wall-clock time rather than
                // trying to fully catch up, which would only make the next
                // frame's backlog worse.
                self.accumulator = 0.0;
                break;
            }
        }
    }

    /// Break (`place = false`) or place (`true`) the block the player is looking
    /// at, within [`EDIT_REACH`]. Returns the [`ChunkCoord`] whose geometry is now
    /// stale so the caller can re-mesh it, or `None` if nothing was in reach.
    ///
    /// Placing puts the block against the hit face. Uses the sim's current
    /// (non-interpolated) pose -- editing is a gameplay decision, and
    /// interpolation is a render-only concern (§9) that must never feed back
    /// into it.
    pub fn edit_block(&mut self, place: bool) -> Option<ChunkCoord> {
        let origin = self.sim.player.pos.to_array();
        let dir = self.sim.player.look_dir().to_array();
        let hit = self.world.raycast(origin, dir, EDIT_REACH)?;
        let target = if place {
            [
                hit.block[0] + hit.normal[0],
                hit.block[1] + hit.normal[1],
                hit.block[2] + hit.normal[2],
            ]
        } else {
            hit.block
        };
        // Publishes a fresh snapshot: workers holding the old Arc keep meshing the
        // pre-edit world, and their results are superseded by the re-mesh request.
        Some(Arc::make_mut(&mut self.world).set_block(target[0], target[1], target[2], place))
    }
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaking_clears_the_targeted_block() {
        // No GPU involved — this is why gameplay does not belong on the renderer.
        let mut game = Game::new();
        // Look straight down from above the terrain.
        game.sim.player = Player::new(glam::vec3(0.5, 60.0, 0.5), 0.0, -1.5);
        let hit = game
            .world()
            .raycast([0.5, 60.0, 0.5], [0.0, -1.0, 0.0], 100.0)
            .expect("ground below");

        // Out of reach from 60 blocks up: nothing changes.
        assert_eq!(game.edit_block(false), None);
        assert!(game
            .world()
            .is_solid_at(hit.block[0], hit.block[1], hit.block[2]));
    }

    #[test]
    fn editing_within_reach_marks_a_chunk_dirty() {
        let mut game = Game::new();
        let ground = game
            .world()
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
            .expect("ground below");
        // Stand just above the surface, looking down — now it is within reach.
        let eye = glam::vec3(0.5, ground.block[1] as f32 + 3.5, 0.5);
        game.sim.player = Player::new(eye, 0.0, -1.5);

        let dirty = game.edit_block(false).expect("a block was in reach");
        assert!(
            !game
                .world()
                .is_solid_at(ground.block[0], ground.block[1], ground.block[2]),
            "the targeted block is now air"
        );
        let b = ground.block;
        assert_eq!(
            dirty,
            ChunkCoord::from_world_pos([b[0] as f32, b[1] as f32, b[2] as f32]),
            "the dirty chunk is the one containing the broken block"
        );
    }

    #[test]
    fn key_input_reports_whether_the_key_was_mapped() {
        let mut game = Game::new();
        assert!(game.key_input(KeyCode::KeyW, true));
        assert!(!game.key_input(KeyCode::KeyP, true), "unmapped key");
    }

    #[test]
    fn advance_by_exactly_one_tick_worth_of_time_runs_one_tick() {
        let mut game = Game::new();
        assert_eq!(game.sim.tick, 0);
        game.advance(TICK_DT);
        assert_eq!(game.sim.tick, 1);
    }

    #[test]
    fn sub_tick_dt_does_not_run_a_tick_yet() {
        let mut game = Game::new();
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.sim.tick, 0, "half a tick's worth of time isn't a tick");
        game.advance(TICK_DT * 0.5);
        assert_eq!(game.sim.tick, 1, "the other half completes it");
    }

    #[test]
    fn a_huge_dt_is_capped_not_caught_up_in_one_frame() {
        let mut game = Game::new();
        game.advance(1000.0 * TICK_DT); // a 1000-tick backlog in one call
        assert_eq!(
            game.sim.tick, MAX_TICKS_PER_FRAME as u64,
            "capped at MAX_TICKS_PER_FRAME, not fully caught up"
        );
        assert_eq!(
            game.accumulator, 0.0,
            "the leftover backlog is dropped, not carried into the next frame"
        );
    }

    #[test]
    fn frame_rate_independent_movement_reaches_the_same_state() {
        // The property Rule 1 exists for: the same input, driven by wildly
        // different frame timings that sum to the same elapsed time, must land
        // on identical sim state -- not "close", identical. 1000 ticks, per the
        // issue's own "Done when" (#57).
        let mut steady = Game::new();
        let mut jittery = Game::new();
        steady.key_input(KeyCode::KeyW, true);
        jittery.key_input(KeyCode::KeyW, true);

        let total_ticks = 1000u64;
        let total_time = total_ticks as f64 * TICK_DT as f64;

        // 1000 frames of exactly one tick's worth of time each.
        for _ in 0..total_ticks {
            steady.advance(TICK_DT);
        }

        // The same total time, spread over wildly uneven frame deltas -- some
        // under a tick, some several ticks at once (forcing the catch-up loop),
        // in `f64` so the *test's own* bookkeeping isn't what introduces drift
        // (`Game::advance`'s internal accumulator is `f64` for the same reason).
        let mut elapsed = 0.0f64;
        let deltas: [f64; 5] = [0.1, 3.7, 0.02, 1.0, 0.5].map(|m| m * TICK_DT as f64);
        let mut i = 0;
        while elapsed < total_time {
            let remaining = total_time - elapsed;
            let dt = deltas[i % deltas.len()].min(remaining);
            jittery.advance(dt as f32);
            elapsed += dt;
            i += 1;
        }

        assert_eq!(steady.sim.tick, jittery.sim.tick);
        assert_eq!(steady.sim.player, jittery.sim.player);
    }
}
