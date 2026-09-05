//! Phase 2's three interest-management gate criteria (`ROADMAP.md`), as tests.
//!
//! Block 2.11. All three run **in-process, with no socket** — deliberately, per
//! `docs/PHASE2_MULTIPLAYER.md` §5: everything that can be built and tested
//! before the transport should be, because a headless tick loop is testable to
//! the tick where a networked one is testable to the flake.
//!
//! The criteria, in `ROADMAP.md`'s words:
//!
//! - *Two clients, one world, one hash.*
//! - *The server never sends terrain.*
//! - *Bandwidth per client does not grow with player count.*

use cubara_server::view::VIEW_RADIUS;
use cubara_server::{Effect, Server};
use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs};
use cubara_voxel::{Angle, BlockId, Chunk, FixedVec3};

/// A fresh world with the real assets and nothing loaded from disk.
fn server() -> Server {
    let mut s = Server::new();
    s.open(std::path::Path::new(
        "cubara-nonexistent-client-view-fixture",
    ));
    s.place_player_on_ground();
    s
}

/// Put a player at a block position and give them a view.
fn add_player(s: &mut Server, block: [i32; 3]) -> PlayerId {
    let id = s.sim.join(Player::new(
        FixedVec3::from_blocks(block[0], block[1], block[2]),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(id);
    id
}

/// One whole tick: the simulation, then the world.
fn tick(s: &mut Server, inputs: &PlayerInputs) {
    s.tick_sim_all(inputs);
    s.tick_world();
}

// ---------------------------------------------------------------------------
// Criterion 1 — two clients, one world, one hash.
// ---------------------------------------------------------------------------

/// Two clients in one world, both driven, and the world stays one world.
///
/// The multiplayer sibling of the survival replay: the point is not that
/// anything interesting happens, it is that two clients acting on the same
/// server leave a single authoritative state that neither of them privately
/// disagrees with.
#[test]
fn two_clients_in_one_world_leave_one_authoritative_state() {
    let mut a = server();
    let local = a.local;
    let ground = a.sim.player(local).pos;
    let second = add_player(
        &mut a,
        [
            ground.x.floor_block() + 6,
            ground.y.floor_block(),
            ground.z.floor_block() + 6,
        ],
    );

    assert_eq!(a.sim.player_count(), 2, "two players are in the world");

    // Both act: one breaks a block under its feet, the other elsewhere.
    let under = |s: &Server, who: PlayerId| {
        let p = s.sim.player(who).pos;
        [p.x.floor_block(), p.y.floor_block() - 2, p.z.floor_block()]
    };
    let a_block = under(&a, local);
    let b_block = under(&a, second);
    a.break_at(a_block);
    a.break_at(b_block);

    for _ in 0..20 {
        tick(&mut a, &PlayerInputs::default());
    }

    // Replay the identical script on a second server. Same events, same order,
    // same world -- and the hash is what says so, at one worker and at six.
    let mut b = server();
    let b_local = b.local;
    add_player(
        &mut b,
        [
            ground.x.floor_block() + 6,
            ground.y.floor_block(),
            ground.z.floor_block() + 6,
        ],
    );
    assert_eq!(b_local, local, "the local id is not positional luck");
    b.break_at(a_block);
    b.break_at(b_block);
    for _ in 0..20 {
        tick(&mut b, &PlayerInputs::default());
    }

    assert_eq!(
        a.hash_with_workers(1),
        b.hash_with_workers(1),
        "two identical two-player runs reached different worlds"
    );
    assert_eq!(
        a.hash_with_workers(1),
        a.hash_with_workers(6),
        "a two-player world's hash depends on how many threads computed it"
    );
}

/// Each client is told about the other, and never about itself.
///
/// §3.4 splits those deliberately: a client predicts its own player and is
/// corrected, and interpolates everyone else because it does not know their
/// inputs. Echoing someone their own position would be useless now and, once
/// there is latency, actively harmful — it would fight the prediction.
#[test]
fn a_client_hears_about_the_other_player_and_not_itself() {
    let mut s = server();
    let local = s.local;
    let p = s.sim.player(local).pos;
    let other = add_player(
        &mut s,
        [
            p.x.floor_block() + 3,
            p.y.floor_block(),
            p.z.floor_block() + 3,
        ],
    );

    tick(&mut s, &PlayerInputs::default());

    let mine = s.drain_effects_for(local);
    let moves: Vec<PlayerId> = mine
        .iter()
        .filter_map(|e| match e {
            Effect::PlayerMoved { who, .. } => Some(*who),
            _ => None,
        })
        .collect();

    assert!(
        moves.contains(&other),
        "the local client was never told where the other player is"
    );
    assert!(
        !moves.contains(&local),
        "the client was sent its own position back"
    );
}

// ---------------------------------------------------------------------------
// Criterion 2 — the server never sends terrain.
// ---------------------------------------------------------------------------

/// The join handshake carries the seed and the edits, and nothing else.
///
/// This is the claim the whole player-count target rests on
/// (`PHASE2_MULTIPLAYER.md` §3): terrain is a pure function of the seed, already
/// proven bit-identical on both CI platforms, so the client generates it.
/// Bandwidth then scales with how much players have *changed* the world rather
/// than with how much of it they can see — which for five thousand players is
/// the difference between feasible and not.
///
/// Checked structurally rather than by inspecting bytes: `Effect` has no variant
/// that *can* carry a chunk, so what this really pins is that nobody adds one.
#[test]
fn the_join_handshake_carries_no_terrain() {
    let mut s = server();
    let local = s.local;

    // Change the world, so the handshake has something real in it and the test
    // cannot pass by the handshake being empty.
    let p = s.sim.player(local).pos;
    let near = [p.x.floor_block(), p.y.floor_block() - 2, p.z.floor_block()];
    s.break_at(near);

    let handshake = s.snapshot_for(local);
    assert!(
        !handshake.is_empty(),
        "the handshake is empty, so this proves nothing"
    );

    for effect in &handshake {
        match effect {
            // The only two things a handshake may contain.
            Effect::Edit { .. } | Effect::BlockEntity { .. } => {}
            other => panic!("the join handshake carried something that is not an edit: {other:?}"),
        }
    }

    // And its size is bounded by what has been edited, not by the world.
    assert_eq!(
        handshake.len(),
        s.world.edit_count(),
        "the handshake is not exactly the edits in view"
    );
}

/// A joining client is told about edits it can see, and not about edits it
/// cannot — which is what makes the handshake's size a property of the
/// neighbourhood rather than of how long the server has been up.
#[test]
fn the_handshake_is_filtered_to_what_the_joiner_can_see() {
    let mut s = server();
    let local = s.local;
    let p = s.sim.player(local).pos;

    let near = [p.x.floor_block(), p.y.floor_block() - 2, p.z.floor_block()];
    let far = [
        p.x.floor_block() + (VIEW_RADIUS + 8) * Chunk::SIZE as i32,
        p.y.floor_block(),
        p.z.floor_block(),
    ];
    s.set_block(near, BlockId::AIR);
    s.set_block(far, BlockId::AIR);

    let handshake = s.snapshot_for(local);
    let carried: Vec<[i32; 3]> = handshake
        .iter()
        .filter_map(|e| match e {
            Effect::Edit { pos, .. } => Some(*pos),
            _ => None,
        })
        .collect();

    assert!(carried.contains(&near), "the nearby edit was left out");
    assert!(
        !carried.contains(&far),
        "an edit {} chunks away was sent to a joiner who cannot see it",
        VIEW_RADIUS + 8
    );
}

// ---------------------------------------------------------------------------
// Criterion 3 — bandwidth per client does not grow with player count.
// ---------------------------------------------------------------------------

/// **The criterion that decides whether the player-count target is reachable.**
///
/// With players spread across the world, the bytes owed to *one* client per tick
/// must not grow as other players are added. Measured at two counts an order of
/// magnitude apart.
///
/// This is the honest, testable form of the owner's *"de structuur er is om 5000
/// spelers in 1 wereld aan te kunnen"* (`PHASE2_MULTIPLAYER.md` §6): what decides
/// feasibility is the **scaling shape** — O(what a client can perceive) rather
/// than O(all players) — and a live five-thousand-client demonstration is not
/// something a CI runner can give. A test that showed one would be theatre; this
/// one shows the property that matters.
///
/// # Why the crowded case is measured too
///
/// Spread out, the right answer is **zero** bytes: nobody is near, so nobody is
/// news. That makes `flat` and `disconnected` look identical — a version of this
/// test that only compared 10 against 1,000 would pass just as happily if player
/// replication had never been written, which is the failure mode worth guarding
/// against and not an imaginary one.
///
/// So the crowd is measured in the same test, and the assertion is a *shape*:
/// flat as the world fills up, and strictly larger when people are actually
/// standing next to you. The second half is what proves the first half is
/// measuring something.
#[test]
fn bytes_to_one_client_do_not_grow_with_the_player_count() {
    // Far enough apart that no two are in each other's view. The chunk pitch
    // times more than twice the radius guarantees it.
    let spacing = (2 * VIEW_RADIUS + 4) * Chunk::SIZE as i32;

    // `spread` places each extra player a full view apart; otherwise they all
    // stand on the client's doorstep.
    //
    // **Everyone moves every tick.** They used to stand still, and the crowded
    // measurement leaned on the fact that a motionless player still cost a full
    // message -- which was itself the defect
    // `a_player_who_does_not_move_costs_nothing_to_watch` now forbids. Fixing
    // that turned the crowd's cost to zero and this test failed, correctly, on
    // its own "the pipe is not dead" guard.
    //
    // Moving players is the honest scenario anyway: interest management is about
    // bounding the traffic that *activity* near you generates, and activity is
    // what a moving player is.
    let measure = |others: i32, spread: bool| -> usize {
        let mut s = server();
        let local = s.local;
        let home = s.sim.player(local).pos;

        let mut added = Vec::new();
        for i in 1..=others {
            let offset = if spread { i * spacing } else { 2 };
            added.push(add_player(
                &mut s,
                [
                    home.x.floor_block() + offset,
                    home.y.floor_block(),
                    home.z.floor_block(),
                ],
            ));
        }

        let shuffle = |s: &mut Server, n: i64| {
            for &id in &added {
                let p = s.sim.player(id).pos;
                s.sim.player_mut(id).pos =
                    FixedVec3::new(p.x + cubara_voxel::Fixed::from_raw(n), p.y, p.z);
            }
        };

        // Settle first: the opening tick backfills, which is a join cost and not
        // a per-tick one. Drain it, then measure the steady state.
        shuffle(&mut s, 64);
        tick(&mut s, &PlayerInputs::default());
        let _ = s.drain_effects_for(local);

        shuffle(&mut s, 64);
        tick(&mut s, &PlayerInputs::default());
        s.drain_effects_for(local)
            .iter()
            .map(Effect::wire_size)
            .sum()
    };

    let few = measure(10, true);
    let many = measure(1_000, true);
    let crowded = measure(10, false);

    assert_eq!(
        few, many,
        "bytes to one client per tick went from {few} with 10 players to {many} with \
         1,000 spread across the world. Interest management is what stops that \
         growing, and if this fires the world cannot hold the player count \
         ROADMAP.md's gate asks for."
    );
    assert!(
        crowded > few,
        "ten players standing next to the client cost {crowded} bytes and ten \
         players spread across the world cost {few}. If those are equal, the flat \
         curve above is flat because nothing is being sent at all -- which would \
         make this whole criterion measure an empty room rather than a filter."
    );
}

/// Every effect costs something, and they do not all cost the same. A
/// `wire_size` that returned a constant would make the scaling test meaningless
/// in a way nothing else would catch.
#[test]
fn wire_size_reflects_what_an_effect_carries() {
    let edit = Effect::Edit {
        pos: [1, 2, 3],
        block: BlockId(4),
    };
    let moved = Effect::PlayerMoved {
        who: PlayerId(1),
        pos: FixedVec3::from_blocks(1, 2, 3),
        yaw: Angle::ZERO,
        pitch: Angle::ZERO,
    };
    let gone = Effect::PlayerGone(PlayerId(1));

    assert!(edit.wire_size() > 0);
    assert!(
        moved.wire_size() > gone.wire_size(),
        "a pose costs more than an id, or the accounting is not accounting"
    );
    assert!(
        moved.wire_size() > edit.wire_size(),
        "a fixed-point pose plus two angles is bigger than a block id"
    );
}

/// A player who sends no input still has a view that tracks them.
#[test]
fn a_view_follows_its_player() {
    let mut s = server();
    let local = s.local;

    let start = s
        .view(local)
        .and_then(|v| v.centre())
        .expect("the local view is centred once the player is placed");

    // Teleport far enough to change chunk, then let a tick notice.
    let p = s.sim.player(local).pos;
    s.sim.player_mut(local).pos = FixedVec3::from_blocks(
        p.x.floor_block() + 5 * Chunk::SIZE as i32,
        p.y.floor_block(),
        p.z.floor_block(),
    );
    tick(&mut s, &PlayerInputs::one(local, InputFrame::default()));

    let moved = s.view(local).and_then(|v| v.centre()).expect("still there");
    assert_ne!(moved, start, "the view did not follow its player");
}

/// **A player who is not moving costs nothing to watch.**
///
/// The companion the bandwidth criterion was missing. That one measures bytes
/// against *player count* and requires the curve to be flat — which it is, and
/// was, while every standing player still cost a full message every tick. Over a
/// real LAN this showed up as 299 ticks producing 299 byte-identical
/// `PlayerMoved` messages for someone who had not moved at all.
///
/// Flat in player count and wasteful per player are different properties, and
/// only one of them was being checked. For a world meant to hold five thousand
/// people — most of whom, at any instant, are standing still — the difference is
/// most of the bandwidth.
#[test]
fn a_player_who_does_not_move_costs_nothing_to_watch() {
    let mut s = server();
    let local = s.local;
    let home = s.sim.player(local).pos;

    // Someone standing near enough to be watched, and not moving.
    add_player(
        &mut s,
        [
            home.x.floor_block() + 2,
            home.y.floor_block(),
            home.z.floor_block(),
        ],
    );

    // Let the world settle: joining, backfill and the first physics step are
    // real movement and legitimately cost bytes. What is measured is the steady
    // state afterwards.
    for _ in 0..30 {
        tick(&mut s, &PlayerInputs::default());
        let _ = s.drain_effects_for(local);
    }

    let mut bytes = 0usize;
    for _ in 0..20 {
        tick(&mut s, &PlayerInputs::default());
        bytes += s
            .drain_effects_for(local)
            .iter()
            .map(Effect::wire_size)
            .sum::<usize>();
    }

    assert_eq!(
        bytes, 0,
        "watching a motionless player for 20 ticks cost {bytes} bytes. Nothing          changed in those ticks, so nothing should have been sent."
    );
}

/// The other half, so the test above cannot be satisfied by never sending
/// anything: a player who *does* move must still be reported.
#[test]
fn a_player_who_moves_is_still_reported() {
    let mut s = server();
    let local = s.local;
    let home = s.sim.player(local).pos;
    let other = add_player(
        &mut s,
        [
            home.x.floor_block() + 2,
            home.y.floor_block(),
            home.z.floor_block(),
        ],
    );

    for _ in 0..30 {
        tick(&mut s, &PlayerInputs::default());
        let _ = s.drain_effects_for(local);
    }

    // Move them, by hand, so this does not depend on physics doing anything.
    let p = s.sim.player(other).pos;
    s.sim.player_mut(other).pos =
        FixedVec3::new(p.x + cubara_voxel::Fixed::from_f32(1.0), p.y, p.z);
    tick(&mut s, &PlayerInputs::default());

    let moves = s
        .drain_effects_for(local)
        .into_iter()
        .filter(|e| matches!(e, Effect::PlayerMoved { who, .. } if *who == other))
        .count();
    assert!(
        moves > 0,
        "a player who moved was not reported, so the idle test above is passing          because nothing is ever sent"
    );
}

/// A player who has been standing still since before you arrived is still
/// somebody you have to be told about.
///
/// The trap in "only send what changed": the change happened before this viewer
/// existed. Without a backfill, walking towards a motionless person means
/// walking towards an empty patch of world — which is a worse bug than the
/// bandwidth one, and the obvious fix introduces it.
#[test]
fn a_motionless_player_is_backfilled_to_someone_who_walks_up() {
    let mut s = server();
    let local = s.local;
    let home = s.sim.player(local).pos;

    // Far enough that the local client cannot see them at all.
    let far = (VIEW_RADIUS + 4) * Chunk::SIZE as i32;
    let other = add_player(
        &mut s,
        [
            home.x.floor_block() + far,
            home.y.floor_block(),
            home.z.floor_block(),
        ],
    );

    for _ in 0..10 {
        tick(&mut s, &PlayerInputs::default());
        let _ = s.drain_effects_for(local);
    }

    // Walk over to them without either of them having moved a muscle since.
    let there = s.sim.player(other).pos;
    s.sim.player_mut(local).pos = FixedVec3::new(there.x, there.y, there.z);
    tick(&mut s, &PlayerInputs::default());

    let told = s
        .drain_effects_for(local)
        .into_iter()
        .any(|e| matches!(e, Effect::PlayerMoved { who, .. } if who == other));
    assert!(
        told,
        "walked up to a motionless player and was never told they were there"
    );
}
