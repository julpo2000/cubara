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
use cubara_world::World;

/// Per-channel difference treated as equal. The same scene rasterises slightly
/// differently across backends and driver versions; an exact match would fire
/// constantly and the test would be deleted rather than trusted.
const TOLERANCE: u8 = 12;

/// Fraction of pixels allowed to exceed [`TOLERANCE`].
///
/// Calibrated against measurement, not taste (macOS/M3, Metal):
///
/// | Signal | Differing pixels |
/// |---|---|
/// | Same machine, same scene, rendered twice | **0.006%** (max channel delta 63) |
/// | A gash carved across the whole framed region | **4.08%** |
/// | This threshold | 0.5% |
///
/// That is ~80x above the noise floor and ~8x below an obvious regression. The
/// noise floor being non-zero at all is itself a finding: chunk draw order is not
/// stable between runs, so a handful of silhouette pixels resolve differently. See
/// issue #81 (chunk draw order is not deterministic) — once that is fixed this threshold
/// can drop a long way, and the test gets correspondingly sharper.
const MAX_DIFFERING: f64 = 0.005;

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
