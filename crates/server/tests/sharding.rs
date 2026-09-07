//! Phase 2's last gate criterion (`ROADMAP.md`), block 2.16.
//!
//! > **A region simulated elsewhere lands where it would have locally:** a
//! > region handed to a second `Server` and ticked there reaches the same world
//! > hash as one ticked in place. This is §7's whole claim — that work can move
//! > without changing the answer — and it is the same shape as block 2.6's
//! > dormancy test, which is not a coincidence: both are "the world does not
//! > care who ticked it".
//!
//! The claim rests on `docs/PHASE2_MULTIPLAYER.md` §7.2:
//!
//! > A shard's output is a **pure function of** `(seed, the region's state at
//! > tick N, the inputs applied between N and N+k)`.
//!
//! Which is only true because of Rule 1. A engine whose tick depended on
//! wall-clock time, hash iteration order or a global would fail this test and
//! could not be made to pass it without becoming deterministic first.

use cubara_server::shard::Shard;
use cubara_server::Server;
use cubara_sim::PlayerId;
use cubara_voxel::{ChunkCoord, FixedVec3};

/// Far from spawn, so nothing else is going on in these chunks.
const SITE: [i32; 3] = [600, 40, 600];

/// A world with a furnace smelting and an item on the floor, and the chunks
/// that hold them.
fn world_with_something_happening() -> (Server, Vec<ChunkCoord>, [i32; 3]) {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-sharding-fixture"));

    let furnace = SITE;
    s.add_furnace(furnace);

    // A block **edit**, not just terrain. Without one the shard's blocks are
    // whatever the seed generates, which a fresh receiver produces for itself --
    // so `install_shard` could skip writing blocks entirely and every assertion
    // would still pass. The first version of this fixture had exactly that hole.
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");
    s.set_block([furnace[0] + 2, furnace[1], furnace[2] + 2], stone);

    let (raw, log, stack) = {
        let items = s.items.as_ref().expect("assets loaded");
        let raw = items.id_of("cubara:raw_iron").expect("raw iron is an item");
        let log = items.id_of("cubara:oak_log").expect("oak log is an item");
        let stack = items.new_stack(log, 2).expect("a stack");
        (raw, log, stack)
    };
    let f = cubara_world::Furnace {
        input: Some((raw, 4)),
        fuel: Some((log, 4)),
        ..Default::default()
    };
    s.set_furnace(furnace, f);

    // Something on the floor too, so entities are part of what moves.
    s.sim.entities.spawn_item(
        stack,
        FixedVec3::from_blocks(furnace[0] + 1, furnace[1] + 4, furnace[2]),
        FixedVec3::ZERO,
    );

    let centre = ChunkCoord::from_block(furnace[0], furnace[1], furnace[2]);
    let mut chunks = Vec::new();
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                chunks.push(ChunkCoord::new(centre.x + dx, centre.y + dy, centre.z + dz));
            }
        }
    }
    chunks.sort();
    (s, chunks, furnace)
}

/// Tick `n` times, the way a dedicated server's loop does.
fn run(s: &mut Server, n: u64) {
    for _ in 0..n {
        s.tick_sim_all(&cubara_sim::PlayerInputs::default());
        s.tick_mining_all(&cubara_sim::PlayerInputs::default());
        s.tick_world();
    }
}

/// **The gate criterion.**
///
/// One server ticks the region itself. Another hands the identical region to a
/// second `Server`, lets *that* one tick it, and takes the result back. The two
/// end in the same state — every block, every furnace, every dropped item.
#[test]
fn a_region_simulated_elsewhere_lands_where_it_would_have_locally() {
    const TICKS: u64 = 400;

    // At home.
    let (mut home, chunks, furnace) = world_with_something_happening();
    home.assign(chunks.iter().copied());
    run(&mut home, TICKS);
    let locally = home.extract_shard(&chunks);

    // Away: the same starting state, handed to a second server that holds no
    // players at all — which is exactly the case that simulated nothing before
    // this block, because which chunks tick was decided by where somebody was
    // standing.
    let (owner, chunks2, _) = world_with_something_happening();
    assert_eq!(chunks, chunks2);
    let handed_over = owner.extract_shard(&chunks);

    let mut elsewhere = Server::new();
    elsewhere.open(std::path::Path::new("cubara-nonexistent-sharding-fixture"));
    // A host with nobody on it. `Server::new` always starts with a local client
    // -- singleplayer is what it is built for -- so a dedicated one has to say
    // so, and this is that. Without it the test would pass on the player's
    // simulation radius rather than on the assignment, which is the one thing
    // it is trying to prove.
    let local = elsewhere
        .local
        .take()
        .expect("a fresh server has a local client");
    elsewhere.sim.leave(local);
    assert_eq!(
        elsewhere.sim.player_count(),
        0,
        "the shard host must have no players, or it is not testing what it claims"
    );
    elsewhere.install_shard(&handed_over);
    elsewhere.assign(chunks.iter().copied());
    run(&mut elsewhere, TICKS);
    let remotely = elsewhere.extract_shard(&chunks);

    // The tick counters differ -- the two servers have their own clocks, which
    // is the point of a shard being a *region's* state rather than a world's --
    // so compare everything else.
    assert_eq!(
        locally.block_entities, remotely.block_entities,
        "the furnace reached a different state away from home"
    );
    assert_eq!(
        locally.entities, remotely.entities,
        "the dropped items reached a different state away from home"
    );
    assert_eq!(
        locally.blocks, remotely.blocks,
        "the blocks differ between a region ticked at home and one ticked away"
    );

    // And the shard really did something, or the equality above is the equality
    // of two untouched worlds.
    let (before, _, _) = world_with_something_happening();
    let untouched = before.extract_shard(&chunks);
    assert_ne!(
        locally.block_entities, untouched.block_entities,
        "nothing smelted in {TICKS} ticks, so this test compared two idle worlds"
    );

    // Handing it back leaves the owner where the local run ended.
    let mut back = owner;
    back.install_shard(&remotely);
    let after = back.extract_shard(&chunks);
    assert_eq!(
        after.block_entities, locally.block_entities,
        "the returned shard did not land in the server that gave it away"
    );
    let _ = furnace;
}

/// A server with no players and no assignment simulates nothing.
///
/// The other half of the same rule: chunks tick because somebody is there **or**
/// because this server was given them, and an idle server must not quietly keep
/// the world turning.
#[test]
fn an_unassigned_server_with_no_players_simulates_nothing() {
    let (mut s, chunks, _) = world_with_something_happening();
    let before = s.extract_shard(&chunks);
    run(&mut s, 400);
    let after = s.extract_shard(&chunks);
    assert_eq!(
        before.block_entities, after.block_entities,
        "a server that was given nothing and holds nobody still ticked a furnace"
    );
}

/// A player keeps their surroundings simulating without any assignment.
///
/// Block 2.16 replaced the player-radius call with a union of two sets, and this
/// is the half that already worked: if it stopped, singleplayer would quietly
/// stop smelting.
#[test]
fn a_player_still_keeps_their_own_chunks_simulating() {
    let (mut s, chunks, furnace) = world_with_something_happening();
    let before = s.extract_shard(&chunks);

    // The *local* player, moved: `centre_player` prefers the local client, so
    // joining a second one and standing it next to the furnace would centre the
    // radius on the first one, back at spawn, and prove nothing.
    let who: PlayerId = s.local.expect("a fresh server has a local client");
    s.sim.player_mut(who).pos = FixedVec3::from_blocks(furnace[0], furnace[1] + 2, furnace[2]);
    s.open_view(who);
    run(&mut s, 400);

    let after = s.extract_shard(&chunks);
    assert_ne!(
        before.block_entities, after.block_entities,
        "a furnace next to a standing player stopped smelting"
    );
}

/// A shard is a value: taking one twice from an unchanged world gives the same
/// answer.
///
/// Cheap, and it is what lets every assertion above be an equality rather than
/// a hash comparison.
#[test]
fn extracting_a_shard_twice_gives_the_same_value() {
    let (s, chunks, _) = world_with_something_happening();
    let a: Shard = s.extract_shard(&chunks);
    let b: Shard = s.extract_shard(&chunks);
    assert_eq!(a.blocks, b.blocks);
    assert_eq!(a.block_entities, b.block_entities);
    assert_eq!(a.entities, b.entities);
}

/// Chunks a server stops being responsible for stop ticking.
///
/// `keep_simulating` sleeps whatever is awake and no longer wanted, and that
/// half is as load-bearing as the waking: a shard handed away that kept ticking
/// at its old home would give two authorities for one region, which is the bug
/// sharding exists to prevent.
#[test]
fn a_region_handed_away_stops_ticking_at_its_old_home() {
    let (mut s, chunks, _) = world_with_something_happening();
    let local = s.local.take().expect("a fresh server has a local client");
    s.sim.leave(local);

    s.assign(chunks.iter().copied());
    run(&mut s, 100);
    let while_held = s.extract_shard(&chunks);
    let (fresh, _, _) = world_with_something_happening();
    assert_ne!(
        while_held.block_entities,
        fresh.extract_shard(&chunks).block_entities,
        "the furnace did not smelt while the region was held, so this proves nothing"
    );

    // Hand it away, and take responsibility for somewhere else instead.
    //
    // Somewhere else rather than *nothing*: with no player and no assignment
    // this server short-circuits before it reaches the active-set update at
    // all, so an assignment that never slept anything would go unnoticed. Still
    // holding something keeps that path live, which is also the realistic case
    // -- a shard host that gives one region away is usually holding others.
    let far = ChunkCoord::new(-900, 4, -900);
    s.assign([far]);
    run(&mut s, 400);

    assert_eq!(
        s.extract_shard(&chunks).block_entities,
        while_held.block_entities,
        "a region this server no longer holds kept smelting at its old home"
    );
}
