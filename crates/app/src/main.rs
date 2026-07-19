//! Cubara — entry point.
//!
//! Owns the window and event loop; all GPU work lives in [`render`]. Milestone M1
//! grows this from a validation triangle into a chunk of cubes rendered at 1000+ FPS.

mod bench;
mod caps;
mod screenshot;

use std::sync::Arc;

use cubara_render::{Profiler, Renderer};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct App {
    renderer: Option<Renderer>,
    /// Kept alive for the program's lifetime when built with `--features profile`.
    _profiler: Option<Profiler>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Cubara");
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.renderer = Some(Renderer::new(window));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                renderer.render();
                // Immediately queue the next frame — we render continuously.
                renderer.window().request_redraw();
            }
            _ => {}
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
