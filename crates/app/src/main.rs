//! Cubara — entry point.
//!
//! Owns the window and event loop; all GPU work lives in `cubara_render`. Forwards
//! keyboard + mouse input to [`Game`] (WASD to move, Space to jump, mouse to look,
//! F4 toggles the free-fly debug mode, 1-9 or the wheel pick a hotbar slot,
//! Esc releases the cursor, a click takes it
//! back, F11 or Alt+Enter toggles fullscreen). Walking under
//! gravity is the default; free-fly (Space/Shift up/down, no collision) is a
//! debug mode inside the same sim (`docs/PHASE1_ARCHITECTURE.md` §10).

mod bench;
mod caps;
mod capture;
mod game;
mod screenshot;
mod streaming;

use std::sync::Arc;

use cubara_render::{grab_cursor, HotbarView, PanelView, Profiler, Renderer};

use crate::capture::CaptureEvent;
use crate::game::{
    load_item_registry, load_ore_registry, load_recipe_book, load_structure_registry, Game,
};
use crate::streaming::NodeStreaming;

use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
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

impl App {
    /// Let go of the mouse when a screen has opened, take it back when one has
    /// closed -- see [`capture::follow_screen`].
    fn follow_screen(&mut self) {
        let open = self.game.inventory_open();
        let want = capture::follow_screen(self.cursor_captured, self.screen_was_open, open);
        self.screen_was_open = open;
        if want != self.cursor_captured {
            self.cursor_captured = want;
            if let Some(renderer) = self.renderer.as_ref() {
                grab_cursor(renderer.window(), want);
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
        self.renderer = Some(renderer);
        // Capture the mouse for first-person look (Esc releases it). A window
        // concern, so the app owns it rather than the renderer.
        grab_cursor(&window, true);
        self.cursor_captured = true;
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
                    } else if code == KeyCode::Escape && pressed {
                        // Out of a screen if one is open; otherwise let go of
                        // (or take back) the mouse so you can leave the window.
                        let out = capture::apply(
                            self.cursor_captured,
                            self.game.inventory_open(),
                            CaptureEvent::Escape,
                        );
                        if out.close_screen {
                            // May be refused (a full inventory cannot take the
                            // grid back); capture then stays with the screen.
                            self.game.toggle_inventory();
                        } else {
                            log::info!("escape: mouse captured {}", out.captured);
                            self.cursor_captured = out.captured;
                            grab_cursor(renderer.window(), self.cursor_captured);
                        }
                    } else if code == KeyCode::F3 && pressed {
                        renderer.toggle_debug();
                    } else if code == KeyCode::F5 && pressed {
                        // An explicit save as well as the one on exit: a crash
                        // or a lost window should not have to cost the session.
                        self.game.save();
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
                        self.game.inventory_open(),
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
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };
                let Some(streaming) = self.streaming.as_mut() else {
                    return;
                };
                let camera = self.game.camera_pose();
                streaming.update(renderer, self.game.world(), camera.eye.to_array());
                let slots = self.game.hotbar_slots();
                let hotbar = slots.as_ref().map(|s| HotbarView {
                    slots: s,
                    selected: self.game.selected_hotbar_slot(),
                });
                let (w, h) = renderer.size();
                let panel_data = self.game.panel_view(w, h);
                let panel = panel_data.as_ref().map(|(p, contents, held)| PanelView {
                    panel: p,
                    contents,
                    held: *held,
                    cursor: self.cursor,
                });
                let others = self.game.other_players();
                renderer.render(
                    camera,
                    self.game.selected_block(),
                    &others,
                    hotbar,
                    panel,
                    Some(self.game.health_view()),
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

    // Headless benchmark mode: `cargo run --release -- --bench [radius]`.
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        let radius = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(12);
        bench::run(radius);
        return;
    }

    // Headless screenshot mode: `cargo run --release -- --screenshot [path]`.
    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("cubara.png");
        screenshot::run(path);
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
}
