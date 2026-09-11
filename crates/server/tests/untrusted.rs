//! Block 2.14 — what a client may say, and what the server does about it.
//!
//! `ROADMAP.md`: *reach, speed, inventory and rate validated server-side.*
//! `docs/RESEARCH_MULTIPLAYER.md` §3.4 is the rule underneath:
//!
//! > A client may simulate anything it can derive from data it already has, and
//! > may never be **believed** about any of it.
//!
//! Most of that rule is already structural rather than checked, and the tests
//! here are as much a record of *which* parts as of the new ones:
//!
//! - **Reach** is free for `Break`, `Place` and `Interact`, because the server
//!   raycasts from the player and a raycast stops at `REACH`. There is no
//!   message in which a client names a block. `ClickFurnace` is the exception —
//!   it names a position, and until this block nothing stopped that position
//!   being on the other side of the world.
//! - **Speed** is bounded by the walking code normalising its direction, so a
//!   large `move_axes` cannot make a player fast. That was true by accident
//!   rather than by statement, and `InputFrame::sanitized` now says it.
//! - **Rate** is capped per client per tick.
//!
//! What is deliberately **not** here, so that its absence is a decision rather
//! than an oversight: `InputFrame::toggle_fly` lets any client enter free-fly,
//! which is noclip. Whether a server permits that is a rule about what the game
//! *is*, not a question about what a client may be believed about, and the
//! project owner has deferred it to a future set of game rules rather than
//! having it invented here. It is a hole, it is a known one, and it is not this
//! block's to close.

use cubara_server::{Action, FurnaceSlot, Server};
use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs};
use cubara_voxel::{Angle, FixedVec3};

/// Any colour will do here: what is being tested is the exchange, not the
/// paint. A real client hard-codes its own (`cubara::game::MY_SHIRT`).
const TEST_SHIRT: cubara_server::wire::Shirt = [10, 20, 30];

const SKY: i32 = 400;

/// One player in empty sky, and a furnace they can see.
fn server_with_player_and_furnace() -> (Server, PlayerId, [i32; 3]) {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-untrusted-fixture"));
    let who = s.sim.join(Player::new(
        FixedVec3::from_blocks(0, SKY, 0),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(who);
    let near = [0, SKY, -3];
    s.add_furnace(near);
    (s, who, near)
}

/// Something burnable, in the player's hand.
fn fill_hand(s: &mut Server, who: PlayerId) -> cubara_voxel::ItemId {
    let items = s.items.as_ref().expect("assets loaded");
    let log = items.id_of("cubara:oak_log").expect("oak log is an item");
    let stack = items.new_stack(log, 1).expect("a stack");
    s.sim.player_mut(who).crafting.set_held(Some(stack));
    log
}

// ---------------------------------------------------------------------------
// Reach
// ---------------------------------------------------------------------------

/// A furnace within arm's length can be used.
///
/// First, because a rejection test that has never accepted anything is a
/// rejection test that might reject everything.
#[test]
fn a_furnace_in_reach_can_be_clicked() {
    let (mut s, who, near) = server_with_player_and_furnace();
    let log = fill_hand(&mut s, who);

    s.apply_as(
        who,
        Action::ClickFurnace {
            pos: near,
            slot: FurnaceSlot::Fuel,
        },
    );

    assert_eq!(
        s.world
            .furnace_at(near)
            .expect("the furnace")
            .fuel
            .map(|(i, _)| i),
        Some(log),
        "a furnace three blocks away was refused"
    );
}

/// A furnace across the world cannot.
///
/// The click is perfectly well-formed and the furnace really is there — which
/// is why the "does this furnace exist" check that was already in place could
/// not catch it. Only the distance separates this from the test above.
#[test]
fn a_furnace_out_of_reach_cannot_be_clicked() {
    let (mut s, who, _near) = server_with_player_and_furnace();
    let far = [500, SKY, 0];
    s.add_furnace(far);
    fill_hand(&mut s, who);

    s.apply_as(
        who,
        Action::ClickFurnace {
            pos: far,
            slot: FurnaceSlot::Fuel,
        },
    );

    assert_eq!(
        s.world.furnace_at(far).expect("the furnace").fuel,
        None,
        "a client fuelled a furnace 500 blocks away"
    );
    assert!(
        s.sim.player(who).crafting.held().is_some(),
        "the item left the player's hand even though the click was refused"
    );
}

/// The boundary is where a raycast could actually have hit, not a bare `REACH`.
///
/// A block whose *centre* is further than `REACH` can still be legitimately
/// targeted, because a ray stops when it enters the block rather than at its
/// middle. This pins the slack so that tightening it later is a deliberate act
/// rather than a tidy-up.
#[test]
fn the_reach_limit_allows_what_a_raycast_could_have_hit() {
    let (mut s, who, _near) = server_with_player_and_furnace();
    fill_hand(&mut s, who);

    // Diagonally out: centre at (4.5, 0.5, -4.5) from the eye is ~6.38 away,
    // past `REACH` — while the block's nearest corner is ~5.66 away, so a ray
    // reaches it comfortably. That gap is exactly what the slack is for, and
    // the first version of this test used an axis-aligned block whose centre
    // was 5.5 away: inside a bare `REACH`, so it passed with the slack removed
    // and proved nothing about it.
    let edge = [4, SKY, -5];
    s.add_furnace(edge);
    s.apply_as(
        who,
        Action::ClickFurnace {
            pos: edge,
            slot: FurnaceSlot::Fuel,
        },
    );
    assert!(
        s.world
            .furnace_at(edge)
            .expect("the furnace")
            .fuel
            .is_some(),
        "a furnace a raycast could have reached was refused"
    );

    // Well past it: no ray from this eye reaches here.
    let beyond = [0, SKY, -12];
    s.add_furnace(beyond);
    s.apply_as(
        who,
        Action::ClickFurnace {
            pos: beyond,
            slot: FurnaceSlot::Fuel,
        },
    );
    assert!(
        s.world
            .furnace_at(beyond)
            .expect("the furnace")
            .fuel
            .is_none(),
        "the reach limit is so loose it accepts twice the reach"
    );
}

// ---------------------------------------------------------------------------
// Speed, and what a client may put in a float
// ---------------------------------------------------------------------------

/// A hostile `move_axes` is cleaned before it reaches the simulation.
#[test]
fn a_hostile_move_axis_is_cleaned() {
    let hostile = InputFrame {
        move_axes: [f32::NAN, f32::INFINITY, 1e30],
        ..InputFrame::default()
    };
    let clean = hostile.sanitized();

    assert_eq!(
        clean.move_axes[0], 0.0,
        "NaN survived into the simulation's input"
    );
    assert_eq!(
        clean.move_axes[1], 0.0,
        "infinity survived into the simulation's input"
    );
    assert_eq!(
        clean.move_axes[2], 1.0,
        "a huge magnitude was not clamped to the documented range"
    );
}

/// Cleaning does not touch input a real client sends.
#[test]
fn ordinary_input_survives_cleaning_unchanged() {
    let ordinary = InputFrame {
        move_axes: [-1.0, 0.0, 0.5],
        look_delta: [Angle::from_raw(1234), Angle::from_raw(-99)],
        jump: true,
        toggle_fly: false,
        breaking: true,
    };
    assert_eq!(
        ordinary.sanitized(),
        ordinary,
        "sanitising changed input that was already valid"
    );
}

/// A client cannot outrun the walk speed by shouting.
///
/// The bound comes from the walking code normalising its direction, which this
/// asserts rather than assumes: it is the kind of property that is true until
/// someone simplifies the expression that made it true.
#[test]
fn a_large_move_axis_does_not_move_a_player_further() {
    let step = |axes: [f32; 3]| {
        let mut s = Server::new();
        s.open(std::path::Path::new("cubara-nonexistent-untrusted-fixture"));
        s.place_player_on_ground();
        let who = s.local.expect("a local client");
        let before = s.sim.player(who).pos;
        let input = InputFrame {
            move_axes: axes,
            ..InputFrame::default()
        };
        s.tick_sim(&input.sanitized());
        let after = s.sim.player(who).pos;
        (after.z - before.z).to_f32().abs()
    };

    let honest = step([0.0, 0.0, 1.0]);
    let greedy = step([0.0, 0.0, 1e6]);
    assert!(honest > 0.0, "the honest input did not move the player");
    assert_eq!(
        honest, greedy,
        "asking to move a million times harder moved the player further"
    );
}

// ---------------------------------------------------------------------------
// Rate
// ---------------------------------------------------------------------------

/// A client that floods gets `MAX_ACTIONS_PER_TICK` of its actions and no more.
///
/// Driven through a real `Session` and a real link, because the cap lives in
/// `collect_input` and testing it anywhere else would be testing a copy of it.
///
/// The first version of this test asserted that the *constant* was a plausible
/// size. That passes whether or not anything enforces it, which is the failure
/// this whole file exists to argue against — so `Session` now counts refusals
/// and the test reads the count.
#[test]
fn a_flood_of_actions_is_capped_at_the_limit() {
    use cubara_server::headless::{Config, Session, MAX_ACTIONS_PER_TICK};
    use cubara_server::wire::ClientMessage;

    let cfg = Config {
        world: std::path::PathBuf::from("cubara-nonexistent-untrusted-fixture"),
        autosave_ticks: 0,
        ..Config::default()
    };
    let mut session = Session::open(&cfg);

    // **Attached in-process, not over a socket.** The cap is a per-*tick*
    // property, and testing it across a real connection measured something else
    // entirely: whether fifty messages happened to arrive inside one tick. They
    // did, until the server got faster -- removing a dedicated server's ghost
    // player made ticks cheaper, the burst spread over more of them, and the
    // test began failing about one run in four. It had already been rewritten
    // once for a race, which is the sign it was racing something it should not
    // have been touching.
    //
    // A local link delivers into the channel before `advance` is called, so the
    // burst is in one tick by construction rather than by luck. It joins through
    // the same `welcome` path a TCP client does, so nothing about the cap is
    // being tested in a way that would not hold over a wire.
    let mut link = session.attach();
    link.send(ClientMessage::Hello(TEST_SHIRT));
    assert_eq!(session.client_count(), 1, "the client never joined");

    const SENT: usize = 50;
    for _ in 0..SENT {
        // Any action will do: what is being measured is how many of them the
        // server lets through in one tick, not what they were. `Interact` is
        // the cheapest -- it raycasts and finds nothing.
        link.send(ClientMessage::Act(Action::Interact));
    }

    // One tick. Everything the client sent is already in the channel, so this is
    // the burst the cap is supposed to be about.
    session.advance(1, &cfg);

    let dropped = session.dropped_actions();
    assert_eq!(
        dropped,
        (SENT - MAX_ACTIONS_PER_TICK) as u64,
        "a client sent {SENT} actions in one tick and {} were let through; the \
         cap is {MAX_ACTIONS_PER_TICK}",
        SENT as u64 - dropped
    );
    // The invariant, and the only assertion here that does not depend on
    // timing. An earlier version compared `dropped` against the total sent,
    // which measures how the burst happened to split across ticks -- a property
    // of the socket, not of the cap. It failed about five runs in six on this
    // machine while the cap was working perfectly.
    assert!(
        session.most_actions_in_one_tick() <= MAX_ACTIONS_PER_TICK,
        "one client had {} actions applied in a single tick, cap is {}",
        session.most_actions_in_one_tick(),
        MAX_ACTIONS_PER_TICK
    );
}

// ---------------------------------------------------------------------------
// Mining time
// ---------------------------------------------------------------------------

/// Holding the button for less than a block's hardness does not break it.
fn mine_for(ticks: u32) -> (Server, [i32; 3], bool) {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-untrusted-fixture"));
    let who = s.sim.join(Player::new(
        FixedVec3::from_blocks(0, SKY, 0),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(who);
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");
    let target = [0, SKY, -3];
    s.set_block(target, stone);

    let holding = InputFrame {
        breaking: true,
        ..InputFrame::default()
    };
    // Only the mining half. Ticking the simulation too would let gravity pull
    // the player away from the block they are aiming at -- they are standing in
    // empty sky -- and the block would survive because the ray stopped hitting
    // it, not because the time had not been paid. The first version of this
    // test did exactly that and reported "stone never gave way".
    for _ in 0..ticks {
        s.tick_mining_all(&PlayerInputs::one(who, holding));
    }
    let gone = s
        .world
        .block_at(target[0], target[1], target[2], s.terrain())
        == cubara_voxel::BlockId::AIR;
    (s, target, gone)
}

/// Stone declares `hardness: 30`, so a bare hand at speed 1 needs thirty ticks.
///
/// Both halves matter. That it breaks eventually says the mechanism works; that
/// it does *not* break early says the time is being counted rather than
/// nodded at.
#[test]
fn a_block_is_not_broken_before_its_hardness_is_paid() {
    let (_s, _t, early) = mine_for(20);
    assert!(
        !early,
        "stone gave way after 20 ticks of a 30-tick hardness"
    );

    let (_s, _t, late) = mine_for(30);
    assert!(
        late,
        "stone never gave way after 30 ticks of holding the button"
    );
}

/// There is no message a client can send that asks for an instant break.
///
/// The structural half of this block, and the reason `Action::Break` was
/// removed rather than validated: the server cannot check a duration it did not
/// measure, so a message that asks for a completed break is one it would have
/// to take on trust. Tag 0 is what that action encoded as; it is now refused.
#[test]
fn the_wire_has_no_instant_break() {
    use cubara_server::wire::ClientMessage;
    // `Act` is client tag 2; 0 was `Break`'s action tag.
    assert!(
        ClientMessage::decode(&[2, 0]).is_err(),
        "the wire still accepts the instant-break action"
    );
    // The neighbouring tags still decode, so this is a rejection of one message
    // rather than the decoder failing on everything.
    assert!(
        ClientMessage::decode(&[2, 1]).is_ok(),
        "Place stopped decoding, so the test above proves nothing"
    );
}

/// Progress is abandoned when the button comes up, not banked (§4.3).
///
/// Otherwise a client could pay for a break twenty ticks at a time across a
/// minute of tapping, which is the rate limit defeated by patience.
#[test]
fn released_progress_is_not_banked() {
    let mut s = Server::new();
    s.open(std::path::Path::new("cubara-nonexistent-untrusted-fixture"));
    let who = s.sim.join(Player::new(
        FixedVec3::from_blocks(0, SKY, 0),
        Angle::ZERO,
        Angle::ZERO,
    ));
    s.open_view(who);
    let stone = s
        .blocks_registry
        .as_deref()
        .and_then(|r| r.id_of("cubara:stone"))
        .expect("stone is a block");
    let target = [0, SKY, -3];
    s.set_block(target, stone);

    let holding = InputFrame {
        breaking: true,
        ..InputFrame::default()
    };
    // Three bursts of twenty, released in between: sixty ticks of holding in
    // total, none of them consecutive enough to finish a thirty-tick block.
    for _ in 0..3 {
        for _ in 0..20 {
            s.tick_mining_all(&PlayerInputs::one(who, holding));
        }
        s.tick_mining_all(&PlayerInputs::one(who, InputFrame::default()));
    }

    assert_ne!(
        s.world
            .block_at(target[0], target[1], target[2], s.terrain()),
        cubara_voxel::BlockId::AIR,
        "sixty ticks of tapping broke a block that needs thirty consecutive"
    );
}

// ---------------------------------------------------------------------------
// Saving off the tick loop (block 2.15)
// ---------------------------------------------------------------------------

/// A session's background save produces a world that loads.
///
/// The writer runs on another thread, so the assertion has to come after the
/// join — `finish_save` is what a shutdown calls, and what `Drop` calls for
/// every other way a session ends.
#[test]
fn a_background_save_produces_a_loadable_world() {
    use cubara_server::headless::{Config, Session};

    let dir = std::env::temp_dir().join("cubara-bg-save");
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = Config {
        world: dir.clone(),
        autosave_ticks: 0,
        ..Config::default()
    };

    let mut session = Session::open(&cfg);
    session.advance(5, &cfg);
    session.save_in_background(&dir);
    session.finish_save();

    assert!(
        dir.join("level.ron").is_file(),
        "the background writer never wrote the header"
    );

    // It loads, and it is the world that was saved.
    let mut restored = Session::open(&cfg);
    restored.advance(1, &cfg);
    assert!(
        restored.server.sim.tick >= 5,
        "the reloaded world had not kept the ticks that were saved"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// No writer thread is ever abandoned.
///
/// Dropping a `JoinHandle` does not stop the thread, it **detaches** it — so a
/// `save` that forgot to wait for the previous one would leave two writers
/// renaming into the same directory, and which files came from which save would
/// depend on the disk. Nothing would look wrong until it did.
///
/// Asserted on the counters rather than on the directory contents, because two
/// atomic writers racing still leave every individual file whole: the damage is
/// a *mixture* of two saves, which is invisible in any single file. An earlier
/// version of this test checked for leftover temporaries and passed happily
/// with the wait removed.
#[test]
fn no_save_writer_is_left_detached() {
    use cubara_server::headless::{Config, Session};

    let dir = std::env::temp_dir().join("cubara-bg-save-twice");
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = Config {
        world: dir.clone(),
        autosave_ticks: 0,
        ..Config::default()
    };

    let mut session = Session::open(&cfg);
    session.advance(3, &cfg);
    session.save_in_background(&dir);
    session.advance(3, &cfg);
    session.save_in_background(&dir);
    session.finish_save();

    assert_eq!(
        session.writers_outstanding(),
        0,
        "a save writer was started and never waited for"
    );

    // And no debris: a torn or abandoned temporary would still be here.
    let debris: Vec<String> = walk(&dir)
        .into_iter()
        .filter(|n| n.contains(".writing"))
        .collect();
    assert!(debris.is_empty(), "temporaries left behind: {debris:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under `root`, as slash-separated relative paths.
fn walk(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

/// `save` waits; `save_in_background` does not, and says so in its name.
///
/// This is a regression test with a story. Block 2.15 moved saving onto a
/// worker thread and gave `save` that behaviour, which quietly turned every
/// existing caller into a race — `a_world_survives_a_restart` saved and
/// reopened the directory immediately, and began failing on macOS CI while
/// passing on Windows and locally. Atomic writes meant the files it did see
/// were whole; they were simply not all there yet.
///
/// The fix was to give the surprising behaviour the surprising name. This pins
/// that: after `save` returns, the world is on disk.
#[test]
fn save_returns_only_once_the_world_is_on_disk() {
    use cubara_server::headless::{Config, Session};

    let dir = std::env::temp_dir().join("cubara-save-is-sync");
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = Config {
        world: dir.clone(),
        autosave_ticks: 0,
        ..Config::default()
    };

    let mut session = Session::open(&cfg);
    session.advance(5, &cfg);
    session.save(&dir);

    // No join, no sleep: if `save` were the background one this would race, and
    // on a loaded CI runner it would lose.
    assert!(
        dir.join("level.ron").is_file(),
        "`save` returned before the header was on disk"
    );
    assert!(
        dir.join("players").is_dir(),
        "`save` returned before the players were on disk"
    );
    assert_eq!(
        session.writers_outstanding(),
        0,
        "`save` returned with a writer still running"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A hotbar index a real client cannot produce is ignored, not clamped.
///
/// `Action::SelectSlot` is authority — which slot is held decides what a place
/// spends and what a break is mined with — so a client sending 200 is a client
/// naming a slot that does not exist. `Inventory::select` clamps, which is the
/// right answer for a UI bug reaching it from inside the process; over the wire
/// clamping would silently turn nonsense into a *choice*, and the choice it
/// invents is "the last slot", which is not what anybody asked for.
///
/// Found by `scripts/check-tests-can-fail.sh`: the bounds check was written and
/// nothing could see it removed, because the clamp underneath hid the
/// difference.
#[test]
fn an_impossible_hotbar_slot_is_ignored_rather_than_clamped() {
    let (mut s, who, _) = server_with_player_and_furnace();

    s.apply_as(who, Action::SelectSlot(3));
    assert_eq!(
        s.sim.player(who).inventory.selected_slot(),
        3,
        "an ordinary selection did not take effect, so this proves nothing"
    );

    s.apply_as(who, Action::SelectSlot(200));
    assert_eq!(
        s.sim.player(who).inventory.selected_slot(),
        3,
        "a slot that does not exist moved the selection instead of being ignored"
    );
}
