//! Golden-image tests: render a fixed scene headlessly and compare it against a
//! committed reference (`ARCHITECTURE.md` Rule 6 — behaviour is pinned by tests
//! before it is rewritten).
//!
//! This is the test that makes the rest of the architecture work usable. The point
//! of the boundaries is that a subsystem can be deleted and rebuilt; the thing that
//! makes that *safe* is a check that says whether the rebuild still draws the same
//! world. Without it, "does it still render correctly?" can only be answered by a
//! human looking at a window, which does not scale and does not run on the next
//! commit.
//!
//! **Regenerating the reference** is deliberate, never automatic:
//!
//! ```bash
//! CUBARA_BLESS=1 cargo test -p cubara-render --test golden
//! ```
//!
//! Only do that when the visual change is intended, and look at the new image
//! before committing it — blessing on autopilot turns this file into decoration.

use std::path::{Path, PathBuf};

use cubara_render::headless::{self, Shot};
use cubara_voxel::{BlockRegistry, Chunk, ChunkCoord};
use cubara_world::World;

/// Per-channel difference treated as equal. The same scene rasterises slightly
/// differently across backends and driver versions; an exact match would fire
/// constantly and the test would be deleted rather than trusted.
const TOLERANCE: u8 = 12;

/// Fraction of pixels allowed to exceed [`TOLERANCE`].
///
/// Calibrated from measurement, not taste. The reference is blessed on macOS/Metal
/// and checked everywhere, so the number that matters is the *cross-backend* delta:
///
/// | Signal | Differing pixels |
/// |---|---|
/// | Same machine, same scene, twice | **0.0000%** (exact, since #81) |
/// | macOS CI runner vs the reference | 0.0000% (max channel delta 1) |
/// | **Windows CI runner (DX12) vs the reference** | **0.0215%** (max delta 79) |
/// | A gash carved across the framed region | **4.07%** |
/// | This threshold | 0.2% |
///
/// ~9x above the worst measured backend difference, and ~20x below an obvious
/// regression. The Windows delta is silhouette-edge pixels rasterising differently
/// between DX12 and Metal — inherent, not a bug, and it will not go to zero.
///
/// Do not tighten this to hug the measured number. A golden test that fires on a
/// driver update gets muted, and a muted test is worse than no test.
const MAX_DIFFERING: f64 = 0.002;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn load_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()))
        .to_rgba8();
    (img.width(), img.height(), img.into_raw())
}

fn save_png(path: &Path, w: u32, h: u32, pixels: &[u8]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create golden dir");
    }
    image::save_buffer(path, pixels, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Render `shot` and compare against `tests/golden/<name>.png`.
fn assert_golden(name: &str, world: &World, shot: Shot) {
    let Some(frame) = headless::render(world, shot) else {
        // No adapter (a GPU-less CI runner). Skipping is honest here — the
        // alternative is a red build that says nothing about the code — but it is
        // reported loudly so a silently-never-running test is noticeable.
        eprintln!("SKIP {name}: no GPU adapter available on this machine");
        return;
    };

    let path = golden_dir().join(format!("{name}.png"));

    if std::env::var_os("CUBARA_BLESS").is_some() {
        save_png(&path, frame.width, frame.height, &frame.pixels);
        eprintln!("BLESSED {}", path.display());
        return;
    }

    assert!(
        path.exists(),
        "no golden image at {}. If this is a new test, create it with:\n    \
         CUBARA_BLESS=1 cargo test -p cubara-render --test golden",
        path.display()
    );

    let (gw, gh, expected) = load_png(&path);
    assert_eq!(
        (gw, gh),
        (frame.width, frame.height),
        "golden {name} is {gw}x{gh}, rendered {}x{}",
        frame.width,
        frame.height
    );

    let diff = headless::compare(&frame.pixels, &expected, TOLERANCE);
    // Always reported, not just on failure: the reference is generated on one
    // backend and checked on others, and this line is how the real cross-backend
    // delta becomes visible in CI logs instead of guessed at.
    eprintln!(
        "golden {name}: {:.4}% differ (tolerance {TOLERANCE}, max delta {}), threshold {:.4}%",
        diff.differing_fraction * 100.0,
        diff.max_channel_delta,
        MAX_DIFFERING * 100.0
    );
    if diff.differing_fraction > MAX_DIFFERING {
        // Write what was actually rendered next to the reference, so the failure is
        // diagnosable instead of just numeric.
        let actual = golden_dir().join(format!("{name}.actual.png"));
        save_png(&actual, frame.width, frame.height, &frame.pixels);
        panic!(
            "golden {name} differs: {:.2}% of pixels exceed tolerance {TOLERANCE} \
             (max channel delta {}), allowed {:.2}%.\n  expected: {}\n  actual:   {}\n\
             If the change is intended: CUBARA_BLESS=1 cargo test -p cubara-render --test golden",
            diff.differing_fraction * 100.0,
            diff.max_channel_delta,
            MAX_DIFFERING * 100.0,
            path.display(),
            actual.display(),
        );
    }
}

#[test]
fn the_same_scene_renders_byte_identically() {
    // The property that makes every other visual test trustworthy, and the one
    // multithreading will depend on: rendering must not vary run to run.
    //
    // This failed before issue #81 — 0.006% of pixels differed with a max channel
    // delta of 63, because `ChunkArena` iterated a `HashMap` when building the
    // indirect draw list, so chunks submitted in a different order each run and
    // depth ties on silhouette edges resolved differently. Meshing already runs on
    // a worker pool, so "whatever order results arrived in" was leaking into the
    // rendered frame.
    //
    // Scope, stated honestly: `from_region` meshes synchronously, so this pins
    // *draw-order* determinism, not worker-arrival determinism. Arrival order still
    // decides which slab offsets a chunk lands in (`ChunkArena::insert` is first-fit),
    // which no longer changes the image — the draw list is coord-sorted and the
    // geometry content is identical — but does mean the arena's internal layout is
    // not reproducible. That matters the moment world state is hashed or saved; see
    // issue #83.
    let shot = Shot::default();
    let Some(a) = headless::render(&World::new(), shot) else {
        eprintln!("SKIP the_same_scene_renders_byte_identically: no GPU adapter");
        return;
    };
    let b = headless::render(&World::new(), shot).expect("adapter was available a moment ago");

    // Tolerance 0: this is exactness, not similarity.
    let diff = headless::compare(&a.pixels, &b.pixels, 0);
    assert_eq!(
        diff.differing_fraction,
        0.0,
        "the same scene rendered twice differs on {:.6}% of pixels (max channel \
         delta {}) — draw order or meshing is leaking nondeterminism into the frame",
        diff.differing_fraction * 100.0,
        diff.max_channel_delta
    );
}

#[test]
fn terrain_renders_as_expected() {
    assert_golden("terrain", &World::new(), Shot::default());
}

#[test]
fn a_cave_mouth_is_visible() {
    // Block 1.5 (#48)'s own bar: caves are a real, cross-chunk 3D noise
    // field carved into the terrain (§8.3), not just a promise buried in
    // `density`'s implementation -- this proves one is actually reachable
    // and visible from outside, at ground level.
    //
    // The default orbit camera can't show this: it always looks down at a
    // fixed shallow angle from above (`Shot::camera`'s doc comment), which
    // can show a cave's roof but never see *into* an opening at ground
    // level, at any scene scale. This shot uses an explicit low, grazing
    // camera instead.
    //
    // Seed 119 and this exact eye/target are the result of scanning a small
    // region around the world origin for a cave chamber that breaches the
    // surface close enough to frame directly (see the PR description for
    // how) -- not hand-placed, and not the only one; caves are common
    // enough that most seeds have several within a couple of hundred blocks
    // of any point.
    let world = World::with_seed(119);
    let eye = glam::vec3(-6.0, 27.0, -2.0);
    let target = glam::vec3(-15.0, 29.0, -2.0);
    let shot = Shot {
        width: 960,
        height: 540,
        region_radius: 3,
        orbit_t: 0.0,
        camera: Some((eye, target - eye)),
        highlighted_block: None,
    };
    assert_golden("cave_mouth", &world, shot);
}

#[test]
fn no_crack_at_a_real_lod_boundary() {
    // Issue #108's own bar: a real level-0/level-1 node boundary, in the
    // live streamed scene (`ChunkArena::from_region`'s ring-schedule
    // streaming, not a synthetic fixture), with no crack of background
    // showing through -- proof the skirts added to `cubara_voxel`'s greedy
    // mesher actually reach the render path, not just the unit tests in
    // isolation.
    //
    // `DEFAULT_RING_SCHEDULE`'s first ring ends at chunk-radius 8 from the
    // region's center, so `region_radius: 12` (giving `schedule_for_radius`
    // `[(0, 8), (1, 12)]`) puts a real level-0-to-level-1 seam inside the
    // framed region -- the default auto-framed orbit camera (same one
    // `terrain_renders_as_expected` uses) frames the whole region from
    // above, which is the honest test: a player flying/walking over this
    // area at a normal viewing angle, not a worst-case grazing shot chosen
    // to manufacture a crack. (An earlier, near-ground grazing version of
    // this shot showed the skirts themselves as a visible fringe along
    // every nearby chunk edge, not just the one LOD seam -- correct
    // per-node behaviour, since a node can't tell a same-level neighbour
    // from a different-level one, but a confusing shot to assert "no
    // crack" against. This angle avoids that.)
    let world = World::new();
    let shot = Shot {
        region_radius: 12,
        ..Shot::default()
    };
    assert_golden("lod_boundary", &world, shot);
}

#[test]
fn the_selected_block_shows_an_outline() {
    // Issue #52's own bar: the highlight must align to the targeted block
    // with no z-fighting against its own (flush) top face -- the coplanar
    // case the depth bias exists for. A close, angled camera on the
    // default-seed world, aimed at a surface block with solid neighbours on
    // every other side: the box's other 11 edges are correctly hidden
    // behind that solid terrain (depth-tested like any other geometry),
    // which is why only the top face's outline shows here.
    let world = World::new();
    let ground = world
        .raycast([5.5, 200.0, 5.5], [0.0, -1.0, 0.0], 400.0)
        .expect("ground below");
    let eye = glam::vec3(2.5, ground.block[1] as f32 + 3.0, 2.5);
    let target = glam::vec3(5.5, ground.block[1] as f32 + 0.5, 5.5);
    let hit = world
        .raycast(eye.to_array(), (target - eye).to_array(), 30.0)
        .expect("the block this shot is framed around");

    let shot = Shot {
        width: 960,
        height: 540,
        region_radius: 3,
        orbit_t: 0.0,
        camera: Some((eye, target - eye)),
        highlighted_block: Some(hit.block),
    };
    assert_golden("outline", &world, shot);
}

#[test]
fn distinct_materials_render_with_distinct_textures() {
    // Block 1.4a's own bar: the packed vertex format carries a texture-array
    // layer per quad, resolved from the block registry -- this must actually
    // show up as visibly different colours on different materials, not just
    // "the flat green constant became a flat placeholder constant". `World`
    // can produce mixed materials since block 1.5, but only as noise-shaped
    // patches at whatever depth the terrain happens to expose them -- an
    // explicit, controlled block of each material is still the clearer test
    // of per-material appearance on its own, so this goes through
    // `render_chunks` with explicit chunks instead.
    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks");
    let registry = BlockRegistry::load(&assets_dir).expect("assets/blocks must be valid");
    let stone = registry
        .id_of("cubara:stone")
        .expect("cubara:stone in registry");
    let soil = registry
        .id_of("cubara:soil")
        .expect("cubara:soil in registry");
    let grass = registry
        .id_of("cubara:grass")
        .expect("cubara:grass in registry");

    // Three chunks in a row, each a solid block of one material.
    let chunks = vec![
        (ChunkCoord::new(-1, 0, 0), Chunk::from_fn(|_, _, _| stone)),
        (ChunkCoord::new(0, 0, 0), Chunk::from_fn(|_, _, _| soil)),
        (ChunkCoord::new(1, 0, 0), Chunk::from_fn(|_, _, _| grass)),
    ];

    let shot = Shot {
        width: 960,
        height: 540,
        region_radius: 0, // unused by render_chunks -- the chunk list is explicit
        orbit_t: 5.0,
        camera: None,
        highlighted_block: None,
    };

    let Some(frame) = headless::render_chunks(&chunks, shot) else {
        eprintln!("SKIP distinct_materials_render_with_distinct_textures: no GPU adapter");
        return;
    };

    let path = golden_dir().join("materials.png");

    if std::env::var_os("CUBARA_BLESS").is_some() {
        save_png(&path, frame.width, frame.height, &frame.pixels);
        eprintln!("BLESSED {}", path.display());
        return;
    }

    assert!(
        path.exists(),
        "no golden image at {}. If this is a new test, create it with:\n    \
         CUBARA_BLESS=1 cargo test -p cubara-render --test golden",
        path.display()
    );

    let (gw, gh, expected) = load_png(&path);
    assert_eq!((gw, gh), (frame.width, frame.height));

    let diff = headless::compare(&frame.pixels, &expected, TOLERANCE);
    eprintln!(
        "golden materials: {:.4}% differ (tolerance {TOLERANCE}, max delta {}), threshold {:.4}%",
        diff.differing_fraction * 100.0,
        diff.max_channel_delta,
        MAX_DIFFERING * 100.0
    );
    if diff.differing_fraction > MAX_DIFFERING {
        let actual = golden_dir().join("materials.actual.png");
        save_png(&actual, frame.width, frame.height, &frame.pixels);
        panic!(
            "golden materials differs: {:.2}% of pixels exceed tolerance {TOLERANCE} \
             (max channel delta {}), allowed {:.2}%.\n  expected: {}\n  actual:   {}",
            diff.differing_fraction * 100.0,
            diff.max_channel_delta,
            MAX_DIFFERING * 100.0,
            path.display(),
            actual.display(),
        );
    }
}

#[test]
fn edits_change_what_is_drawn() {
    // Proves the golden test has teeth: carve a trench through the surface and the
    // frame must differ from the untouched-terrain reference. A golden test that
    // cannot fail is worse than none, because it reads as coverage.
    let shot = Shot::default();
    let mut world = World::new();
    let Some(base) = headless::render(&World::new(), shot) else {
        eprintln!("SKIP edits_change_what_is_drawn: no GPU adapter");
        return;
    };

    // A wide gash straight across the framed region (radius 6 chunks = ~208
    // blocks across), deep enough to cut the surface everywhere it passes.
    for x in -100..100 {
        for z in -16..16 {
            for y in 0..40 {
                world.set_block(x, y, z, false);
            }
        }
    }
    let edited = headless::render(&world, shot).expect("adapter was available a moment ago");

    let diff = headless::compare(&edited.pixels, &base.pixels, TOLERANCE);
    eprintln!(
        "trench changed {:.3}% of pixels (threshold {:.3}%)",
        diff.differing_fraction * 100.0,
        MAX_DIFFERING * 100.0
    );
    assert!(
        diff.differing_fraction > MAX_DIFFERING,
        "carving a gash across the whole region changed only {:.3}% of pixels — the \
         comparison is too loose to catch a real regression",
        diff.differing_fraction * 100.0
    );
}
