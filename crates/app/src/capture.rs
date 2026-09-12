//! Who the mouse belongs to, and which keys change the window rather than the
//! game.
//!
//! Pure rules, no window: the event handler in `main.rs` asks these and then
//! does what they say, so the part that decides can be tested without one.

use winit::keyboard::KeyCode;

/// Something that can change whether the game holds the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureEvent {
    /// Escape: let go, or take it back.
    Escape,
    /// The window lost focus -- alt-tab, or a click outside it.
    FocusLost,
    /// A mouse button went down inside the window.
    Click,
}

/// What the handler should do after a [`CaptureEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// Whether the game holds the mouse now.
    pub captured: bool,
    /// Whether the event was spent on capturing, and must not also reach the
    /// world. The click that brings you back into the game is not a click
    /// *in* the game -- otherwise returning to the window places a block.
    pub consumed: bool,
    /// Whether the open screen (inventory, bench, furnace) should close.
    pub close_screen: bool,
}

/// Apply `event` to a mouse that is `captured` or not, with an inventory-style
/// screen `screen_open` or not.
///
/// **Losing focus always lets go.** On Windows the operating system drops the
/// cursor clip when the window loses focus, so a game that still believes it
/// holds the mouse is wrong about it -- and nothing took it back afterwards,
/// which is how clicking out, maximising, and clicking back in left the mouse
/// dead.
///
/// **A click takes it back**, rather than regaining focus: focus also comes
/// back when somebody clicks the title bar to move or maximise the window, and
/// grabbing the cursor in the middle of that is hostile.
///
/// **Escape closes a screen before it does anything else.** With a bench open,
/// "let go of the mouse" is meaningless -- the mouse is already free -- and
/// what somebody pressing Escape there wants is out.
pub fn apply(captured: bool, screen_open: bool, event: CaptureEvent) -> CaptureOutcome {
    let keep = CaptureOutcome {
        captured,
        consumed: false,
        close_screen: false,
    };
    match event {
        CaptureEvent::Escape if screen_open => CaptureOutcome {
            consumed: true,
            close_screen: true,
            ..keep
        },
        CaptureEvent::Escape => CaptureOutcome {
            captured: !captured,
            consumed: true,
            ..keep
        },
        CaptureEvent::FocusLost => CaptureOutcome {
            captured: false,
            ..keep
        },
        // With a screen open the cursor is free on purpose, to click slots.
        CaptureEvent::Click if !captured && !screen_open => CaptureOutcome {
            captured: true,
            consumed: true,
            ..keep
        },
        CaptureEvent::Click => keep,
    }
}

/// Whether the game should hold the mouse after a screen `was_open` and now
/// `is_open`.
///
/// **Capture follows the screen, whatever opened it.** It used to be set only
/// by the E key, so a bench or furnace opened by clicking it -- which reaches
/// the client as a message, not a key -- left the mouse captured, and moving
/// it to a slot turned the camera instead.
///
/// Only the *change* acts: a screen opening lets go, a screen closing takes
/// the mouse back. Otherwise Escape and focus loss decide, and this must not
/// undo them every frame.
pub fn follow_screen(captured: bool, was_open: bool, is_open: bool) -> bool {
    match (was_open, is_open) {
        (false, true) => false,
        (true, false) => true,
        _ => captured,
    }
}

/// Whether this key press toggles fullscreen: F11, or Alt+Enter.
pub fn is_fullscreen_toggle(code: KeyCode, alt_held: bool) -> bool {
    match code {
        KeyCode::F11 => true,
        KeyCode::Enter | KeyCode::NumpadEnter => alt_held,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn losing_focus_lets_go_of_the_mouse() {
        let out = apply(true, false, CaptureEvent::FocusLost);
        assert!(!out.captured && !out.consumed && !out.close_screen);
    }

    #[test]
    fn escape_on_an_open_screen_closes_it_and_leaves_capture_alone() {
        let out = apply(false, true, CaptureEvent::Escape);
        assert!(out.close_screen, "escape did not close the screen");
        assert!(!out.captured, "capture is follow_screen's to restore");
        assert!(out.consumed);
        assert!(!apply(true, false, CaptureEvent::Escape).close_screen);
    }

    /// The bench bug: opened by a click, the mouse stayed captured.
    #[test]
    fn a_screen_opening_lets_go_and_closing_takes_the_mouse_back() {
        assert!(!follow_screen(true, false, true), "opening kept the mouse");
        assert!(
            follow_screen(false, true, false),
            "closing did not take it back"
        );
    }

    #[test]
    fn no_change_in_the_screen_leaves_capture_to_escape_and_focus() {
        assert!(!follow_screen(false, false, false), "undid an escape");
        assert!(follow_screen(true, false, false));
        assert!(!follow_screen(false, true, true));
    }

    /// The bug: click out, maximise, click back in, and the mouse did nothing.
    #[test]
    fn clicking_back_into_the_window_takes_the_mouse_back_without_acting() {
        let out = apply(true, false, CaptureEvent::FocusLost);
        let back = apply(out.captured, false, CaptureEvent::Click);
        assert!(back.captured, "the click did not recapture the mouse");
        assert!(
            back.consumed,
            "the recapturing click also reached the world"
        );
    }

    #[test]
    fn a_click_while_playing_is_a_click_in_the_game() {
        assert_eq!(
            apply(true, false, CaptureEvent::Click),
            CaptureOutcome {
                captured: true,
                consumed: false,
                close_screen: false,
            }
        );
    }

    #[test]
    fn a_click_on_an_open_screen_leaves_the_cursor_free() {
        assert_eq!(
            apply(false, true, CaptureEvent::Click),
            CaptureOutcome {
                captured: false,
                consumed: false,
                close_screen: false,
            }
        );
    }

    #[test]
    fn escape_toggles() {
        assert!(!apply(true, false, CaptureEvent::Escape).captured);
        assert!(apply(false, false, CaptureEvent::Escape).captured);
    }

    #[test]
    fn f11_and_alt_enter_toggle_fullscreen_and_plain_enter_does_not() {
        assert!(is_fullscreen_toggle(KeyCode::F11, false));
        assert!(is_fullscreen_toggle(KeyCode::Enter, true));
        assert!(is_fullscreen_toggle(KeyCode::NumpadEnter, true));
        assert!(!is_fullscreen_toggle(KeyCode::Enter, false));
        assert!(!is_fullscreen_toggle(KeyCode::KeyW, true));
    }
}
