//! The determinism harness (`ARCHITECTURE.md` Rule 1, issue #90).
//!
//! `ARCHITECTURE.md` names this test directly: run a fixed input script
//! twice -- once with the chunk-hash's worker pool forced to one thread,
//! once at several -- and compare a hash of the resulting world state. If
//! they diverge, the simulation isn't actually deterministic no matter what
//! it says elsewhere, and that is a CI failure, not a paragraph.
//!
//! An integration test (`tests/`, not `src/`) rather than a `#[cfg(test)]`
//! module: it drives the crate exactly the way a real replay consumer would
//! -- `Sim::new`, `Sim::tick`, `WorldHash::compute` -- through public API
//! only, which is a more honest rehearsal of block 1.9's save/load
//! round-trip (the next consumer of this same hash) than an inline test
//! with access to crate internals would be.

use cubara_sim::{hash_region, InputFrame, Player, Sim, WorldHash};
use cubara_voxel::{BlockId, ChunkCoord};
use cubara_world::{TerrainBlocks, World};

/// Arbitrary, fixed -- the only requirement is that it never changes once
/// a hash below is pinned against it.
const FIXTURE_SEED: u64 = 0x00C0_FFEE_D0D0;

/// No real registry involved (this crate doesn't load one) -- three
/// distinct synthetic ids, the same precedented pattern
/// `cubara_world::worldgen`'s own tests use, so `World::chunk_at` has
/// something to resolve edits/terrain layers to.
fn fixture_blocks() -> TerrainBlocks {
    TerrainBlocks {
        grass: BlockId(1),
        soil: BlockId(2),
        stone: BlockId(3),
    }
}

/// Chunks the fixture's movement and edits stay well within -- generous
/// padding, not tuned to the exact path, since the hash only needs to cover
/// *somewhere* the script provably touches, not the whole world.
fn fixture_region() -> Vec<ChunkCoord> {
    (-3..=3)
        .flat_map(|x| (0..=2).flat_map(move |y| (-3..=3).map(move |z| ChunkCoord::new(x, y, z))))
        .collect()
}

/// The minimal replay driver issue #90's Scope calls for: a seed and a
/// script, ticked in order, nothing else. Movement-only -- see
/// `replay_with_edits` for the fixture's own scripted block edits, which
/// can't go through `InputFrame`/`Sim::tick` (neither has any notion of
/// placing or breaking a block; that stays a `cubara-app`-level action,
/// `Game::edit_block`, deliberately out of this issue's scope).
fn replay(seed: u64, script: &[InputFrame]) -> (Sim, World) {
    let mut world = World::with_seed(seed);
    let mut sim = Sim::new(seed, Player::new(glam::vec3(0.5, 40.0, 0.5), 0.0, 0.0));
    for input in script {
        sim.tick(&mut world, input);
    }
    (sim, world)
}

/// Like [`replay`], but also applies `edits` (tick index, block coord,
/// solid) between ticks -- a fixed, scripted mutation, still "data, not
/// generated randomly per run" (the issue's own phrase for the movement
/// script), just not expressible as an `InputFrame`.
fn replay_with_edits(
    seed: u64,
    script: &[InputFrame],
    edits: &[(usize, [i32; 3], bool)],
) -> (Sim, World) {
    let mut world = World::with_seed(seed);
    let mut sim = Sim::new(seed, Player::new(glam::vec3(0.5, 40.0, 0.5), 0.0, 0.0));
    for (i, input) in script.iter().enumerate() {
        sim.tick(&mut world, input);
        for &(tick, coord, solid) in edits {
            if tick == i {
                world.set_block(coord[0], coord[1], coord[2], solid);
            }
        }
    }
    (sim, world)
}

/// Movement, gravity, and a jump -- committed, fixed data (built by this
/// function, not randomized), per the issue's design decision. Settle onto
/// the ground first (exercises gravity from a clean start), then walk
/// forward while slowly turning (covers varied terrain, same technique as
/// `cubara_sim`'s own `walking_uneven_terrain_for_10_000_ticks...` test),
/// jump once, and keep walking through the landing.
fn fixture_script() -> Vec<InputFrame> {
    let mut script = Vec::new();

    for _ in 0..90 {
        script.push(InputFrame::default());
    }

    let walking = InputFrame {
        move_axes: [0.0, 0.0, 1.0],
        look_delta: [1.0, 0.0],
        ..InputFrame::default()
    };
    for _ in 0..180 {
        script.push(walking);
    }

    script.push(InputFrame {
        jump: true,
        move_axes: [0.0, 0.0, 1.0],
        ..InputFrame::default()
    });

    let forward = InputFrame {
        move_axes: [0.0, 0.0, 1.0],
        ..InputFrame::default()
    };
    for _ in 0..60 {
        script.push(forward);
    }

    script
}

/// Break one block during the settle phase, place a different one near the
/// end -- fixed world coordinates, not tied to wherever the player actually
/// ends up (this is a hash regression test, not a physics test; the edits
/// only need to be deterministic and inside [`fixture_region`]).
fn fixture_edits() -> Vec<(usize, [i32; 3], bool)> {
    vec![(50, [2, 15, 2], false), (300, [5, 20, 5], true)]
}

/// This implementation's own computed value for [`FIXTURE_SEED`] +
/// [`fixture_script`] + [`fixture_edits`] + [`fixture_region`], run once
/// and pinned here -- not cross-checked against another implementation
/// (there isn't one to check against; same situation as `WorldRng`'s and
/// `cubara_world::worldgen`'s own known-sequence/known-hash tests). What
/// matters is that it stays exactly this value on every future run, on
/// every platform.
///
/// **Changed once, deliberately, in block 2.1b (#136):** the player's
/// inventory joined the world-state hash, so the digest over the same fixture
/// legitimately moved. That is the *only* reason this constant may be edited --
/// a change to what state is hashed, stated in the PR that makes it. If it
/// moves without such a change, the sim has become non-deterministic and this
/// test is doing its job; re-pinning it then is the same mistake as blessing a
/// golden image to make a rendering test pass.
///
/// Previous values, so a bisect can tell "hash definition changed" apart from
/// "sim diverged":
///
/// | Value | Why it changed |
/// |---|---|
/// | `0x84d6_5897_33af_263a` | block 1.8 (#90), the original pin |
/// | `0xede5_39f2_ee54_4d2a` | block 2.1b (#136), inventory added to the hash |
const KNOWN_FIXTURE_HASH: u64 = 0xede5_39f2_ee54_4d2a;

#[test]
fn replay_of_the_same_seed_and_script_is_deterministic() {
    let script = fixture_script();
    let (sim_a, world_a) = replay(FIXTURE_SEED, &script);
    let (sim_b, world_b) = replay(FIXTURE_SEED, &script);
    let region = fixture_region();
    let blocks = fixture_blocks();

    assert_eq!(
        WorldHash::compute(&sim_a, &world_a, &region, blocks, 1),
        WorldHash::compute(&sim_b, &world_b, &region, blocks, 1),
        "the same seed and input script must reach the same state every time"
    );
}

#[test]
fn the_fixture_reaches_a_known_hash_regardless_of_worker_count() {
    let (sim, world) = replay_with_edits(FIXTURE_SEED, &fixture_script(), &fixture_edits());
    let region = fixture_region();
    let blocks = fixture_blocks();

    let single_threaded = WorldHash::compute(&sim, &world, &region, blocks, 1);
    // Not `std::thread::available_parallelism()` -- a fixed worker count
    // keeps this test meaningful even on a CI runner that happens to
    // report one core, where "default width" would otherwise silently
    // degrade to the same code path as "forced to one thread" and prove
    // nothing about merge order.
    let multi_threaded = WorldHash::compute(&sim, &world, &region, blocks, 6);
    assert_eq!(
        single_threaded, multi_threaded,
        "the world-state hash must not depend on how many threads computed it"
    );

    assert_eq!(
        single_threaded.value(),
        KNOWN_FIXTURE_HASH,
        "the fixture's hash changed -- if this fires on only one of macOS/Windows CI, \
         the sim/world pipeline has diverged cross-platform, which ARCHITECTURE.md Rule 1 \
         calls a CI failure, not a paragraph"
    );
}

#[test]
fn chunk_hash_alone_is_independent_of_worker_count_at_fixture_scale() {
    // The same property as the test above, isolated to hash_region (no
    // Sim/tick/edits involved) and swept across several worker counts, not
    // just one vs six.
    let (_, world) = replay_with_edits(FIXTURE_SEED, &fixture_script(), &fixture_edits());
    let region = fixture_region();
    let blocks = fixture_blocks();

    let baseline = hash_region(&world, &region, blocks, 1);
    for threads in [2, 3, 4, 8, 16] {
        assert_eq!(
            hash_region(&world, &region, blocks, threads),
            baseline,
            "hash_region diverged at thread_count = {threads}"
        );
    }
}
