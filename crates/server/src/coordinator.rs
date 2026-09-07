//! Who is holding which shard, what they last agreed it looked like, and
//! whether they can be believed about it.
//!
//! Block 2.17, designed in `docs/PHASE2_MULTIPLAYER.md` §7. The owner's ask:
//!
//! > *"als 2 pcs een wereld runnen hoeven ze niet allebei alles te doen. ze
//! > kunnen ook taken verspreiden zodat elke speler zijn eigen gebied actief
//! > kan houden."*
//!
//! # What this is, and what it is not
//!
//! §7.4 names three genuinely hard parts. This module does **two** of them and
//! says so rather than implying three:
//!
//! 1. **Handoff at boundaries** — a player walking from region A to region B,
//!    an entity crossing a seam, a furnace exactly on it. **Not here.** It needs
//!    a rule for who owns a boundary chunk and what happens to an entity
//!    mid-crossing, and inventing one before there is a second machine actually
//!    running shards would be inventing a mechanic. Named, deferred, and the
//!    reason this module holds *disjoint* shards only — [`claim`](Coordinator::claim)
//!    refuses an overlap rather than picking a winner.
//! 2. **Reclaim on failure** — done. A shard's state is checkpointed to the
//!    coordinator, so a peer that crashes costs the work since its last
//!    checkpoint rather than the region.
//! 3. **Nondeterminism has nowhere to hide** — done, and it is the interesting
//!    one. §7.2's claim is that a shard's output is a pure function of its
//!    inputs, so *any* machine can replay a peer's slice and compare. [`audit`](Coordinator::audit)
//!    is that.
//!
//! # Why the audit changes the question
//!
//! §3.4 says a client may never be believed, and a peer authoritative over a
//! region is a client being believed about a great deal. Most games cannot let
//! a player's machine simulate anything for exactly this reason.
//!
//! Here the answer is not "trust them" but "check them, cheaply, afterwards".
//! That does not make cheating impossible; it makes it **detectable**, which
//! turns "can a peer be trusted" into "how often is a peer audited" — a dial
//! rather than an architecture (§7.3).
//!
//! The audit is only as good as Rule 1. If two machines could disagree about a
//! tick for any reason at all, every audit would be a false positive and the
//! whole mechanism would have to be switched off. That is why this is the last
//! block rather than the first.

use std::collections::{BTreeMap, BTreeSet};

use cubara_sim::PlayerInputs;
use cubara_voxel::ChunkCoord;

use crate::shard::Shard;
use crate::Server;

/// Which peer is which. Assigned by the coordinator, never claimed by the peer
/// — the same rule `PlayerId` follows, and for the same reason (§3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerId(pub u64);

/// Why a claim was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Some of the chunks are already held by another peer.
    ///
    /// Refused rather than resolved: deciding who wins an overlap is boundary
    /// handoff (§7.4 item 1), and this module does not do that. A coordinator
    /// that silently reassigned would be inventing the rule.
    Overlaps { held_by: PeerId },
}

/// What a peer reported back, and whether it survives being checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audit {
    /// The replay reached exactly what the peer reported.
    Agrees,
    /// It did not. The shard is not to be trusted, and the checkpoint is still
    /// the last state anybody agreed on.
    Disagrees,
    /// Nothing to check against — no checkpoint for this shard, so there is no
    /// starting state to replay from. Deliberately not `Agrees`: "I could not
    /// check" and "I checked and it was fine" are different answers, and
    /// collapsing them is how an audit quietly stops auditing.
    Unknown,
}

/// Who holds what, and what they last agreed it looked like.
///
/// Owns no `World` of its own — Rule 8. A coordinator that held "the" world
/// would be the single owner this whole design exists to avoid.
#[derive(Debug, Default)]
pub struct Coordinator {
    holders: BTreeMap<PeerId, Vec<ChunkCoord>>,
    /// The last state anybody agreed on, per peer.
    ///
    /// This is what makes a peer's disappearance survivable (§7.4 item 2).
    /// Without it a shard lives only in the RAM of the machine holding it, and
    /// that machine quitting eats part of the world.
    checkpoints: BTreeMap<PeerId, Shard>,
}

impl Coordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give `peer` responsibility for `chunks`.
    ///
    /// Refuses an overlap rather than resolving one; see [`ClaimError`].
    pub fn claim(&mut self, peer: PeerId, chunks: &[ChunkCoord]) -> Result<(), ClaimError> {
        let want: BTreeSet<ChunkCoord> = chunks.iter().copied().collect();
        for (&other, held) in &self.holders {
            if other == peer {
                continue;
            }
            if held.iter().any(|c| want.contains(c)) {
                return Err(ClaimError::Overlaps { held_by: other });
            }
        }
        let mut chunks: Vec<ChunkCoord> = want.into_iter().collect();
        chunks.sort();
        self.holders.insert(peer, chunks);
        Ok(())
    }

    /// What `peer` is responsible for.
    pub fn held_by(&self, peer: PeerId) -> Option<&[ChunkCoord]> {
        self.holders.get(&peer).map(|v| v.as_slice())
    }

    /// Which peer holds `chunk`, if any.
    pub fn holder_of(&self, chunk: ChunkCoord) -> Option<PeerId> {
        self.holders
            .iter()
            .find(|(_, held)| held.contains(&chunk))
            .map(|(&peer, _)| peer)
    }

    /// Record the state everybody agrees on for `peer`'s shard.
    ///
    /// Taken *before* handing the shard out, and again after each audit that
    /// agrees. A checkpoint that was never checked would be a place to launder
    /// a bad shard into the record.
    pub fn checkpoint(&mut self, peer: PeerId, shard: Shard) {
        self.checkpoints.insert(peer, shard);
    }

    /// The last agreed state for `peer`'s shard.
    pub fn checkpoint_of(&self, peer: PeerId) -> Option<&Shard> {
        self.checkpoints.get(&peer)
    }

    /// Check a peer's reported result by replaying its slice (§7.2).
    ///
    /// `replay_on` is any server with the same seed and assets — the
    /// coordinator's own, another peer's, a CI runner's. It is left to the
    /// caller rather than owned here because a `Coordinator` owning a `World`
    /// would be exactly the single-owner assumption Rule 8 forbids.
    ///
    /// `ticks` and `inputs` are what the peer was told to apply. The replay runs
    /// them against the checkpoint and compares what comes out with what came
    /// back.
    pub fn audit(
        &self,
        peer: PeerId,
        reported: &Shard,
        replay_on: &mut Server,
        ticks: u64,
        inputs: &PlayerInputs,
    ) -> Audit {
        let Some(from) = self.checkpoints.get(&peer) else {
            return Audit::Unknown;
        };
        replay_on.install_shard(from);
        replay_on.assign(from.chunks.iter().copied());
        for _ in 0..ticks {
            replay_on.tick_sim_all(inputs);
            replay_on.tick_mining_all(inputs);
            replay_on.tick_world();
        }
        let expected = replay_on.extract_shard(&from.chunks);

        // The tick counter is each server's own -- a shard is a region's state,
        // not a world's -- so the comparison is of what is *in* the region.
        let same = expected.blocks == reported.blocks
            && expected.block_entities == reported.block_entities
            && expected.entities == reported.entities;
        if same {
            Audit::Agrees
        } else {
            Audit::Disagrees
        }
    }

    /// A peer has gone. Hand back what it was holding and the last state
    /// anybody agreed on.
    ///
    /// The checkpoint comes back with the assignment because the two are only
    /// useful together: knowing which chunks are unowned without knowing what
    /// was in them is knowing that part of the world is missing.
    pub fn peer_lost(&mut self, peer: PeerId) -> Option<(Vec<ChunkCoord>, Option<Shard>)> {
        let chunks = self.holders.remove(&peer)?;
        let last = self.checkpoints.remove(&peer);
        Some((chunks, last))
    }

    /// How many peers are holding something.
    pub fn peer_count(&self) -> usize {
        self.holders.len()
    }
}
