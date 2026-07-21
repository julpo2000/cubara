//! Headless single-frame screenshot, for visual verification without a window.
//!
//! A thin wrapper over [`cubara_render::headless::render`] — the same code the
//! golden-image tests use, which in turn goes through the one scene-render path the
//! window uses (`ARCHITECTURE.md` Rule 5). This file deliberately contains no
//! rendering of its own; when it did, it drifted from the game and stopped proving
//! anything.
//!
//! Run with: `cargo run --release -- --screenshot out.png`

use cubara_render::{headless, Shot};
use cubara_world::World;

pub fn run(path: &str) {
    let shot = Shot::default();
    let Some(frame) = headless::render(&World::new(), shot) else {
        log::error!("no suitable GPU adapter — cannot render a screenshot");
        return;
    };

    image::save_buffer(
        path,
        &frame.pixels,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    log::info!(
        "screenshot written to {path} ({}x{})",
        frame.width,
        frame.height
    );
}
