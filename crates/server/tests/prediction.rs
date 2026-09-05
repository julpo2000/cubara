//! Block 2.13 — prediction and reconciliation, tested against real latency.
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §8.4 is the reason this block exists here and
//! not earlier:
//!
//! > Building them before there is latency means building them against a round
//! > trip that is always zero, which cannot show whether they work.
//!
//! The latency in this file is **simulated, in-process, and exact**: the harness
//! below holds every message for a fixed number of ticks before delivering it.
//! That is deliberate, and it is not a weaker substitute for testing over a real
//! socket. A test that drives two machines is testable to the flake; this one is
//! testable to the tick, and `CLAUDE.md` is explicit that what counts as
//! verification here is an automated check rather than something someone watched
//! once. The real socket has its own test (`two_processes.rs`), and it is a
//! different question: *does the wire work*, not *does the compensation work*.
//!
//! The number being compensated for is real, though. Measured between two
//! machines on a LAN while block 2.12b was being written: ~72 ms of TCP round
//! trip, which is between four and five ticks at 60 Hz. `LATENCY` below is four.

use cubara_server::predict::{Prediction, MAX_PENDING};
use cubara_server::{Effect, Server};
use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs, PlayerState};
use cubara_voxel::{Angle, BlockId, FixedVec3};
use cubara_world::World;

/// One-way delay, in ticks. Four each way is ~133 ms round trip at 60 Hz —
/// slightly worse than the LAN measurement, which is the right side to err on.
const LATENCY: u64 = 4;

/// Walking forward, held.
fn forward() -> InputFrame {
    InputFrame {
        move_axes: [0.0, 0.0, 1.0],
        ..InputFrame::default()
    }
}

/// A client, a server, and a wire between them that takes time.
///
/// Both directions are queues of `(deliver_at_tick, payload)`. Nothing is
/// handed over early, and nothing is handed over out of order — this models a
/// reliable ordered link, which is what `net::Link` is today.
struct Harness {
    server: Server,
    /// The id this client answers to *on the server*. Deliberately not
    /// `PlayerId::LOCAL`: the server's own local player is 0, and a test that
    /// used it would be testing the one case a real client never is in.
    me: PlayerId,
    /// The client's replica world, generated from the seed like a real one.
    world: World,
    client: Prediction,
    now: u64,
    up: Vec<(u64, u64, InputFrame)>,
    down: Vec<(u64, Vec<Effect>)>,
    /// The last sequence number the server has applied — what `Session` tracks
    /// per connection, done here by hand because this test is not a `Session`.
    last_seq: u64,
    /// What the client believed immediately after predicting each sequence
    /// number, kept so it can be checked against what the server later says
    /// about that same input.
    ///
    /// This is the recording that makes the central claim testable. Comparing
    /// the client's state *before and after* a correction is not enough, and
    /// the difference is the whole point: a client that is restored every tick
    /// and then steps once is never visibly yanked, yet it renders a full round
    /// trip in the past. That is precisely the defect prediction exists to
    /// prevent, and a before/after check cannot see it. Only asking "was the
    /// guess for input N right?" can.
    predicted: std::collections::HashMap<u64, PlayerState>,
    /// Corrections that disagreed with what the client had predicted for that
    /// same input.
    mispredictions: u32,
    /// Corrections checked against a recorded prediction, so a test cannot pass
    /// by never having compared anything.
    checked: u32,
}

impl Harness {
    fn new() -> Self {
        let mut server = Server::new();
        server.open(std::path::Path::new(
            "cubara-nonexistent-prediction-fixture",
        ));
        server.place_player_on_ground();

        // A second player, six blocks along, so this client is never the
        // server's own local player.
        let ground = server.sim.player(server.local).pos;
        let me = server.sim.join(Player::new(
            FixedVec3::from_blocks(
                ground.x.floor_block() + 6,
                ground.y.floor_block(),
                ground.z.floor_block(),
            ),
            Angle::ZERO,
            Angle::ZERO,
        ));
        server.open_view(me);

        let seed = server.world.seed();
        // The client starts from the state the handshake would have carried.
        let client = Prediction::new(seed, *server.sim.player(me));
        Self {
            server,
            me,
            world: World::with_seed(seed),
            client,
            now: 0,
            up: Vec::new(),
            down: Vec::new(),
            last_seq: 0,
            predicted: std::collections::HashMap::new(),
            mispredictions: 0,
            checked: 0,
        }
    }

    /// One tick of everything: the client acts, the wire delivers what is due,
    /// the server steps, and the client is corrected by whatever has arrived.
    fn tick(&mut self, frame: InputFrame) {
        let blocks = self.server.terrain();

        // 1. The client acts on its own input immediately.
        if let Some(seq) = self.client.predict(frame, &mut self.world, blocks) {
            self.predicted.insert(seq, self.client.player().state());
            self.up.push((self.now + LATENCY, seq, frame));
        }

        // 2. Whatever has reached the server by now.
        let mut inputs = PlayerInputs::default();
        let due: Vec<(u64, InputFrame)> = self
            .up
            .iter()
            .filter(|&&(at, _, _)| at <= self.now)
            .map(|&(_, seq, f)| (seq, f))
            .collect();
        self.up.retain(|&(at, _, _)| at > self.now);
        for (seq, f) in due {
            inputs.set(self.me, f);
            self.last_seq = self.last_seq.max(seq);
        }

        // 3. The server steps, then says what it thinks.
        self.server.tick_sim_all(&inputs);
        self.server.tick_world();
        self.server.publish_self_state(self.me, self.last_seq);
        let owed = self.server.drain_effects_for(self.me);
        if !owed.is_empty() {
            self.down.push((self.now + LATENCY, owed));
        }

        // 4. Whatever has reached the client by now.
        let arrived: Vec<Vec<Effect>> = self
            .down
            .iter()
            .filter(|&&(at, _)| at <= self.now)
            .map(|(_, fx)| fx.clone())
            .collect();
        self.down.retain(|&(at, _)| at > self.now);
        for fx in arrived {
            self.deliver(fx, blocks);
        }

        self.now += 1;
    }

    /// Apply one batch exactly as a client would: edits onto the replica first,
    /// then the correction — so a replay runs against a world that already has
    /// the changes the same batch brought.
    fn deliver(&mut self, effects: Vec<Effect>, blocks: cubara_world::TerrainBlocks) {
        let mut correction: Option<(u64, PlayerState)> = None;
        for e in effects {
            match e {
                Effect::Edit { pos, block } => {
                    self.world.set_block(pos[0], pos[1], pos[2], block);
                }
                Effect::SelfState { seq, state } => correction = Some((seq, state)),
                _ => {}
            }
        }
        if let Some((seq, state)) = correction {
            // The claim, checked at the only moment it can be: the server has
            // now applied input `seq`, and the client predicted a state for
            // that same input some ticks ago. If prediction works, they agree.
            if let Some(&guess) = self.predicted.get(&seq) {
                self.checked += 1;
                if guess != state {
                    self.mispredictions += 1;
                }
            }
            self.client.reconcile(seq, state, &mut self.world, blocks);
        }
    }

    /// Run until the wire is empty and both sides have stopped changing, so the
    /// two states can be compared without one of them being mid-flight.
    fn settle(&mut self) {
        for _ in 0..(LATENCY * 4 + 8) {
            self.tick(InputFrame::default());
        }
    }

    fn believed(&self) -> PlayerState {
        self.client.player().state()
    }

    fn authoritative(&self) -> PlayerState {
        self.server.sim.player(self.me).state()
    }
}

// ---------------------------------------------------------------------------
// What prediction buys
// ---------------------------------------------------------------------------

/// The whole product of the block, measured as a tick count.
///
/// With a round trip of `2 * LATENCY`, a client that did not predict could not
/// move before tick `2 * LATENCY` — the server has not even *seen* the input
/// until `LATENCY`. A predicting client moves on the tick the key went down.
#[test]
fn the_player_moves_on_the_tick_the_key_was_pressed() {
    let mut h = Harness::new();
    let start = h.believed().pos;

    h.tick(forward());

    // Asserted on `z` alone, which is the axis `forward` drives at yaw zero.
    // Not on the whole position: gravity moves `y` on both sides every tick
    // whether or not anyone pressed anything, so comparing positions would
    // conflate "the input arrived" with "time passed" and the test would pass
    // for the wrong reason.
    assert_ne!(
        h.believed().pos.z,
        start.z,
        "the client did not move on the tick it pressed forward; \
         prediction is not doing anything"
    );
    assert_eq!(
        h.authoritative().pos.z,
        start.z,
        "the server moved along the input axis before the input could possibly \
         have reached it — the harness is not actually delaying anything, so \
         this test proves nothing"
    );
}

/// Prediction is a guess, and the guess has to be right.
///
/// The client walks for a while under latency, then everything drains. If
/// prediction and reconciliation agree with the server, the two states are
/// **identical** at the end — not close, identical, because both sides ran the
/// same integer tick over the same world.
#[test]
fn prediction_converges_on_what_the_server_later_says() {
    let mut h = Harness::new();
    for _ in 0..90 {
        h.tick(forward());
    }
    h.settle();

    assert_eq!(
        h.believed(),
        h.authoritative(),
        "the client and the server disagree about where the client is after \
         everything has been delivered"
    );
    assert!(
        h.believed().pos != FixedVec3::ZERO,
        "the player never moved, so this proved nothing"
    );
}

/// **The central claim**: the guess is right.
///
/// For every input the client predicted, the server later reports the state it
/// reached applying that same input — and the two must be identical, because
/// both ran the same integer tick over the same world.
///
/// Asserted this way rather than by watching for a visible jump at correction
/// time, and the difference matters more than it looks. Deleting the replay
/// from `reconcile` — the heart of this block — leaves a client that is
/// restored every tick and steps once, which never jumps and is always a full
/// round trip behind. A before/after check calls that correct. This one does
/// not, and it was written after the before/after version failed to notice the
/// replay had been removed.
#[test]
fn every_prediction_matches_what_the_server_later_reports() {
    let mut h = Harness::new();
    // Prime, and the length is not arbitrary. Two things have to finish before
    // a prediction can be expected to be right, and the second is the longer:
    //
    // 1. The round trip has to fill. Until the client's first input has been
    //    all the way there and back, the server has been ticking this player
    //    with no input at all, so the client genuinely is wrong.
    // 2. The player has to come to rest on the terrain. This client spawns at
    //    another player's ground height six blocks away, so it starts by
    //    falling and stepping onto whatever is actually under it — and while
    //    that is resolving, the two sides are a step apart for real.
    //
    // Both are the system working. A client that joins mid-world is wrong until
    // it has been corrected, which is what correction is for. Measured: this
    // settles by tick ~24, and 40 leaves room without hiding a regression,
    // because a defect that survived settling would still be caught over the
    // 120 ticks below.
    for _ in 0..40 {
        h.tick(forward());
    }
    h.mispredictions = 0;
    h.checked = 0;

    for _ in 0..120 {
        h.tick(forward());
    }

    assert!(
        h.checked > 100,
        "only {} predictions were ever checked against the server; the test \
         is not comparing anything",
        h.checked
    );
    assert_eq!(
        h.mispredictions, 0,
        "{} of {} predictions disagreed with what the server later reported \
         for the same input",
        h.mispredictions, h.checked
    );
}

// ---------------------------------------------------------------------------
// When the client is wrong
// ---------------------------------------------------------------------------

/// Prediction is for latency, never for truth (§3.4).
///
/// The server drops a solid block into this client's path. For `LATENCY` ticks
/// the client does not know it is there and happily predicts through it; then
/// the edit and the correction arrive together and the server's answer wins.
#[test]
fn a_correction_wins_when_the_client_was_wrong() {
    let mut h = Harness::new();
    for _ in 0..(LATENCY * 2 + 2) {
        h.tick(forward());
    }

    // A wall, one block ahead of where the player is walking, at body height.
    let p = h.authoritative().pos;
    for dy in 0..3 {
        for dz in 1..4 {
            h.server.set_block(
                [
                    p.x.floor_block(),
                    p.y.floor_block() - 1 + dy,
                    p.z.floor_block() + dz,
                ],
                BlockId::STONE,
            );
        }
    }

    for _ in 0..60 {
        h.tick(forward());
    }
    h.settle();

    assert_eq!(
        h.believed(),
        h.authoritative(),
        "the client kept its own answer after the server disagreed — \
         a client is never to be believed about where it is"
    );
}

// ---------------------------------------------------------------------------
// The protocol's sharp edges
// ---------------------------------------------------------------------------

/// A stale or duplicated correction must never rewind the player.
///
/// TCP delivers in order, so this cannot happen today. It is checked because
/// the check is what makes a future UDP link a rewiring rather than a rewrite —
/// and because a rewind is invisible in a log and obvious on screen.
#[test]
fn a_stale_correction_is_ignored() {
    let mut server = Server::new();
    server.open(std::path::Path::new(
        "cubara-nonexistent-prediction-fixture",
    ));
    server.place_player_on_ground();
    let blocks = server.terrain();
    let seed = server.world.seed();
    let mut world = World::with_seed(seed);
    let start = *server.sim.player(server.local);
    let mut client = Prediction::new(seed, start);

    for _ in 0..10 {
        client.predict(forward(), &mut world, blocks);
    }

    let mut recent = start.state();
    recent.pos = FixedVec3::from_blocks(10, 40, 10);
    client.reconcile(5, recent, &mut world, blocks);
    let after_recent = client.player().state();

    let mut ancient = start.state();
    ancient.pos = FixedVec3::from_blocks(-99, 40, -99);
    client.reconcile(3, ancient, &mut world, blocks);

    assert_eq!(
        client.player().state(),
        after_recent,
        "a correction older than one already applied rewound the player"
    );
    assert_eq!(client.acked(), 5, "the stale correction moved the ack back");
}

/// A correction carrying the sequence number already acknowledged is **not**
/// stale, and dropping it would be a slow desync.
///
/// The server sends one every tick whether or not new input arrived, so a client
/// standing still sees the same `seq` over and over while gravity keeps changing
/// the state it names. This is the case a naive `seq <= acked` check breaks.
#[test]
fn a_repeat_of_the_same_sequence_still_applies() {
    let mut server = Server::new();
    server.open(std::path::Path::new(
        "cubara-nonexistent-prediction-fixture",
    ));
    server.place_player_on_ground();
    let blocks = server.terrain();
    let seed = server.world.seed();
    let mut world = World::with_seed(seed);
    let start = *server.sim.player(server.local);
    let mut client = Prediction::new(seed, start);

    client.predict(InputFrame::default(), &mut world, blocks);

    let mut first = start.state();
    first.pos = FixedVec3::from_blocks(5, 40, 5);
    client.reconcile(0, first, &mut world, blocks);

    let mut second = start.state();
    second.pos = FixedVec3::from_blocks(5, 30, 5);
    client.reconcile(0, second, &mut world, blocks);

    assert_eq!(
        client.player().state().pos.y,
        second.pos.y,
        "a second correction under the same sequence number was dropped; \
         a standing-still client would stop being corrected at all"
    );
}

/// An unanswered client stops guessing rather than growing without bound.
///
/// The failure this prevents only happens on a bad connection, which is exactly
/// when nobody is in a position to debug it.
#[test]
fn an_unacknowledged_client_stops_predicting_rather_than_growing() {
    let mut server = Server::new();
    server.open(std::path::Path::new(
        "cubara-nonexistent-prediction-fixture",
    ));
    server.place_player_on_ground();
    let blocks = server.terrain();
    let seed = server.world.seed();
    let mut world = World::with_seed(seed);
    let start = *server.sim.player(server.local);
    let mut client = Prediction::new(seed, start);

    for _ in 0..(MAX_PENDING * 2) {
        client.predict(forward(), &mut world, blocks);
        assert!(
            client.pending_len() <= MAX_PENDING,
            "the unacknowledged-input log grew past its bound"
        );
    }

    assert!(
        client.is_stalled(),
        "a client with no acknowledgement for {MAX_PENDING} ticks kept guessing"
    );
    assert_eq!(
        client.predict(forward(), &mut world, blocks),
        None,
        "a stalled client still handed out sequence numbers to send"
    );

    // The next correction is truth, and prediction resumes from it.
    let mut rescue = start.state();
    rescue.pos = FixedVec3::from_blocks(3, 40, 3);
    client.reconcile(500, rescue, &mut world, blocks);

    assert!(
        !client.is_stalled(),
        "the correction did not restart prediction"
    );
    assert_eq!(
        client.player().state().pos,
        rescue.pos,
        "the client did not accept the correction as truth after stalling"
    );
    assert!(
        client.predict(forward(), &mut world, blocks).is_some(),
        "prediction did not resume after the stall was cleared"
    );
}
