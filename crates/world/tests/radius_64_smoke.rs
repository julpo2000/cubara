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

use cubara_voxel::{BlockRegistry, ChunkCoord, Faces, Material, MeshContext, Shape};
use cubara_world::{streaming, TerrainBlocks, World};

const RADIUS: i32 = 64;

/// A registry with a single solid material -- matches `World::chunk_at`'s
/// current `BlockId::STONE` placeholder (block 1.4 is the real three-material
/// registry wired into the app; this smoke test only needs solid-vs-air, not
/// texturing).
fn test_registry() -> BlockRegistry {
    BlockRegistry::from_materials(vec![(
        std::path::PathBuf::from("test-fixture.ron"),
        Material {
            name: "cubara:stone".to_string(),
            solid: true,
            faces: Faces::All("stone".to_string()),
            shapes: vec![Shape::Full],
        },
    )])
    .expect("fixture registry is valid")
}

fn zero_layer(_: &str) -> u32 {
    0
}

/// Local measurement (Apple M3, debug build): a radius-64 load settles in
/// ~87s since block 1.5's seeded noise + caves replaced the old sin/cos
/// formula (see `BENCHMARKS.md` footnote 17 -- real per-voxel noise sampling
/// costs real CPU, unlike three cheap trig calls per column). 120s is no
/// longer "generous" at that baseline, so this scan runs on multiple threads
/// (below) -- matching how the live game actually streams chunks (a worker
/// pool, not one serial loop), which this budget should have been
/// measuring against all along.
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

    // A plain atomic counter, incremented by every worker and polled from the
    // test thread, so a timeout can report how far the workers actually got
    // instead of just that they didn't finish (the "report the number
    // reached rather than hanging" design decision in issue #89).
    let scanned = Arc::new(AtomicU64::new(0));

    // Split the region across a worker per core, each generating + meshing
    // its own slice against its own `World`/registry -- generation is a pure
    // function of `(seed, coord)` (§8.1), so this needs no coordination
    // between workers at all, the same property that lets the live game
    // stream chunks off a background pool instead of the main thread.
    let worker_count = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let slice_len = coords.len().div_ceil(worker_count);
    let handles: Vec<_> = coords
        .chunks(slice_len)
        .map(|slice| slice.to_vec())
        .map(|slice| {
            let scanned = scanned.clone();
            thread::spawn(move || {
                let world = World::new();
                let registry = test_registry();
                // Single-material fixture: point grass/soil/stone at the same
                // id so the depth rule doesn't introduce material-boundary
                // quads that aren't there for a real registry either at this
                // test's scope (settling time and arena capacity, not
                // material rendering).
                let stone = registry.id_of("cubara:stone").unwrap();
                let blocks = TerrainBlocks {
                    grass: stone,
                    soil: stone,
                    stone,
                };
                let ctx = MeshContext {
                    registry: &registry,
                    layer_of: &zero_layer,
                };
                let mut chunks = 0u64;
                let mut vertices = 0u64;
                let mut indices = 0u64;
                for coord in slice {
                    if let Some(chunk) = world.chunk_at(coord, blocks) {
                        let level = streaming::lod_for(coord, center);
                        let mesh = chunk.build_mesh_lod(&ctx, level);
                        if !mesh.indices.is_empty() {
                            chunks += 1;
                            vertices += mesh.vertices.len() as u64;
                            indices += mesh.indices.len() as u64;
                        }
                    }
                    scanned.fetch_add(1, Ordering::Relaxed);
                }
                (chunks, vertices, indices)
            })
        })
        .collect();

    let start = Instant::now();
    loop {
        if handles.iter().all(|h| h.is_finished()) {
            break;
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
    }
    let (chunks, vertices, indices) = handles
        .into_iter()
        .map(|h| h.join().expect("world-load thread panicked"))
        .fold((0u64, 0u64, 0u64), |(ac, av, ai), (c, v, i)| {
            (ac + c, av + v, ai + i)
        });

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
