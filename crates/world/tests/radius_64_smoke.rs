//! CI smoke test for issue #89.
//!
//! Phase 1's perf gate (`--bench 64` at ≥1000 FPS) deliberately does not run in
//! CI — GitHub's runners have no representative GPU, and a perf number measured
//! on one would be noise, not signal (see `ROADMAP.md` phase 1's exit gate). What
//! *should* break CI is the thing a perf gate can't catch on a GPU-less runner:
//! world generation or meshing regressing into a hang or an unbounded blow-up as
//! streaming policy, worldgen or the mesher change. This test is CPU-only (no
//! wgpu device, no adapter) so it runs on every CI runner unconditionally.
//!
//! It reproduces exactly the streamed region `--bench 64` builds
//! (`ChunkArena::from_region`'s loop, minus the GPU upload) and checks two
//! bounds: it must settle within a generous wall-clock budget, and the resulting
//! geometry must stay under the render arena's fixed vertex/index capacities
//! (`crates/render/src/arena.rs`, duplicated here as plain numbers because
//! `cubara-world` must not depend on `cubara-render` — Rule 3).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cubara_voxel::ChunkCoord;
use cubara_world::{streaming, World};

const RADIUS: i32 = 64;

/// Local measurement (Apple M3, release build): a radius-64 load settles in
/// ~1.3-1.4 seconds (see `BENCHMARKS.md`'s block-1.2 row for why that moved
/// from ~250-270 ms). 120s leaves generous headroom for a slower or loaded CI
/// runner without masking a genuine hang.
const TIME_BUDGET: Duration = Duration::from_secs(120);

/// Mirrors `cubara_render::arena::{VERTEX_CAPACITY, INDEX_CAPACITY}`. If those
/// change, check whether this bound should move too.
const VERTEX_CAPACITY: u64 = 4_000_000;
const INDEX_CAPACITY: u64 = 6_000_000;

#[test]
fn radius_64_world_load_settles_within_budget() {
    let center = ChunkCoord::new(0, 0, 0);
    let coords = streaming::desired_chunks(center, RADIUS, 0..=2);
    let total = coords.len() as u64;

    // A plain atomic counter, polled from the test thread, so a timeout can
    // report how far the background thread actually got instead of just that it
    // didn't finish (the "report the number reached rather than hanging" design
    // decision in issue #89).
    let scanned = Arc::new(AtomicU64::new(0));
    let scanned_writer = scanned.clone();

    let handle = thread::spawn(move || {
        let world = World::new();
        let mut chunks = 0u64;
        let mut vertices = 0u64;
        let mut indices = 0u64;
        for coord in coords {
            if let Some(chunk) = world.chunk_at(coord) {
                let level = streaming::lod_for(coord, center);
                let mesh = chunk.build_mesh_lod(level);
                if !mesh.indices.is_empty() {
                    chunks += 1;
                    vertices += mesh.vertices.len() as u64;
                    indices += mesh.indices.len() as u64;
                }
            }
            scanned_writer.fetch_add(1, Ordering::Relaxed);
        }
        (chunks, vertices, indices)
    });

    let start = Instant::now();
    let (chunks, vertices, indices) = loop {
        if handle.is_finished() {
            break handle.join().expect("world-load thread panicked");
        }
        let elapsed = start.elapsed();
        if elapsed > TIME_BUDGET {
            let reached = scanned.load(Ordering::Relaxed);
            panic!(
                "radius-{RADIUS} world load did not settle within {TIME_BUDGET:?} -- reached \
                 {reached}/{total} coordinates scanned before the bound tripped. This is the \
                 bound issue #89 exists to enforce, not a spurious CI flake: worldgen or \
                 meshing has regressed into a hang or a severe slowdown."
            );
        }
        thread::sleep(Duration::from_millis(20));
    };

    eprintln!(
        "radius {RADIUS} settled in {:?}: {chunks}/{total} chunks meshed, {vertices} vertices, \
         {indices} indices",
        start.elapsed()
    );

    assert!(
        vertices < VERTEX_CAPACITY,
        "radius-{RADIUS} load needs {vertices} vertices, which meets or exceeds the render \
         arena's {VERTEX_CAPACITY}-vertex capacity -- geometry no longer fits and would be \
         silently dropped mid-frame (see ArenaUsage::exhausted in crates/render/src/arena.rs)"
    );
    assert!(
        indices < INDEX_CAPACITY,
        "radius-{RADIUS} load needs {indices} indices, which meets or exceeds the render \
         arena's {INDEX_CAPACITY}-index capacity -- geometry no longer fits and would be \
         silently dropped mid-frame (see ArenaUsage::exhausted in crates/render/src/arena.rs)"
    );
}
