//! Headless single-frame screenshot, for visual verification without a window.
//!
//! A thin wrapper over [`cubara_render::headless::render`] — the same code the
//! golden-image tests use, which in turn goes through the one scene-render path the
//! window uses (`ARCHITECTURE.md` Rule 5). This file deliberately contains no
//! rendering of its own; when it did, it drifted from the game and stopped proving
//! anything.
//!
//! Run with: `cargo run --release -- --screenshot out.png`

use cubara_render::materials::TextureLayers;
use cubara_render::{headless, load_registry, Shot};
use cubara_voxel::ChunkCoord;
use cubara_world::mesh::mesh_region;
use cubara_world::node::schedule_for_radius;
use cubara_world::World;

use crate::streaming::to_meshed_node;

pub fn run(path: &str) {
    let shot = Shot::default();
    let world = World::new();
    let registry = load_registry();
    let layers = TextureLayers::from_registry(&registry);
    let layer_of = |name: &str| layers.layer_of(name);
    let schedule = schedule_for_radius(shot.region_radius);
    let meshed = mesh_region(
        &world,
        &registry,
        &layer_of,
        ChunkCoord::new(0, 0, 0),
        0..=2,
        &schedule,
    )
    .into_iter()
    .filter_map(to_meshed_node);
    let Some(frame) = headless::render(meshed, shot) else {
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
