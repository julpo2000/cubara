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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputFrame {
    /// Movement input this frame, each axis in `-1.0..=1.0`: `[0]` strafe
    /// (+right), `[1]` vertical (+up, free-fly only), `[2]` forward (+look
    /// direction). Not a raw key snapshot -- opposing keys held together are
    /// already cancelled out (`right - left`, etc.) by whoever builds this.
    pub move_axes: [f32; 3],
    /// Raw mouse motion this frame, in pixels: `[0]` right, `[1]` down.
    /// Deliberately unscaled -- look sensitivity is a gameplay tuning value,
    /// applied where the rest of movement happens ([`crate::Player`]), not
    /// baked into what counts as "the input".
    pub look_delta: [f32; 2],
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
