//! Cubara — entry point.
//!
//! Owns the window and event loop; all GPU work lives in `cubara_render`. Forwards
//! keyboard + mouse input to [`Game`] (WASD to move, Space to jump, mouse to look,
//! F4 toggles the free-fly debug mode, 1-9 or the wheel pick a hotbar slot,
//! Esc opens the pause menu (or closes whatever screen is open -- inventory,
//! pause, console), a click takes the cursor
//! back, F11 or Alt+Enter toggles fullscreen). Walking under
//! gravity is the default; free-fly (Space/Shift up/down, no collision) is a
//! debug mode inside the same sim (`docs/PHASE1_ARCHITECTURE.md` §10).
//!
//! `/` opens the command console (`/tp`, `/give`, `/gamemode`, `/seed` --
//! `ROADMAP.md`'s phase 3 note); `C` in the pause menu switches survival/creative -- creative flies,
//! takes no damage, and has unlimited blocks (owner's call, 2026-09-18).

mod bench;
mod caps;
mod capture;
mod far_streaming;
mod flight;
mod game;
mod options;
mod screenshot;
mod settings;
mod streaming;

use std::sync::Arc;

use cubara_render::{grab_cursor, HotbarView, Hud, PanelView, Profiler, Renderer};

use crate::capture::CaptureEvent;
use crate::game::{
    load_item_registry, load_ore_registry, load_recipe_book, load_structure_registry, Game,
};
use crate::streaming::NodeStreaming;

use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};

/// Touchpad scroll distance, in pixels, that counts as one wheel notch.
const PIXELS_PER_NOTCH: f32 = 40.0;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// What a mouse button does while playing.
///
/// A named mapping rather than two `if`s in the event handler, so the one thing
/// somebody could silently swap back has a test. The owner asked for **right to
/// mine, left to place** -- the opposite of the genre's default -- and an
/// arrangement that unusual is exactly the kind a later refactor "corrects"
/// without noticing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hand {
    /// Held down for as long as the block is being dug (block 2.4b).
    Mine,
    /// A single press.
    Place,
}

/// What `button` does with the world. `None` for buttons that do nothing.
///
/// Only while the cursor is captured -- the inventory screen keeps the usual
/// meaning, where right-click is "take half". That is about a *slot* rather
/// than the world, and swapping it would make the screen disagree with every
/// other game's screen for no reason.
fn hand_for(button: MouseButton) -> Option<Hand> {
    match button {
        MouseButton::Right => Some(Hand::Mine),
        MouseButton::Left => Some(Hand::Place),
        _ => None,
    }
}

#[derive(Default)]
struct App {
    /// World + camera + what input does to them. The renderer draws it; it does
    /// not own it (`ARCHITECTURE.md` Rule 3).
    game: Game,
    renderer: Option<Renderer>,
    /// Which nodes are streamed in around the camera -- shares `renderer`'s
    /// lifecycle (both created together in `resumed`, since streaming exists
    /// to feed the renderer and needs its own `MeshAssets`).
    streaming: Option<NodeStreaming>,
    /// The far terrain beyond the voxels (`docs/PROPOSAL_FAR_VIEW.md`), same
    /// lifecycle as `streaming`.
    far: Option<far_streaming::FarStreaming>,
    /// This PC's settings (`settings.rs`), read when the window opens.
    settings: settings::Settings,
    /// The options screen is open, over the pause menu.
    options_open: bool,
    /// A benchmark running in a child process (`--bench tune`), choosing the
    /// far-terrain quality that holds `target_fps`.
    tuning: Option<std::process::Child>,
    /// The frame rate to hold: the monitor's refresh rate.
    target_fps: u32,
    /// Steps the quality down when frames miss the refresh rate while playing.
    frame_watch: Option<options::FrameWatch>,
    /// A server to join instead of hosting one, from `--connect <addr>`.
    ///
    /// Acted on in `resumed`, not here: joining needs the client's registries,
    /// and those exist only once the renderer has validated its textures. A
    /// failed join is fatal rather than a silent fall back to singleplayer --
    /// somebody who typed an address wants that world, and quietly giving them
    /// a different one is worse than saying no.
    connect_to: Option<String>,
    /// Whether the mouse is captured for first-person look (toggled with Escape).
    cursor_captured: bool,
    /// Whether a screen was open when capture last looked, so only a change
    /// moves the mouse (`capture::follow_screen`).
    screen_was_open: bool,
    /// Whether an Alt key is down, for Alt+Enter.
    alt_held: bool,
    /// Last known cursor position in window pixels. Only meaningful while the
    /// inventory screen is open -- a captured cursor does not move.
    cursor: (f32, f32),
    /// Kept alive for the program's lifetime when built with `--features profile`.
    _profiler: Option<Profiler>,
    /// When the last frame was drawn. The app loop owns the clock and hands `dt`
    /// to the game; the renderer keeps its own timing only for the FPS readout.
    last_frame: Option<std::time::Instant>,
}

/// Whether *any* screen is up -- the inventory, the pause menu, or the
/// command console. What `capture`'s free-the-mouse rule cares about is "is
/// something other than the game asking for keys and clicks", not which one
/// -- the same reason `capture::apply`/`follow_screen` take a plain `bool`
/// rather than an enum.
///
/// A free function taking `&Game`, not an `App` method: a method borrows all
/// of `self`, and every call site here runs while `self.renderer` is already
/// borrowed mutably (`let Some(renderer) = self.renderer.as_mut() ...` at the
/// top of `window_event`) -- borrowing only the one field this actually
/// reads is what keeps those disjoint.
fn any_screen_open(game: &Game) -> bool {
    game.inventory_open() || game.pause_open() || game.console().is_some()
}

/// What pressing Escape does, decided from `Game`'s screen state alone -- no
/// window, so it's a plain function of state and testable without one, the
/// same shape as `capture::apply`.
///
/// **Priority order matters and is the whole of this function's contract:**
/// the console can only be open once the pause menu already isn't
/// (`Game::open_console` closes it), so checking pause first is enough to
/// never mistake one screen for another when more than one flag happens to
/// be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeAction {
    /// A screen was open; close it. The inventory's close may be *refused*
    /// (a full grid cannot take the crafting cells back) -- the pause menu
    /// and console never refuse.
    CloseInventory,
    ClosePauseMenu,
    CancelConsole,
    /// Nothing was open: pause the game rather than just letting go of the
    /// mouse -- the ordinary FPS convention this project had no menu to
    /// point Escape at until now.
    OpenPauseMenu,
}

fn escape_action(game: &Game) -> EscapeAction {
    if game.pause_open() {
        EscapeAction::ClosePauseMenu
    } else if game.console().is_some() {
        EscapeAction::CancelConsole
    } else if game.inventory_open() {
        EscapeAction::CloseInventory
    } else {
        EscapeAction::OpenPauseMenu
    }
}

/// The pause menu or command console's text -- or, with neither open, the last
/// command's answer while it lasts -- or `None` -- see [`Hud::menu`].
///
/// Keyboard-driven rather than clickable: `ROADMAP.md`'s phase 3 note lists
/// four rows (options, new world, play mode, commands), and Options is still
/// empty. A hit-tested clickable layout is a lot of `cubara-render` machinery
/// for two keys -- `[C]` and `[N]` are the whole interaction, the same shape
/// as F3/F4/F5's single keys.
fn menu_text(game: &Game) -> Option<String> {
    if let Some(console) = game.console() {
        return Some(format!("{console}_\n[Enter] send   [Esc] cancel"));
    }
    if game.pause_open() {
        let mode = if game.is_creative() {
            "Creative"
        } else {
            "Survival"
        };
        let new_world = if !game.hosting() {
            "New World -- only whoever runs the world can"
        } else if game.new_world_armed() {
            "[N] again to confirm New World -- this world is kept as a backup"
        } else {
            "[N] New World"
        };
        return Some(format!(
            "-- PAUSED --\n\
             [Esc] resume\n\
             [C] play mode: {mode}\n\
             [O] Options\n\
             {new_world}\n\
             Commands: press / -- /tp x y z, /give item [count], /gamemode, /seed"
        ));
    }
    game.console_reply().map(str::to_string)
}

/// A seed for a world nobody has played yet.
///
/// Chosen here, in the window, because it is a *choice* rather than
/// simulation: once made it is world state like any other (Rule 1), saved in
/// the header and handed to every client. `RandomState` is std's own
/// randomly keyed hasher -- no new dependency for one number -- and hashing
/// the clock through it means two New Worlds in one run still differ.
fn fresh_seed() -> u64 {
    use std::hash::BuildHasher;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::hash::RandomState::new().hash_one(nanos)
}

/// `YYYY-MM-DD` (UTC) for `secs` since the Unix epoch -- the date in a kept
/// world's folder name, `saves/world-2026-09-26-1`.
///
/// The civil-from-days conversion (Howard Hinnant's), written out rather than
/// pulling in a date crate for one folder name.
fn date_label(secs: u64) -> String {
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

impl App {
    /// Let go of the mouse when a screen has opened, take it back when one has
    /// closed -- see [`capture::follow_screen`].
    fn follow_screen(&mut self) {
        let open = any_screen_open(&self.game);
        let want = capture::follow_screen(self.cursor_captured, self.screen_was_open, open);
        self.screen_was_open = open;
        if want != self.cursor_captured {
            self.cursor_captured = want;
            if let Some(renderer) = self.renderer.as_ref() {
                grab_cursor(renderer.window(), want);
            }
        }
    }

    /// Start a benchmark in a child process -- `cubara --bench tune` at the
    /// monitor's refresh rate and this window's size -- unless one is running.
    /// A child rather than this process so the game keeps running while it
    /// measures; its answer lands in the settings file and is picked up by
    /// [`poll_tuning`](Self::poll_tuning). The game drawing at the same time
    /// makes the answer, if anything, cautious.
    fn start_tuning(&mut self) {
        if self.tuning.is_some() {
            return;
        }
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let (w, h) = renderer.size();
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                log::warn!("cannot benchmark: no path to this program ({e})");
                return;
            }
        };
        let spawned = std::process::Command::new(exe)
            .args(["--bench", "tune", "--target", &self.target_fps.to_string()])
            .args(["--size", &format!("{w}x{h}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                log::info!(
                    "benchmarking this PC for {} FPS in the background",
                    self.target_fps
                );
                self.tuning = Some(child);
            }
            Err(e) => log::warn!("could not start the benchmark: {e}"),
        }
    }

    /// Take a finished benchmark's answer: read the settings it wrote and draw
    /// the far terrain at the quality it chose.
    fn poll_tuning(&mut self) {
        let Some(child) = self.tuning.as_mut() else {
            return;
        };
        let Ok(Some(status)) = child.try_wait() else {
            return;
        };
        self.tuning = None;
        if !status.success() {
            log::warn!("the benchmark did not finish ({status})");
            return;
        }
        if let Some(chosen) = settings::load(&settings::settings_path()) {
            log::info!("the benchmark chose far terrain {}", chosen.far.name());
            self.settings = chosen;
            self.set_far_quality(self.settings.far);
        }
    }

    /// Draw the far terrain at `quality` from now on, and give the frame watch
    /// time to settle before it judges the new quality.
    fn set_far_quality(&mut self, quality: far_streaming::FarQuality) {
        if let (Some(far), Some(renderer)) = (self.far.as_mut(), self.renderer.as_mut()) {
            far.set_quality(quality, renderer);
        }
        if let Some(w) = self.frame_watch.as_mut() {
            w.settle();
        }
    }

    /// Frames are missing the refresh rate: one quality down, remembered for
    /// the next start. Only for a quality the benchmark chose -- one the
    /// player picked is theirs -- and not while a benchmark is measuring.
    fn watch_frame(&mut self, dt: f32) {
        if self.settings.by_hand || self.tuning.is_some() {
            return;
        }
        let Some(watch) = self.frame_watch.as_mut() else {
            return;
        };
        if !watch.frame(dt) || self.settings.far == far_streaming::FarQuality::Off {
            return;
        }
        let lower = self.settings.far.lower();
        log::warn!(
            "frames are missing {} FPS: far terrain {} -> {}",
            self.target_fps,
            self.settings.far.name(),
            lower.name()
        );
        self.settings.far = lower;
        if let Err(e) = settings::save(&settings::settings_path(), &self.settings) {
            log::warn!("could not save the settings: {e}");
        }
        self.set_far_quality(lower);
    }

    /// One key on the options screen -- which eats every key while open, as
    /// the console does.
    fn options_key(&mut self, code: KeyCode, pressed: bool, event: &KeyEvent) {
        if !pressed {
            return;
        }
        if code == KeyCode::Escape {
            self.options_open = false;
            return;
        }
        let Some(key) = event
            .text
            .as_ref()
            .and_then(|t| t.chars().next())
            .and_then(options::options_key)
        else {
            return;
        };
        options::apply(&mut self.settings, key);
        if let Err(e) = settings::save(&settings::settings_path(), &self.settings) {
            log::warn!("could not save the settings: {e}");
        }
        match key {
            options::OptionsKey::Quality(q) => self.set_far_quality(q),
            options::OptionsKey::Auto | options::OptionsKey::Benchmark => self.start_tuning(),
        }
    }

    /// Route one key event to the open command console -- called instead of
    /// every other key handling while [`Game::console`] is `Some`, so a
    /// letter key never also strafes or opens another screen underneath.
    ///
    /// `code` (not `event.logical_key`) picks out Enter/Backspace/Escape,
    /// same as the rest of this file's key handling -- physical position,
    /// not the character it happens to produce this layout. Ordinary text
    /// comes from `event.text` instead, which is what actually accounts for
    /// layout and modifiers (Shift for `/`, an AZERTY `q` where QWERTY has
    /// `a`, ...); `code` alone cannot spell a command.
    fn console_key(&mut self, code: KeyCode, pressed: bool, event: &KeyEvent) {
        if !pressed {
            return;
        }
        match code {
            KeyCode::Escape => {
                self.game.console_cancel();
                self.follow_screen();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.game.console_submit();
                self.follow_screen();
            }
            KeyCode::Backspace => self.game.console_backspace(),
            _ => {
                if let Some(text) = event.text.as_ref() {
                    for c in text.chars() {
                        self.game.console_push(c);
                    }
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Cubara");
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let (renderer, mesh_assets) = Renderer::new(window.clone(), self.game.camera_pose());
        let layers = mesh_assets.layers;
        let registry = std::sync::Arc::new(mesh_assets.registry);
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        self.game.set_assets(registry.clone(), items, recipes);
        match self.connect_to.clone() {
            Some(addr) => {
                if let Err(e) = self.game.connect(&addr) {
                    eprintln!("could not join {addr}: {e}");
                    std::process::exit(1);
                }
            }
            // After `set_assets`, which stands the player on the ground --
            // loading replaces that with wherever they actually were (#179).
            // Only when hosting: a joined world is the server's to load.
            None => {
                self.game.load();
            }
        }
        let structures = load_structure_registry();
        let ores = load_ore_registry();
        self.streaming = Some(NodeStreaming::new(
            registry,
            &structures,
            &ores,
            move |name: &str| layers.layer_of(name),
        ));
        self.settings = settings::load(&settings::settings_path()).unwrap_or_default();
        self.far = Some(far_streaming::FarStreaming::new(
            self.game.world().seed(),
            self.settings.far,
        ));
        let mut renderer = renderer;
        renderer.set_icons(self.game.item_icons());
        self.renderer = Some(renderer);
        // Capture the mouse for first-person look (Esc releases it). A window
        // concern, so the app owns it rather than the renderer.
        grab_cursor(&window, true);
        self.cursor_captured = true;

        // The frame rate to hold is the monitor's (the owner, 2026-09-26: never
        // below your refresh rate while playing). A PC not yet benchmarked for
        // this monitor and GPU is benchmarked now, in the background.
        self.target_fps = options::target_fps(
            window
                .current_monitor()
                .and_then(|m| m.refresh_rate_millihertz()),
        );
        self.frame_watch = Some(options::FrameWatch::new(self.target_fps));
        let gpu = self
            .renderer
            .as_ref()
            .map(|r| r.adapter_name().to_string())
            .unwrap_or_default();
        if self.settings.needs_tuning(self.target_fps, &gpu) {
            self.start_tuning();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let Some(streaming) = self.streaming.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                // #179: the world was never written anywhere. `ROADMAP.md` says
                // phase 1 delivers a world that "is still there after you close
                // it", and until now closing it was exactly when it stopped
                // being there.
                self.game.save();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                renderer.resize(size.width, size.height);
                // A confined cursor is clipped to the rectangle the window had
                // when it was grabbed; after maximising or going fullscreen
                // that rectangle is the old one. Grab again for the new size.
                if self.cursor_captured {
                    grab_cursor(renderer.window(), true);
                }
            }
            WindowEvent::Focused(true) => {
                // Deliberately not a recapture -- see `capture::apply`. Logged
                // because "the mouse does nothing" is otherwise undiagnosable
                // from a report.
                log::info!("window focused (captured: {})", self.cursor_captured);
            }
            WindowEvent::Focused(false) => {
                log::info!("window lost focus: mouse released");
                let out = capture::apply(
                    self.cursor_captured,
                    self.game.inventory_open(),
                    CaptureEvent::FocusLost,
                );
                self.cursor_captured = out.captured;
                grab_cursor(renderer.window(), self.cursor_captured);
                // A mouse button released outside the window never arrives
                // (winit synthesises key releases, not button ones), so a dig
                // in progress would carry on while you are away.
                self.game.set_breaking(false);
                self.alt_held = false;
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.alt_held = mods.state().alt_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    if pressed
                        && !event.repeat
                        && capture::is_fullscreen_toggle(code, self.alt_held)
                    {
                        // Borderless rather than exclusive: it switches
                        // instantly, alt-tabs cleanly, and keeps the desktop's
                        // resolution. The resize that follows re-grabs.
                        let window = renderer.window();
                        let next = match window.fullscreen() {
                            Some(_) => None,
                            None => Some(Fullscreen::Borderless(None)),
                        };
                        log::info!("fullscreen: {}", next.is_some());
                        window.set_fullscreen(next);
                    } else if self.options_open {
                        self.options_key(code, pressed, &event);
                    } else if self.game.console().is_some() {
                        // The console eats every key while it's open -- typing
                        // "t" or "p" must not also strafe or open the panic
                        // menu underneath it. Not an `else if` chain entry
                        // alongside the rest: it has to come before anything
                        // that reads a letter or digit key as a game control.
                        self.console_key(code, pressed, &event);
                    } else if code == KeyCode::Escape && pressed {
                        // Capture is corrected by `follow_screen` below,
                        // the same path E already uses: opening any screen
                        // releases the mouse, closing (or staying open, on
                        // a refused close) takes it back or leaves it be.
                        match escape_action(&self.game) {
                            EscapeAction::CloseInventory => self.game.toggle_inventory(),
                            EscapeAction::ClosePauseMenu | EscapeAction::OpenPauseMenu => {
                                self.game.toggle_pause()
                            }
                            EscapeAction::CancelConsole => self.game.console_cancel(),
                        }
                        self.follow_screen();
                    } else if code == KeyCode::F3 && pressed {
                        renderer.toggle_debug();
                    } else if code == KeyCode::F5 && pressed {
                        // An explicit save as well as the one on exit: a crash
                        // or a lost window should not have to cost the session.
                        self.game.save();
                    } else if code == KeyCode::Slash && pressed && !any_screen_open(&self.game) {
                        self.game.open_console();
                        self.follow_screen();
                    } else if code == KeyCode::KeyO
                        && pressed
                        && self.game.pause_open()
                        && !self.game.inventory_open()
                    {
                        self.options_open = true;
                    } else if code == KeyCode::KeyC
                        && pressed
                        && self.game.pause_open()
                        && !self.game.inventory_open()
                    {
                        // The pause menu's one real control today: switch
                        // play mode. `ROADMAP.md`'s phase 3 note lists three
                        // more rows (options, new world, a fuller command
                        // set) that are display-only until they have
                        // somewhere to write to.
                        self.game.set_creative(!self.game.is_creative());
                    } else if code == KeyCode::KeyN
                        && pressed
                        && !event.repeat
                        && self.game.pause_open()
                    {
                        // Two presses -- `Game::press_new_world` -- and then
                        // the world goes, so whatever was streamed from it
                        // goes too.
                        if self.game.press_new_world() {
                            let today = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if self
                                .game
                                .new_world(fresh_seed(), &date_label(today))
                                .is_some()
                            {
                                streaming.reset(renderer);
                            }
                            self.follow_screen();
                        }
                    } else if code == KeyCode::KeyE && pressed {
                        // Toggling may be *refused* -- see
                        // `Game::toggle_inventory` -- so the mouse follows what
                        // the game decided, in `follow_screen` below, not what
                        // was asked for.
                        self.game.toggle_inventory();
                        self.follow_screen();
                    } else {
                        self.game.key_input(code, pressed);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Only while playing: with a screen open the wheel is not
                // aimed at the hotbar.
                if self.cursor_captured && !self.game.inventory_open() {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        // A touchpad reports pixels. About one notch's worth of
                        // finger travel per slot.
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / PIXELS_PER_NOTCH,
                    };
                    self.game.scroll_hotbar(lines);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Only meaningful while the screen is open; a captured
                // first-person cursor sits in the middle and never moves.
                self.cursor = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed {
                    let out = capture::apply(
                        self.cursor_captured,
                        any_screen_open(&self.game),
                        CaptureEvent::Click,
                    );
                    if out.consumed {
                        log::info!("click in the window: mouse captured again");
                        self.cursor_captured = out.captured;
                        grab_cursor(renderer.window(), self.cursor_captured);
                        return;
                    }
                }
                if self.game.inventory_open() && state == ElementState::Pressed {
                    let (w, h) = renderer.size();
                    self.game.click_panel(
                        self.cursor.0,
                        self.cursor.1,
                        button == MouseButton::Right,
                        w,
                        h,
                    );
                }
                // **Right mines, left places** -- the owner's preference, and
                // the opposite of the genre's default. Asked for directly, so
                // it is the binding rather than an option: a setting nobody has
                // asked to change is a menu to maintain and a second code path
                // to get wrong.
                //
                // Holding right mines the targeted block over several ticks
                // (block 2.4b); left click places one. Both only while the
                // cursor is captured (i.e. actually playing).
                //
                // The inventory screen keeps the usual meaning above -- there,
                // right-click is "take half", which is about a *slot* rather
                // than about the world, and swapping it would make the screen
                // disagree with every other game's screen for no reason.
                if self.cursor_captured && hand_for(button) == Some(Hand::Mine) {
                    // Held state, not an edge: mining advances for as long as
                    // the button is down, and `Game::advance` reads it each
                    // tick. The break itself happens on the tick the block
                    // gives way, not here.
                    self.game.set_breaking(state == ElementState::Pressed);
                }
                if self.cursor_captured
                    && state == ElementState::Pressed
                    && hand_for(button) == Some(Hand::Place)
                {
                    // Asked for, not done. What the placement changed arrives
                    // on the next frame's tick like every other effect, and
                    // `advance` below invalidates it there -- one frame later,
                    // which is exactly what a client on the other end of a
                    // socket gets.
                    self.game.place_block();
                }
            }
            WindowEvent::RedrawRequested => {
                let now = std::time::Instant::now();
                let dt = self
                    .last_frame
                    .map(|t| (now - t).as_secs_f32())
                    .unwrap_or(0.0);
                self.last_frame = Some(now);
                // The only place `Instant::now()` appears -- `Game::advance` turns
                // this wall-clock `dt` into fixed sim ticks without ever reading
                // the clock itself (`ARCHITECTURE.md` Rule 1, §9).
                for cc in self.game.advance(dt) {
                    streaming.invalidate(self.game.world(), cc);
                }
                // A bench or furnace opens as a message from the server, which
                // `advance` just applied -- so this is where the mouse learns.
                self.follow_screen();
                self.poll_tuning();
                self.watch_frame(dt);
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };
                let Some(streaming) = self.streaming.as_mut() else {
                    return;
                };
                let camera = self.game.camera_pose();
                streaming.update(renderer, self.game.world(), camera.eye.to_array());
                if let Some(far) = self.far.as_mut() {
                    // Whatever changed the world -- New World, a load, a join --
                    // changed its seed, and the far terrain follows the seed.
                    let seed = self.game.world().seed();
                    if far.seed() != seed {
                        far.reset(seed, renderer);
                        if let Some(w) = self.frame_watch.as_mut() {
                            w.settle();
                        }
                    }
                    far.update(renderer, camera.eye.to_array());
                }
                let slots = self.game.hotbar_slots();
                let hotbar = slots.as_ref().map(|s| HotbarView {
                    slots: s,
                    selected: self.game.selected_hotbar_slot(),
                });
                let (w, h) = renderer.size();
                let panel_data = self.game.panel_view(w, h);
                let hovered = self
                    .game
                    .hovered_item_name(self.cursor.0, self.cursor.1, w, h);
                let panel = panel_data.as_ref().map(|(p, contents, held)| PanelView {
                    panel: p,
                    contents,
                    held: *held,
                    cursor: self.cursor,
                    tooltip: hovered.as_deref(),
                    gauges: self.game.furnace_gauges(),
                });
                let others = self.game.other_players();
                let menu = if self.options_open {
                    Some(options::options_text(
                        &self.settings,
                        self.target_fps,
                        self.tuning.is_some(),
                    ))
                } else {
                    menu_text(&self.game)
                };
                renderer.render(
                    camera,
                    self.game.selected_block(),
                    self.game.cracking(),
                    &others,
                    Hud {
                        hotbar,
                        panel,
                        health: Some(self.game.health_view()),
                        // Only while looking through the camera: over a
                        // screen it would mark nothing.
                        crosshair: self.cursor_captured && !self.game.inventory_open(),
                        menu: menu.as_deref(),
                    },
                    // Fog at the far terrain's edge, not the voxels': until the
                    // haze (`PROPOSAL_FAR_VIEW.md` block F5), that is no fog in
                    // anything a player can see.
                    far_streaming::FAR_VIEW_RADIUS as f32,
                );
                // Immediately queue the next frame — we render continuously.
                renderer.window().request_redraw();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        // Raw mouse motion drives first-person look, but only while captured.
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            if self.cursor_captured {
                self.game.mouse_look(dx as f32, dy as f32);
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().collect();

    // The dedicated server: `cubara server [options]`.
    //
    // A subcommand rather than a separate download, because a machine that has
    // the game can host a world without fetching anything. It is *also* its own
    // binary (`cubara-server`), because this one links wgpu and winit even
    // here, where it will never open a window -- and a headless host should not
    // need a GPU stack installed to run a world.
    //
    // Both call the same `headless::run` and the same parser, so there is one
    // server rather than two that drift (Rule 5).
    if args.get(1).map(String::as_str) == Some("server") {
        match cubara_server::headless::parse_args(&args[2..]) {
            Ok(cfg) => cubara_server::headless::run(&cfg),
            Err(msg) => {
                eprintln!("{msg}");
                std::process::exit(2);
            }
        }
        return;
    }

    // GPU capability report: `cargo run --release -- --caps`.
    if args.iter().any(|a| a == "--caps") {
        caps::run();
        return;
    }

    // Headless benchmark mode:
    // `cargo run --release -- --bench [radius] [--size WIDTHxHEIGHT]`.
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        let target = bench::parse_target(args.get(i + 1).map(String::as_str));
        let size = match args.iter().position(|a| a == "--size") {
            // Refused rather than defaulted: a typo silently measuring 1080p
            // would be recorded as the size that was asked for.
            Some(j) => match args.get(j + 1).and_then(|s| bench::parse_size(s)) {
                Some(size) => size,
                None => {
                    eprintln!("--size needs WIDTHxHEIGHT, e.g. --size 2560x1440");
                    std::process::exit(2);
                }
            },
            None => bench::DEFAULT_SIZE,
        };
        let flag = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|j| args.get(j + 1))
        };
        let eye = flag("--eye").map(|text| {
            bench::parse_eye(text).unwrap_or_else(|| {
                eprintln!("--eye needs X,Y,Z in blocks, e.g. --eye 8,40,8");
                std::process::exit(2);
            })
        });
        let squash = match flag("--squash") {
            Some(text) => match text.parse::<i32>() {
                Ok(k) if k >= 1 => Some(k),
                _ => {
                    eprintln!("--squash needs a whole number of at least 1");
                    std::process::exit(2);
                }
            },
            // The old band, for comparing against rows measured before it went.
            None if args.iter().any(|a| a == "--band") => None,
            // What the game streams with.
            None => Some(streaming::VERTICAL_LOD_SQUASH),
        };
        let overlay = args.iter().any(|a| a == "--overlay");
        let gpu_timing = match flag("--gpu-timing") {
            Some(text) => bench::parse_gpu_timing_mode(text).unwrap_or_else(|| {
                eprintln!("--gpu-timing needs off, auto, or on");
                std::process::exit(2);
            }),
            None => bench::GpuTimingMode::default(),
        };
        // Default on (this is what every other row in BENCHMARKS.md now
        // measures); `off` is the same-commit, same-run A/B a package that
        // changes shading needs -- see bench.rs's `run` for why.
        let fog = match flag("--fog").map(String::as_str) {
            Some("off") => false,
            Some("on") | None => true,
            Some(_) => {
                eprintln!("--fog needs off or on");
                std::process::exit(2);
            }
        };
        // `--bench gate`: the owner's three eyes at the game's own view
        // distance, and one GATE line -- what `check-phase-gate.sh` runs.
        // Exits 0 either way, like every `--bench`: the GATE line is the
        // answer, and the script reads that rather than trusting a code.
        let radius = match target {
            bench::Target::Radius(radius) => radius,
            bench::Target::Gate => {
                let tries = bench::try_qualities(1000.0, size, overlay, gpu_timing, fog);
                let (_, line) = bench::gate_line(&tries, 1000.0);
                log::info!("{line}");
                return;
            }
            // `--bench tune [--target FPS]`: the best far-terrain quality that
            // holds this PC's frame-rate target -- by default a 60 Hz monitor's
            // -- written to its settings. What the game runs on its own, with
            // its monitor's refresh rate and its window's size.
            // `--bench flight [--eye X,Y,Z] [--look DX,DY,DZ] [--seconds S]
            // [--speed B] [--fps F] [--frames DIR]`: how much of the screen is
            // still loading while flying at the game's speed.
            bench::Target::Flight => {
                let number = |name: &str, default: f32| {
                    flag(name).map_or(default, |t| {
                        t.parse().unwrap_or_else(|_| {
                            eprintln!("{name} needs a number");
                            std::process::exit(2);
                        })
                    })
                };
                let look = flag("--look").map_or([1.0, -0.15, 0.3], |text| {
                    bench::parse_eye(text).unwrap_or_else(|| {
                        eprintln!("--look needs DX,DY,DZ, e.g. --look 1,-0.15,0.3");
                        std::process::exit(2);
                    })
                });
                let flight = flight::Flight {
                    start: eye.unwrap_or([8.0, 120.0, 8.0]),
                    look,
                    speed: number("--speed", flight::FLY_SPEED),
                    seconds: number("--seconds", 20.0),
                    fps: number("--fps", 60.0) as u32,
                    every: number("--every", 0.25),
                    size,
                    quality: far_streaming::FarQuality::High,
                };
                match flight::fly(&flight, flag("--frames").map(std::path::Path::new)) {
                    Some(report) => log::info!("{}", report.line()),
                    None => log::error!("FLIGHT: no GPU to fly on"),
                }
                return;
            }
            bench::Target::Tune => {
                let target_fps = flag("--target")
                    .and_then(|t| t.parse::<u32>().ok())
                    .unwrap_or(60);
                let tries = bench::try_qualities(target_fps as f64, size, overlay, gpu_timing, fog);
                let far = bench::best_quality(&tries, target_fps as f64)
                    .unwrap_or(far_streaming::FarQuality::Off);
                let chosen = settings::Settings {
                    far,
                    tuned: Some(settings::Tuned {
                        target_fps,
                        gpu: bench::adapter_name().unwrap_or_default(),
                    }),
                    by_hand: false,
                };
                match settings::save(&settings::settings_path(), &chosen) {
                    Ok(()) => log::info!(
                        "TUNE: {} holds {target_fps} FPS on {}",
                        far.name(),
                        chosen.tuned.as_ref().map_or("", |t| t.gpu.as_str())
                    ),
                    Err(e) => log::error!(
                        "TUNE: {} holds {target_fps} FPS, but the settings could not be saved: {e}",
                        far.name()
                    ),
                }
                return;
            }
        };
        bench::run(
            radius,
            size,
            bench::View {
                eye,
                squash,
                far: args
                    .iter()
                    .any(|a| a == "--far")
                    .then_some(far_streaming::FarQuality::High),
            },
            overlay,
            gpu_timing,
            fog,
        );
        return;
    }

    // Headless screenshot mode: `cargo run --release -- --screenshot [path]`.
    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let path = args
            .get(i + 1)
            .filter(|p| !p.starts_with("--"))
            .map(String::as_str)
            .unwrap_or("cubara.png");
        let value = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
        };
        let view = value("--eye")
            .and_then(bench::parse_eye)
            .map(|eye| screenshot::View {
                eye,
                look: value("--look")
                    .and_then(bench::parse_eye)
                    .unwrap_or([1.0, -0.2, 0.0]),
                radius: value("--radius").and_then(|r| r.parse().ok()).unwrap_or(32),
                size: value("--size")
                    .and_then(bench::parse_size)
                    .unwrap_or((1920, 1080)),
            });
        // Static overlay text -- the pause menu or console, for a review
        // screenshot of `Hud::menu` with no window (`--screenshot menu.png
        // --menu "line one\nline two"`, `\n` unescaped since a shell arg
        // can't carry a real newline).
        let menu = value("--menu").map(|m| m.replace("\\n", "\n"));
        screenshot::run(path, view, menu);
        return;
    }

    let event_loop = EventLoop::new().expect("create event loop");
    // Poll continuously rather than waiting for OS events — we want max FPS.
    event_loop.set_control_flow(ControlFlow::Poll);

    // Join somebody else's world: `cargo run --release -- --connect 192.168.0.5:25650`.
    let connect_to = args
        .iter()
        .position(|a| a == "--connect")
        .map(|i| match args.get(i + 1) {
            Some(addr) => addr.clone(),
            None => {
                eprintln!("--connect needs an address, e.g. --connect 192.168.0.5:25650");
                std::process::exit(2);
            }
        });

    let mut app = App {
        _profiler: Profiler::init(),
        connect_to,
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Right mines, left places.** The owner asked for it the other way round
    /// from the genre, so it is pinned: a later refactor that "corrects" it
    /// back has to do so deliberately.
    #[test]
    fn the_right_button_mines_and_the_left_places() {
        assert_eq!(hand_for(MouseButton::Right), Some(Hand::Mine));
        assert_eq!(hand_for(MouseButton::Left), Some(Hand::Place));
    }

    /// Nothing else reaches the world.
    ///
    /// A middle click that quietly placed a block would be a very confusing
    /// afternoon, and `_ =>` arms are where that kind of thing hides.
    #[test]
    fn other_buttons_do_nothing_to_the_world() {
        assert_eq!(hand_for(MouseButton::Middle), None);
        assert_eq!(hand_for(MouseButton::Back), None);
        assert_eq!(hand_for(MouseButton::Other(9)), None);
    }

    #[test]
    fn escape_opens_the_pause_menu_when_nothing_else_is_open() {
        let game = Game::new();
        assert_eq!(escape_action(&game), EscapeAction::OpenPauseMenu);
    }

    #[test]
    fn escape_closes_whichever_screen_is_actually_open() {
        let mut game = Game::new();
        game.toggle_pause();
        assert_eq!(escape_action(&game), EscapeAction::ClosePauseMenu);

        game.toggle_pause(); // back to nothing open
        game.open_console();
        assert_eq!(escape_action(&game), EscapeAction::CancelConsole);

        game.console_cancel();
        game.toggle_inventory(); // opening needs no assets; see `Game::toggle_inventory`
        assert_eq!(escape_action(&game), EscapeAction::CloseInventory);
    }

    #[test]
    fn menu_text_is_none_with_nothing_open() {
        let game = Game::new();
        assert_eq!(menu_text(&game), None);
    }

    #[test]
    fn menu_text_shows_the_current_play_mode() {
        let mut game = Game::new();
        game.toggle_pause();
        assert!(
            menu_text(&game).is_some_and(|t| t.contains("Survival")),
            "a fresh game starts in survival"
        );

        game.set_creative(true);
        assert!(
            menu_text(&game).is_some_and(|t| t.contains("Creative")),
            "the pause menu must reflect the switch, not just the game's own state"
        );
    }

    #[test]
    fn menu_text_offers_new_world_and_asks_before_doing_it() {
        let mut game = Game::new();
        game.toggle_pause();
        let first = menu_text(&game).unwrap();
        assert!(first.contains("[N] New World"), "{first}");
        game.press_new_world();
        let armed = menu_text(&game).unwrap();
        assert!(
            armed.contains("again to confirm") && armed.contains("kept"),
            "the second press is not asked for: {armed}"
        );
    }

    #[test]
    fn date_label_is_the_utc_calendar_date() {
        assert_eq!(date_label(0), "1970-01-01");
        assert_eq!(date_label(946_598_400), "1999-12-31");
        assert_eq!(date_label(1_709_208_000), "2024-02-29", "a leap day");
        assert_eq!(
            date_label(1_790_467_199),
            "2026-09-26",
            "the last second of a day"
        );
        assert_eq!(
            date_label(1_790_467_200),
            "2026-09-27",
            "and the first of the next"
        );
    }

    #[test]
    fn fresh_seeds_differ() {
        assert_ne!(fresh_seed(), fresh_seed());
    }

    #[test]
    fn menu_text_shows_the_console_over_the_pause_menu() {
        let mut game = Game::new();
        game.toggle_pause();
        game.open_console();
        let text = menu_text(&game).expect("the console is open");
        assert!(
            text.starts_with('/'),
            "the console's own text should be what's shown, not the pause menu underneath it"
        );
    }

    #[test]
    fn menu_text_shows_a_commands_answer_while_nothing_else_is_open() {
        let mut game = Game::new();
        game.open_console();
        for c in "seed".chars() {
            game.console_push(c);
        }
        game.console_submit();
        game.advance(cubara_sim::TICK_DT);
        let shown = menu_text(&game).expect("the answer is on screen");
        assert!(shown.starts_with("seed: "), "{shown:?}");

        game.open_console();
        assert_eq!(
            menu_text(&game).as_deref(),
            Some("/_\n[Enter] send   [Esc] cancel"),
            "while typing the next command, the console is what is shown"
        );
    }
}
