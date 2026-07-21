//! Cubara — entry point.
//!
//! Owns the window and event loop; all GPU work lives in `cubara_render`. Forwards
//! keyboard + mouse input to the renderer's first-person camera (WASD to move,
//! Space/Shift up/down, mouse to look, Esc to release the cursor).

mod bench;
mod caps;
mod game;
mod screenshot;

use std::sync::Arc;

use cubara_render::{grab_cursor, Profiler, Renderer};

use crate::game::Game;

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct App {
    /// World + camera + what input does to them. The renderer draws it; it does
    /// not own it (`ARCHITECTURE.md` Rule 3).
    game: Game,
    renderer: Option<Renderer>,
    /// Whether the mouse is captured for first-person look (toggled with Escape).
    cursor_captured: bool,
    /// Kept alive for the program's lifetime when built with `--features profile`.
    _profiler: Option<Profiler>,
    /// When the last frame was drawn. The app loop owns the clock and hands `dt`
    /// to the game; the renderer keeps its own timing only for the FPS readout.
    last_frame: Option<std::time::Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Cubara");
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.renderer = Some(Renderer::new(
            window.clone(),
            self.game.world(),
            self.game.camera(),
        ));
        // Capture the mouse for first-person look (Esc releases it). A window
        // concern, so the app owns it rather than the renderer.
        grab_cursor(&window, true);
        self.cursor_captured = true;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    // Escape toggles mouse capture so you can leave the window.
                    if code == KeyCode::Escape && pressed {
                        self.cursor_captured = !self.cursor_captured;
                        grab_cursor(renderer.window(), self.cursor_captured);
                    } else if code == KeyCode::F3 && pressed {
                        renderer.toggle_debug();
                    } else {
                        self.game.key_input(code, pressed);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Left click breaks the targeted block, right click places one — but
                // only while the cursor is captured (i.e. actually playing).
                if self.cursor_captured && state == ElementState::Pressed {
                    let edit = match button {
                        MouseButton::Left => self.game.edit_block(false),
                        MouseButton::Right => self.game.edit_block(true),
                        _ => None,
                    };
                    // The game decides what changed; the renderer re-meshes it.
                    if let Some(cc) = edit {
                        renderer.invalidate(self.game.world(), cc);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = std::time::Instant::now();
                let dt = self
                    .last_frame
                    .map(|t| (now - t).as_secs_f32())
                    .unwrap_or(0.0);
                self.last_frame = Some(now);
                self.game.update(dt);
                renderer.render(self.game.world(), self.game.camera());
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

    let mut app = App {
        _profiler: Profiler::init(),
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}
