//! Block 2.17 — a shard held somewhere else, checked, and recovered.
//!
//! `docs/PHASE2_MULTIPLAYER.md` §7. Two of §7.4's three hard parts:
//! **reclaim on failure** and **an audit that makes nondeterminism visible**.
//! Boundary handoff is deliberately not here — see the module docs.

use cubara_server::coordinator::{Audit, ClaimError, Coordinator, PeerId};
use cubara_server::Server;
use cubara_sim::PlayerInputs;
use cubara_voxel::{ChunkCoord, FixedVec3};

const SITE: [i32; 3] = [600, 40, 600];

fn fixture() -> (Server, Vec<ChunkCoord>) {
    let mut s = Server::new();
    s.open(std::path::Path::new(
        "cubara-nonexistent-coordinator-fixture",
    ));
    let local = s.local.take().expect("a fresh server has a local client");
    s.sim.leave(local);

    s.add_furnace(SITE);
    let (raw, log, stack) = {
        let items = s.items.as_ref().expect("assets loaded");
        let raw = items.id_of("cubara:raw_iron").expect("raw iron");
        let log = items.id_of("cubara:oak_log").expect("oak log");
        let stack = items.new_stack(log, 2).expect("a stack");
        (raw, log, stack)
    };
    s.set_furnace(
        SITE,
        cubara_world::Furnace {
            input: Some((raw, 4)),
            fuel: Some((log, 4)),
            ..Default::default()
        },
    );
    s.sim.entities.spawn_item(
        stack,
        FixedVec3::from_blocks(SITE[0] + 1, SITE[1] + 4, SITE[2]),
        FixedVec3::ZERO,
    );

    let c = ChunkCoord::from_block(SITE[0], SITE[1], SITE[2]);
    let mut chunks = Vec::new();
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                chunks.push(ChunkCoord::new(c.x + dx, c.y + dy, c.z + dz));
            }
        }
    }
    chunks.sort();
    (s, chunks)
}

fn run(s: &mut Server, n: u64) {
    for _ in 0..n {
        s.tick_sim_all(&PlayerInputs::default());
        s.tick_mining_all(&PlayerInputs::default());
        s.tick_world();
    }
}

/// An honest peer's work survives being checked.
///
/// First, because an audit that has never said `Agrees` is an audit that might
/// say `Disagrees` about everything.
#[test]
fn an_honest_peer_passes_its_audit() {
    const TICKS: u64 = 300;
    let (owner, chunks) = fixture();
    let peer = PeerId(1);

    let mut coord = Coordinator::new();
    coord.claim(peer, &chunks).expect("a free region");
    coord.checkpoint(peer, owner.extract_shard(&chunks));

    // The peer does the work.
    let (mut worker, _) = fixture();
    worker.install_shard(coord.checkpoint_of(peer).expect("a checkpoint"));
    worker.assign(chunks.iter().copied());
    run(&mut worker, TICKS);
    let reported = worker.extract_shard(&chunks);

    let (mut auditor, _) = fixture();
    assert_eq!(
        coord.audit(
            peer,
            &reported,
            &mut auditor,
            TICKS,
            &PlayerInputs::default()
        ),
        Audit::Agrees,
        "a peer that did the work honestly failed its audit"
    );

    // And the work was real, or the audit compared two idle worlds.
    let (before, _) = fixture();
    assert_ne!(
        reported.block_entities,
        before.extract_shard(&chunks).block_entities,
        "nothing happened in {TICKS} ticks, so the audit proved nothing"
    );
}

/// A peer that lies is caught.
///
/// §7.2's whole claim. The lie here is the cheapest one available — an extra
/// item that was never smelted — and it is exactly the kind a peer authoritative
/// over its own region could tell.
#[test]
fn a_peer_that_conjures_an_item_is_caught() {
    const TICKS: u64 = 300;
    let (owner, chunks) = fixture();
    let peer = PeerId(1);

    let mut coord = Coordinator::new();
    coord.claim(peer, &chunks).expect("a free region");
    coord.checkpoint(peer, owner.extract_shard(&chunks));

    let (mut worker, _) = fixture();
    worker.install_shard(coord.checkpoint_of(peer).expect("a checkpoint"));
    worker.assign(chunks.iter().copied());
    run(&mut worker, TICKS);

    // The lie: a furnace that produced more than the ticks paid for.
    let mut cheated = worker.extract_shard(&chunks);
    let (_, f) = cheated
        .block_entities
        .first_mut()
        .expect("the furnace is in the shard");
    f.output = Some((
        f.output.map(|(id, _)| id).unwrap_or_else(|| {
            worker
                .items
                .as_ref()
                .and_then(|i| i.id_of("cubara:iron_ingot"))
                .expect("iron ingot")
        }),
        64,
    ));

    let (mut auditor, _) = fixture();
    assert_eq!(
        coord.audit(
            peer,
            &cheated,
            &mut auditor,
            TICKS,
            &PlayerInputs::default()
        ),
        Audit::Disagrees,
        "a peer invented 64 ingots and the audit believed it"
    );
}

/// Without a checkpoint there is nothing to replay from, and the audit says so
/// rather than passing.
///
/// `Unknown` is a third answer on purpose: "I could not check" and "I checked
/// and it was fine" are different, and collapsing them is how an audit quietly
/// stops auditing.
#[test]
fn an_audit_without_a_checkpoint_is_unknown_not_agreement() {
    let (owner, chunks) = fixture();
    let peer = PeerId(1);
    let mut coord = Coordinator::new();
    coord.claim(peer, &chunks).expect("a free region");

    let (mut auditor, _) = fixture();
    assert_eq!(
        coord.audit(
            peer,
            &owner.extract_shard(&chunks),
            &mut auditor,
            10,
            &PlayerInputs::default()
        ),
        Audit::Unknown,
        "an unauditable shard was reported as agreeing"
    );
}

/// A peer that disappears does not take its region with it.
///
/// §7.4's second hard part. The checkpoint is what makes this survivable: the
/// cost of a peer vanishing is the work since it was last checked, not the
/// region.
#[test]
fn a_lost_peer_hands_its_region_back_with_its_last_agreed_state() {
    let (owner, chunks) = fixture();
    let peer = PeerId(1);
    let mut coord = Coordinator::new();
    coord.claim(peer, &chunks).expect("a free region");
    let agreed = owner.extract_shard(&chunks);
    coord.checkpoint(peer, agreed.clone());

    let (returned, last) = coord
        .peer_lost(peer)
        .expect("the peer was holding something");
    assert_eq!(returned, chunks, "the region did not come back");
    let last = last.expect("the last agreed state came back with it");
    assert_eq!(last.block_entities, agreed.block_entities);
    assert_eq!(last.entities, agreed.entities);
    assert_eq!(coord.peer_count(), 0, "the lost peer is still listed");

    // And the region is claimable again, which is what "recovered" means.
    coord
        .claim(PeerId(2), &chunks)
        .expect("the region is free again");
}

/// Two peers cannot hold the same chunk.
///
/// Refused rather than resolved: deciding who wins an overlap **is** boundary
/// handoff, which §7.4 names as the hard part this block does not do. A
/// coordinator that silently reassigned would be inventing that rule instead of
/// leaving it to be designed.
#[test]
fn a_second_claim_on_a_held_region_is_refused() {
    let (_owner, chunks) = fixture();
    let mut coord = Coordinator::new();
    coord.claim(PeerId(1), &chunks).expect("a free region");

    assert_eq!(
        coord.claim(PeerId(2), &chunks[..3]),
        Err(ClaimError::Overlaps { held_by: PeerId(1) }),
        "two peers were allowed to own the same chunks"
    );
    assert_eq!(coord.holder_of(chunks[0]), Some(PeerId(1)));

    // A disjoint region is fine, which is the point of refusing only overlaps.
    let far = ChunkCoord::new(-900, 4, -900);
    coord.claim(PeerId(2), &[far]).expect("a disjoint region");
    assert_eq!(coord.holder_of(far), Some(PeerId(2)));
}
