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

use cubara_server::headless::MAX_ACTIONS_PER_TICK;
use cubara_server::{Action, FurnaceSlot, Server};
use cubara_sim::{InputFrame, Player, PlayerId};
use cubara_voxel::{Angle, FixedVec3};

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
        let who = s.local;
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
    use cubara_server::headless::{Config, Session};
    use cubara_server::net::connect;
    use cubara_server::wire::ClientMessage;

    let cfg = Config {
        world: std::path::PathBuf::from("cubara-nonexistent-untrusted-fixture"),
        autosave_ticks: 0,
        ..Config::default()
    };
    let mut session = Session::open(&cfg);
    let addr = session
        .listen("127.0.0.1:0")
        .expect("bind an ephemeral port");

    let mut link = connect(addr.to_string()).expect("connect to the session");
    link.send(ClientMessage::Hello);

    // Wait for the accept rather than assuming one tick is enough. The
    // acceptor is non-blocking and the connection crosses a real socket, so how
    // many ticks it takes is not something this test can know -- and asserting
    // after a fixed single tick made it fail roughly one run in three.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while session.client_count() == 0 && std::time::Instant::now() < deadline {
        session.advance(1, &cfg);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(session.client_count(), 1, "the client never joined");

    const SENT: usize = 50;
    for _ in 0..SENT {
        link.send(ClientMessage::Act(Action::Break));
    }

    // A generous number of ticks: the actions cross a real socket, so how many
    // ticks they take to arrive is not something this test should pretend to
    // know. What it asserts is the total, which does not depend on the split.
    for _ in 0..60 {
        session.advance(1, &cfg);
    }

    let dropped = session.dropped_actions();
    assert!(
        dropped > 0,
        "a client sent {SENT} actions and none were refused; the cap is not \
         enforced"
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
