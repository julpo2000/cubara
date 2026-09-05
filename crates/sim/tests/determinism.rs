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

use cubara_sim::{hash_region, InputFrame, Player, PlayerId, PlayerInputs, Sim, WorldHash};
use cubara_voxel::{Angle, BlockId, ChunkCoord, FixedVec3};
use cubara_world::{TerrainBlocks, World};

/// The single player these fixtures drive.
const P: PlayerId = PlayerId::LOCAL;

/// Arbitrary, fixed -- the only requirement is that it never changes once
/// a hash below is pinned against it.
const FIXTURE_SEED: u64 = 0x00C0_FFEE_D0D0;

/// No real registry involved (this crate doesn't load one) -- three
/// distinct synthetic ids, the same precedented pattern
/// `cubara_world::worldgen`'s own tests use, so `World::chunk_at` has
/// something to resolve edits/terrain layers to.
fn fixture_blocks() -> TerrainBlocks {
    TerrainBlocks {
        oak: None,
        ores: cubara_world::OreSet::EMPTY,
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
    let mut sim = Sim::new(
        seed,
        Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 40.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        ),
    );
    for input in script {
        sim.tick(&mut world, &PlayerInputs::one(P, *input), fixture_blocks());
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
    edits: &[(usize, [i32; 3], BlockId)],
) -> (Sim, World) {
    let mut world = World::with_seed(seed);
    let mut sim = Sim::new(
        seed,
        Player::new(
            cubara_voxel::FixedVec3::from_f32([0.5, 40.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        ),
    );
    for (i, input) in script.iter().enumerate() {
        sim.tick(&mut world, &PlayerInputs::one(P, *input), fixture_blocks());
        for &(tick, coord, block) in edits {
            if tick == i {
                world.set_block(coord[0], coord[1], coord[2], block);
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
        look_delta: [pixels(1.0), Angle::ZERO],
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
fn fixture_edits() -> Vec<(usize, [i32; 3], BlockId)> {
    vec![
        (50, [2, 15, 2], BlockId::AIR),
        // `fixture_blocks().stone`, not `BlockId::STONE` -- this fixture's
        // stone is id 3, and the constant is 1, which is its *grass*. Ids come
        // from sorted names, so a hardcoded one silently means a different
        // material (`World::chunk_at`'s doc comment).
        (300, [5, 20, 5], fixture_blocks().stone),
    ]
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
/// | `0xc763_3252_db46_3e78` | block 2.2b (#148), crafting grid + cursor added |
/// | `0x4f74_448e_2b83_20bb` | block 2.9a (#172), health + regen counter added |
/// | `0xe7dc_d72a_011b_4f9b` | fixed-point positions — the hash stopped folding in most `f32` bits |
/// | `0x7616_a4bd_6251_ca49` | fixed-point angles — **the last float left the digest** |
///
/// The last two are a different *kind* of change from the four before them,
/// which were all "new player state joined the digest". These changed the
/// representation of state that was already there.
///
/// The first did positions and velocities. This one does yaw and pitch, which
/// were the only floats still in it — and the ones that mattered most, because
/// they reach the world through `sin`/`cos`, among the least portable functions
/// in any standard library, and the resulting ray decides which block gets
/// broken (`docs/RESEARCH_MULTIPLAYER.md` §3.5).
///
/// **This digest now contains no floating-point value at all.** That is what
/// makes the toolchain pin a lint convenience again rather than load-bearing
/// for correctness: a hash built from integers cannot drift with a compiler
/// version.
///
/// **Moved again in block 2.10** (`0x7616_a4bd_6251_ca49` before it), and this
/// time not because a representation changed but because the *shape* did: the
/// world holds many players, so the digest folds a player count, the id counter
/// that hands out `PlayerId`s, and each player's id beside their state. The
/// fixture still drives exactly one player through exactly the same script, and
/// that player ends in exactly the same condition; what moved is the frame
/// around them.
const KNOWN_FIXTURE_HASH: u64 = 0x3935_6967_c3f3_6b29;

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

/// The mouse motion these scripts used to be written in.
///
/// `InputFrame::look_delta` is an `Angle` now (§3.5: nothing that crosses the
/// wire is a float), and the pixels-to-angle conversion moved to the client.
/// These scripts predate that and are written in pixels, so this is the same
/// conversion the client does -- which is what keeps them meaning what they
/// meant, rather than quietly turning 454 times as far.
fn pixels(px: f32) -> Angle {
    Angle::from_raw((px * cubara_sim::SENSITIVITY_PER_PIXEL as f32) as i32)
}

// ---------------------------------------------------------------------------
// Block 2.10: the world holds many players.
// ---------------------------------------------------------------------------

/// **Who holds which id is part of the world**, so swapping two players between
/// ids is a different world and the hash says so.
///
/// Stated as a test because the opposite is a tempting simplification: it would
/// be easy to fold players' state without their ids and call the result
/// order-independent. It would also be wrong -- two people who swapped bodies
/// are not the same world, and a server reconciling against that hash would
/// never notice they had.
///
/// The companion property -- that the *order the map was filled in* does not
/// matter -- is `crates/sim/tests/save_load.rs`'s
/// `a_saved_player_lists_order_does_not_change_the_world`, because loading is
/// the only path that can present ids out of order.
#[test]
fn who_holds_which_id_is_part_of_the_world() {
    let blocks = fixture_blocks();
    let region = fixture_region();

    let alice = || {
        Player::new(
            FixedVec3::from_f32([0.5, 40.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        )
    };
    let bob = || {
        Player::new(
            FixedVec3::from_f32([4.5, 42.0, -2.5]),
            Angle::from_radians(1.0),
            Angle::ZERO,
        )
    };

    // `Sim::new` seats its argument as PlayerId::LOCAL, so the two runs differ
    // in which of the two that is -- which is what "join order" means here.
    let run = |first: Player, second: Player| {
        let mut world = World::with_seed(FIXTURE_SEED);
        let mut sim = Sim::new(FIXTURE_SEED, first);
        sim.join(second);
        for _ in 0..30 {
            sim.tick(&mut world, &PlayerInputs::default(), fixture_blocks());
        }
        sim
    };

    let a = run(alice(), bob());
    let b = run(bob(), alice());

    assert_eq!(a.player_count(), 2, "both players are in the world");
    // Not equal, and that is the point: ids are positional on join, so the two
    // runs really did seat different people at PlayerId(0). The *hash* below is
    // what must not care about anything except who holds which id.
    assert_ne!(
        a.player(PlayerId::LOCAL).pos,
        b.player(PlayerId::LOCAL).pos,
        "the two runs seated the same player first, so this proves nothing"
    );

    // Same world, described from either end: fold each run's players by id and
    // the digests agree, because the map is ordered rather than insertion-kept.
    let world_a = World::with_seed(FIXTURE_SEED);
    let world_b = World::with_seed(FIXTURE_SEED);
    let ha = WorldHash::compute(&a, &world_a, &region, blocks, 1);
    let hb = WorldHash::compute(&b, &world_b, &region, blocks, 1);
    assert_ne!(
        ha, hb,
        "swapping who is player 0 is a different world, and the hash should say so"
    );
}

/// Ids are never reused. A player who leaves does not free their id for the
/// next joiner -- the same promise `EntityKey` makes, and for the same reason.
#[test]
fn a_left_players_id_never_comes_back() {
    let p = || {
        Player::new(
            FixedVec3::from_f32([0.5, 40.0, 0.5]),
            Angle::ZERO,
            Angle::ZERO,
        )
    };
    let mut sim = Sim::new(FIXTURE_SEED, p());

    let second = sim.join(p());
    assert_eq!(second, PlayerId(1));
    sim.leave(second).expect("they were here");
    assert_eq!(sim.player_count(), 1);

    let third = sim.join(p());
    assert_eq!(third, PlayerId(2), "an id was reused after a player left");
    assert!(sim.get(second).is_none(), "the departed player came back");
}

/// Every player steps on the same tick, not just the one an input names.
#[test]
fn a_player_who_sent_no_input_is_still_simulated() {
    let mut world = World::with_seed(FIXTURE_SEED);
    let high = FixedVec3::from_f32([0.5, 60.0, 0.5]);
    let mut sim = Sim::new(FIXTURE_SEED, Player::new(high, Angle::ZERO, Angle::ZERO));
    let quiet = sim.join(Player::new(
        FixedVec3::from_f32([2.5, 60.0, 2.5]),
        Angle::ZERO,
        Angle::ZERO,
    ));

    let before = sim.player(quiet).pos.y;
    for _ in 0..20 {
        // An input for the local player only. The other sent nothing.
        sim.tick(
            &mut world,
            &PlayerInputs::one(PlayerId::LOCAL, InputFrame::default()),
            fixture_blocks(),
        );
    }
    assert!(
        sim.player(quiet).pos.y < before,
        "gravity skipped the player who sent no input this tick"
    );
}
