//! Cubara — entry point.
//!
//! Owns the window and event loop; all GPU work lives in `cubara_render`. Forwards
//! keyboard + mouse input to [`Game`] (WASD to move, Space to jump, mouse to look,
//! F4 toggles the free-fly debug mode, Esc releases the cursor). Walking under
//! gravity is the default; free-fly (Space/Shift up/down, no collision) is a
//! debug mode inside the same sim (`docs/PHASE1_ARCHITECTURE.md` §10).

mod bench;
mod caps;
mod game;
mod screenshot;
mod streaming;

use std::sync::Arc;

use cubara_render::{grab_cursor, HotbarView, PanelView, Profiler, Renderer};

use crate::game::{
    load_item_registry, load_ore_registry, load_recipe_book, load_structure_registry, Game,
};
use crate::streaming::NodeStreaming;

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
    /// Which nodes are streamed in around the camera -- shares `renderer`'s
    /// lifecycle (both created together in `resumed`, since streaming exists
    /// to feed the renderer and needs its own `MeshAssets`).
    streaming: Option<NodeStreaming>,
    /// Whether the mouse is captured for first-person look (toggled with Escape).
    cursor_captured: bool,
    /// Last known cursor position in window pixels. Only meaningful while the
    /// inventory screen is open -- a captured cursor does not move.
    cursor: (f32, f32),
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
        let (renderer, mesh_assets) = Renderer::new(window.clone(), self.game.camera_pose());
        let layers = mesh_assets.layers;
        let registry = std::sync::Arc::new(mesh_assets.registry);
        let items = load_item_registry();
        let recipes = load_recipe_book(&items);
        self.game.set_assets(registry.clone(), items, recipes);
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
                    } else if code == KeyCode::KeyE && pressed {
                        // The screen and the mouse are one thing: you cannot
                        // click slots while the cursor is locked to look around.
                        // Toggling may be *refused* -- see
                        // `Game::toggle_inventory` -- so capture follows what
                        // the game decided, not what was asked for.
                        self.game.toggle_inventory();
                        self.cursor_captured = !self.game.inventory_open();
                        grab_cursor(renderer.window(), self.cursor_captured);
                    } else {
                        self.game.key_input(code, pressed);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Only meaningful while the screen is open; a captured
                // first-person cursor sits in the middle and never moves.
                self.cursor = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state, button, .. } => {
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
                // Left click breaks the targeted block, right click places one — but
                // only while the cursor is captured (i.e. actually playing).
                if self.cursor_captured && state == ElementState::Pressed {
                    let edit = match button {
                        MouseButton::Left => self.game.break_block(),
                        MouseButton::Right => self.game.place_block(),
                        _ => None,
                    };
                    // The game decides what changed; streaming re-meshes it.
                    if let Some(cc) = edit {
                        streaming.invalidate(self.game.world(), cc);
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
                // The only place `Instant::now()` appears -- `Game::advance` turns
                // this wall-clock `dt` into fixed sim ticks without ever reading
                // the clock itself (`ARCHITECTURE.md` Rule 1, §9).
                self.game.advance(dt);
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
                renderer.render(camera, self.game.selected_block(), hotbar, panel);
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
