//! Input, as a value.
//!
//! `InputFrame` exists so [`crate::Sim::tick`] never touches a keyboard or a
//! window: `cubara-app` translates whatever the platform handed it (winit
//! key codes, raw mouse deltas) into this plain, platform-free value once
//! per frame, and the same value drives every fixed step that frame's catch-up
//! loop runs. Recording input as a value rather than reading live device
//! state is what makes a future replay (block 1.8) possible at all — a
//! recorded sequence of `InputFrame`s reproduces a session exactly, and it's
//! also the shape netcode eventually wants (send the input, not the result).
use cubara_voxel::Angle;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputFrame {
    /// Movement input this frame, each axis in `-1.0..=1.0`: `[0]` strafe
    /// (+right), `[1]` vertical (+up, free-fly only), `[2]` forward (+look
    /// direction). Not a raw key snapshot -- opposing keys held together are
    /// already cancelled out (`right - left`, etc.) by whoever builds this.
    pub move_axes: [f32; 3],
    /// How far to turn this frame: `[0]` yaw (right is positive), `[1]` pitch
    /// (down is positive, matching screen coordinates).
    ///
    /// **An [`Angle`], not pixels.** It used to be raw mouse motion, scaled by
    /// a sensitivity constant inside [`crate::Player`]. `docs/RESEARCH_MULTIPLAYER.md`
    /// §3.5 requires that nothing crossing the wire is a float, and an
    /// `InputFrame` is the first thing that will — so the pixels-to-angle
    /// conversion moved to the client, using
    /// [`crate::SENSITIVITY_PER_PIXEL`].
    ///
    /// That is also where it belongs: sensitivity is a setting on the machine
    /// holding the mouse, not a fact about the world.
    pub look_delta: [Angle; 2],
    /// Jump, as a rising edge -- `true` only on the frame the key went down,
    /// not for as long as it's held. Walking mode consumes this once; it has
    /// no effect in free-fly. The caller (`cubara-app`) is responsible for
    /// only setting this on the actual edge and clearing it once consumed,
    /// so a multi-tick catch-up burst this frame doesn't apply it more than
    /// once (see [`crate::Sim::tick`]).
    pub jump: bool,
    /// Toggle free-fly debug mode, also a rising edge, same one-shot
    /// contract as [`Self::jump`].
    pub toggle_fly: bool,
    /// Whether the break button is **held** this frame.
    ///
    /// Held state like [`Self::move_axes`], deliberately *not* a rising edge
    /// like [`Self::jump`]: mining takes many ticks
    /// (`PHASE2_ARCHITECTURE.md` §4.3) and the whole point is that holding is
    /// what advances it. A catch-up burst applying this to every tick is
    /// correct here, where for `jump` it would be a bug.
    ///
    /// It lives in the input value rather than being read from the mouse at
    /// break time because a replay has to reproduce a mining session, and it
    /// can only do that if "was the button down on this tick" is part of the
    /// recorded input.
    pub breaking: bool,
}

impl InputFrame {
    /// This frame with its axes forced back inside the range the field's
    /// documentation has always claimed for them (block 2.14).
    ///
    /// `move_axes` is the only float that crosses the wire into the simulation,
    /// and it arrives from a machine that may be lying. Three things it can say
    /// that a keyboard cannot:
    ///
    /// - **NaN.** `f32::NAN != 0.0`, so it reaches `normalize()`, comes out NaN,
    ///   and casts to zero -- harmless today, entirely by luck, and one
    ///   refactor away from poisoning a position that is folded into the world
    ///   hash. A world whose hash depends on a client's NaN is a world that
    ///   cannot be replayed.
    /// - **Infinity.** Same path, same luck.
    /// - **A large magnitude.** Walking normalises, so this is capped today --
    ///   but by an implementation detail two crates away, not by anything that
    ///   says so. Clamping here makes the bound a property of the input rather
    ///   than a side effect of how movement happens to be computed.
    ///
    /// Non-finite values become zero rather than being clamped: there is no
    /// "nearest valid direction" to a NaN, and inventing one would be guessing
    /// what a client meant when the honest answer is that it said nothing.
    ///
    /// **Applied at the boundary, not inside [`crate::Sim::tick`].** The tick is
    /// the hot path and runs for every player every step; input arrives once per
    /// client per tick, at exactly one place, and validating where the untrusted
    /// thing enters is what makes the rest of the code able to assume it is
    /// clean. `look_delta` needs nothing: an `Angle` is an integer that wraps,
    /// so there is no value it cannot legitimately hold.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let axis = |v: f32| {
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        Self {
            move_axes: [
                axis(self.move_axes[0]),
                axis(self.move_axes[1]),
                axis(self.move_axes[2]),
            ],
            ..self
        }
    }
}
