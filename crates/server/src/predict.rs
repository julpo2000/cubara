//! What a client believes about itself, and how it stops being wrong.
//!
//! Block 2.13, designed in `docs/PHASE2_MULTIPLAYER.md` §5 and
//! `docs/RESEARCH_MULTIPLAYER.md` §8.4.
//!
//! # The problem, in one number
//!
//! Over a real socket between two machines on a LAN, the round trip measured
//! ~72 ms. Without prediction that is what a keypress costs: press a key, the
//! input crosses to the server, the server steps, the correction comes back,
//! and only then does the player move. Four ticks of nothing, every time.
//!
//! Prediction removes the wait by acting on the input immediately and being
//! corrected afterwards. §3.4's rule is what keeps that honest:
//!
//! > A client may simulate anything it can derive from data it already has, and
//! > may never be **believed** about any of it. Prediction is for latency, never
//! > for truth.
//!
//! So this type never decides anything. It guesses early, and every guess is
//! overwritten by the server's answer the moment it arrives.
//!
//! # Why this runs `Sim::tick` rather than its own physics
//!
//! [`Prediction`] holds a whole [`Sim`] containing exactly one player -- its
//! own -- and steps it with `Sim::tick` against the client's replica `World`.
//! Walking, jumping, collision, fall damage, regeneration and the target
//! raycast therefore have **one** implementation (Rule 5), and prediction is
//! that implementation run early rather than a second copy of it run
//! differently.
//!
//! The alternative is the standard way this goes wrong. A hand-written "client
//! physics" drifts from the server by a fraction each tick, and a predictor
//! that drifts is a *permanent* correction: the player is yanked backwards
//! every tick forever, which is worse than not predicting at all. Sharing the
//! tick makes drift impossible rather than unlikely.
//!
//! # Why the sequence number is not a tick number
//!
//! The client counts its own inputs from zero. The server echoes back the last
//! one it applied, and the gap between that and what the client has sent is
//! exactly the set of inputs to replay.
//!
//! It is deliberately not the tick counter. The LAN test made the reason
//! concrete: a client joining a running world was handed tick 8,484, because the
//! world had been up for two and a half minutes. Neither side can count from
//! zero, and a client that assumed it could would replay from the wrong place on
//! its first correction. A sequence number *can* count from zero, because it is
//! about the conversation rather than about the world.
//!
//! # What this deliberately does not predict
//!
//! Block edits. Breaking and placing go out as [`Action`](crate::Action)s and
//! come back as [`Effect::Edit`](crate::Effect::Edit)s, and the server raycasts.
//! A client that drew a block gone and then had it come back would be showing a
//! lie it did not have to tell -- and unlike a mispredicted position, which
//! nobody sees corrected, a block flickering back into existence is the most
//! visible failure this system can produce.
//!
//! What prediction buys for editing is subtler and matters more: the break is
//! decided from the pose the player *is* in rather than the pose they were in
//! one round trip ago. At 72 ms that is the difference between hitting the block
//! under the crosshair and hitting the one they were looking at four ticks back.

use std::collections::VecDeque;

use cubara_sim::{InputFrame, Player, PlayerId, PlayerInputs, PlayerState, Sim};
use cubara_world::{TerrainBlocks, World};

/// How many unacknowledged inputs are held before the client gives up guessing.
///
/// Two seconds at 60 Hz, which is far beyond any round trip this protocol is
/// for -- the LAN measurement was 72 ms, so this is ~28 round trips of slack.
///
/// It exists because the alternative is an unbounded buffer, and an unbounded
/// buffer is an out-of-memory bug that appears only on a bad connection, which
/// is exactly the situation nobody can debug. A number that is never reached in
/// normal play and always reached in a stall is the right shape for a bound.
pub const MAX_PENDING: usize = 120;

/// One client's belief about its own player, and the machinery that corrects it.
///
/// Not a replica of the world -- that is `World` itself, and the client already
/// has one. This is a replica of *the client's own player*, which is the one
/// piece of world state a client is allowed to run ahead of the server on.
#[derive(Debug)]
pub struct Prediction {
    /// The client's own simulation, holding exactly one player.
    ///
    /// A whole `Sim` rather than a bare `Player` so that stepping it is
    /// `Sim::tick` -- see the module docs. Its `tick` counter counts *steps
    /// taken*, replays included, and therefore means nothing; nothing reads it
    /// and nothing should.
    sim: Sim,
    /// Inputs sent and not yet acknowledged, oldest first.
    pending: VecDeque<(u64, InputFrame)>,
    /// The next sequence number [`predict`](Self::predict) will hand out.
    next_seq: u64,
    /// The highest sequence number the server has acknowledged.
    acked: u64,
    /// Whether prediction has given up until the next correction (see
    /// [`MAX_PENDING`]).
    stalled: bool,
}

/// The player a fresh `Sim` puts its one player under. An internal detail: the
/// id this client answers to *on the server* is a different number, assigned by
/// the server, and the two must not be confused -- which is why this is a
/// private constant rather than a field anyone could pass the wrong value to.
const ME: PlayerId = PlayerId::LOCAL;

impl Prediction {
    /// Start predicting from a known player.
    ///
    /// `seed` only feeds the private `Sim`'s RNG, which the player step never
    /// draws from; it is passed for tidiness rather than for behaviour.
    pub fn new(seed: u64, player: Player) -> Self {
        Self {
            sim: Sim::new(seed, player),
            pending: VecDeque::new(),
            next_seq: 0,
            acked: 0,
            stalled: false,
        }
    }

    /// Where this client believes it is. Never authority: it may be wrong,
    /// briefly, between a prediction and the correction that overwrites it.
    pub fn player(&self) -> &Player {
        self.sim.player(ME)
    }

    /// Act on one tick's input immediately, and return the sequence number to
    /// send it under.
    ///
    /// The caller sends `ClientMessage::Input { seq, frame }` with the returned
    /// number. `None` means the client has stalled ([`MAX_PENDING`]) and is
    /// waiting for a correction rather than guessing further -- there is nothing
    /// to send, because sending it would only lengthen a queue nobody is
    /// draining.
    pub fn predict(
        &mut self,
        frame: InputFrame,
        world: &mut World,
        blocks: TerrainBlocks,
    ) -> Option<u64> {
        if self.stalled {
            return None;
        }
        if self.pending.len() >= MAX_PENDING {
            // Two seconds without an acknowledgement. Stop guessing rather than
            // drift arbitrarily far from a server that is not answering: the
            // next correction becomes truth, and the log it would have been
            // replayed against is worthless anyway.
            self.stalled = true;
            self.pending.clear();
            return None;
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back((seq, frame));
        self.step(&frame, world, blocks);
        Some(seq)
    }

    /// Take the server's correction: rewind to what it says, then replay
    /// everything it has not seen yet.
    ///
    /// `seq` is the last input from this client the server had applied when it
    /// produced `state`.
    ///
    /// Where the client was right -- which is the normal case -- the replay ends
    /// exactly where it already was and nothing visibly moves. That is what
    /// makes correction invisible, and it is what the convergence test asserts.
    pub fn reconcile(
        &mut self,
        seq: u64,
        state: PlayerState,
        world: &mut World,
        blocks: TerrainBlocks,
    ) {
        // A correction older than one already applied must never rewind the
        // player. TCP delivers in order today, so this cannot fire -- and the
        // check is here anyway, because it is what makes a UDP `Link` (§5.1) a
        // rewiring rather than a rewrite.
        //
        // `seq == self.acked` is *not* stale and must be applied: the server
        // sends a correction every tick whether or not new input arrived, so a
        // standing-still client sees the same `seq` repeatedly while gravity and
        // regeneration keep changing the state it names.
        if seq < self.acked && !self.stalled {
            return;
        }
        self.acked = seq;
        self.stalled = false;
        while self.pending.front().is_some_and(|&(s, _)| s <= seq) {
            self.pending.pop_front();
        }

        self.sim.player_mut(ME).restore(state);
        // Replayed against the replica **as it is now**, not as it was. The
        // client has been given the server's edits too, so the current world is
        // the closest thing it has to what the server stepped through -- and
        // replaying against a remembered older one would converge on the
        // client's own past instead of on the server.
        let replay: Vec<InputFrame> = self.pending.iter().map(|&(_, f)| f).collect();
        for frame in replay {
            self.step(&frame, world, blocks);
        }
    }

    /// How many inputs are waiting to be acknowledged.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether prediction has given up until the next correction.
    pub fn is_stalled(&self) -> bool {
        self.stalled
    }

    /// The last sequence number the server has acknowledged.
    pub fn acked(&self) -> u64 {
        self.acked
    }

    /// One step of the one player, through the same tick the server runs.
    fn step(&mut self, frame: &InputFrame, world: &mut World, blocks: TerrainBlocks) {
        self.sim.tick(world, &PlayerInputs::one(ME, *frame), blocks);
    }
}
