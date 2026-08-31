//! The chunk lifecycle, as a simulation concern
//! (`docs/PHASE2_ARCHITECTURE.md` §11).
//!
//! # This is not the rendering lifecycle
//!
//! Issue #47 originally asked for one enum covering
//! `Ungenerated → Generated → Meshed → Active ⇄ Dormant → Unloaded`. That is
//! **two lifecycles wearing one name** (§11.1):
//!
//! | | Unit | Owner |
//! |---|---|---|
//! | Rendering | [`NodeKey`](crate::node::NodeKey) | `NodeStreaming`, in the app |
//! | Simulation | [`ChunkCoord`](cubara_voxel::ChunkCoord) | [`World`](crate::World) |
//!
//! `Meshed` is deliberately absent here. A node above level 0 covers up to 512
//! chunks and exists because of its distance from a *camera*, which the
//! simulation must not care about (Rule 3). The tell that separating them is
//! right: as one enum, a chunk would go dormant because it was far from the
//! camera rather than from the player — and with a second player, or a camera
//! that is not the player, those diverge immediately.

use std::collections::BTreeMap;

use cubara_voxel::ChunkCoord;

/// Where a chunk is in its simulation lifecycle (§11.2).
///
/// [`Ungenerated`](Self::Ungenerated) is the absence of an entry rather than a
/// stored value: terrain is a pure function of `(seed, coord)` (§8.1), so a
/// chunk nothing has touched needs no record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChunkState {
    /// Terrain exists and edits apply, but nothing in it is ticking.
    #[default]
    Generated,
    /// Inside the simulation radius: its block entities tick every tick.
    Active,
    /// Outside the radius. Nothing ticks, and it remembers the tick it stopped
    /// so reactivation can make up the difference (§11.3).
    ///
    /// The tick it went dormant, **not** a countdown: a countdown would have to
    /// be decremented every tick, which is precisely the work dormancy exists
    /// to avoid.
    Dormant { since: u64 },
    /// Dropped from memory.
    ///
    /// **Nothing transitions into this yet.** Unloading loses a chunk's edits
    /// and block entities until block 2.8 persists them, and not unloading is
    /// better than unloading lossily (§11.2). The state is named so that 2.8
    /// has somewhere to land, and [`ChunkStates::set`] rejects the transition
    /// until then.
    Unloaded,
}

impl ChunkState {
    /// Whether `self -> next` is a legal transition (§11.2).
    ///
    /// ```text
    /// Generated ──> Active <──> Dormant ──> Unloaded
    /// ```
    ///
    /// Written as an explicit table rather than as scattered `if`s, because the
    /// whole point of the block is that the lifecycle is one readable thing
    /// instead of ad-hoc flags.
    pub fn can_transition_to(self, next: ChunkState) -> bool {
        use ChunkState::*;
        match (self, next) {
            // Re-asserting the state you are already in is a no-op, not an
            // error: the streamer recomputes desired states every tick and
            // should not have to diff them itself.
            (Generated, Generated) | (Active, Active) | (Unloaded, Unloaded) => true,
            (Dormant { .. }, Dormant { .. }) => true,

            (Generated, Active) => true,
            (Active, Dormant { .. }) => true,
            (Dormant { .. }, Active) => true,

            // A chunk that has never ticked can go straight to sleep -- it is
            // generated far away and simply never woke up.
            (Generated, Dormant { .. }) => true,

            // Unloading is 2.8's, and only from a state that is not mid-tick.
            (Generated, Unloaded) | (Dormant { .. }, Unloaded) => false,

            // Everything else: no. Notably Active -> Unloaded, which would drop
            // a chunk out from under a running tick.
            _ => false,
        }
    }
}

/// Every chunk that is not [`Ungenerated`](ChunkState::Ungenerated), by
/// position.
///
/// `BTreeMap` for the reason everything else in this crate is one: iteration
/// order reaches the world-state hash, and Rule 1 forbids results that depend on
/// unordered iteration.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChunkStates {
    states: BTreeMap<ChunkCoord, ChunkState>,
}

/// A chunk that just woke up, and how long it was asleep — what the caller needs
/// in order to catch its contents up (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Woken {
    pub coord: ChunkCoord,
    /// Ticks that passed while it was dormant. May be zero.
    pub elapsed: u64,
}

impl ChunkStates {
    pub fn new() -> Self {
        Self::default()
    }

    /// The state of `coord`. A chunk with no entry is
    /// [`Generated`](ChunkState::Generated) — the terrain is always available,
    /// it simply is not simulating.
    pub fn get(&self, coord: ChunkCoord) -> ChunkState {
        self.states
            .get(&coord)
            .copied()
            .unwrap_or(ChunkState::Generated)
    }

    /// Move `coord` to `next`, returning whether the transition was legal.
    ///
    /// An illegal transition is **rejected and reported**, not applied and not
    /// panicked on: a streamer bug should show up as a chunk that failed to
    /// wake, which is diagnosable, rather than as a crash in a release build or
    /// a silently corrupt lifecycle.
    pub fn set(&mut self, coord: ChunkCoord, next: ChunkState) -> bool {
        let current = self.get(coord);
        if !current.can_transition_to(next) {
            return false;
        }
        self.states.insert(coord, next);
        true
    }

    /// Put `coord` to sleep as of `now`. No-op if it is not Active.
    pub fn sleep(&mut self, coord: ChunkCoord, now: u64) -> bool {
        self.set(coord, ChunkState::Dormant { since: now })
    }

    /// Wake `coord` at `now`, reporting how long it was asleep so the caller can
    /// catch its contents up (§11.3).
    ///
    /// Returns `None` if it was not dormant — waking an already-Active chunk is
    /// a no-op with nothing to catch up, not an error.
    pub fn wake(&mut self, coord: ChunkCoord, now: u64) -> Option<Woken> {
        let elapsed = match self.get(coord) {
            ChunkState::Dormant { since } => now.saturating_sub(since),
            _ => {
                self.set(coord, ChunkState::Active);
                return None;
            }
        };
        if !self.set(coord, ChunkState::Active) {
            return None;
        }
        Some(Woken { coord, elapsed })
    }

    /// Every Active chunk, in position order.
    pub fn active(&self) -> impl Iterator<Item = ChunkCoord> + '_ {
        self.states
            .iter()
            .filter(|(_, s)| **s == ChunkState::Active)
            .map(|(c, _)| *c)
    }

    /// Every chunk with a recorded state, in position order. Feeds the world
    /// hash, so the order is load-bearing.
    pub fn iter(&self) -> impl Iterator<Item = (&ChunkCoord, &ChunkState)> {
        self.states.iter()
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(x, 0, z)
    }

    #[test]
    fn a_chunk_nothing_has_touched_is_generated() {
        let s = ChunkStates::new();
        assert_eq!(s.get(c(0, 0)), ChunkState::Generated);
        assert!(s.is_empty(), "and stores nothing for it");
    }

    #[test]
    fn the_legal_transitions_are_the_ones_in_the_diagram() {
        use ChunkState::*;
        let d = Dormant { since: 0 };
        // Legal.
        assert!(Generated.can_transition_to(Active));
        assert!(Generated.can_transition_to(d));
        assert!(Active.can_transition_to(d));
        assert!(d.can_transition_to(Active));
        // Illegal: nothing unloads yet (§11.2), and Active must never be
        // dropped out from under a running tick.
        assert!(!Active.can_transition_to(Unloaded));
        assert!(!Generated.can_transition_to(Unloaded));
        assert!(!d.can_transition_to(Unloaded));
        assert!(!Unloaded.can_transition_to(Active));
    }

    #[test]
    fn an_illegal_transition_is_rejected_rather_than_applied() {
        let mut s = ChunkStates::new();
        assert!(s.set(c(0, 0), ChunkState::Active));
        assert!(!s.set(c(0, 0), ChunkState::Unloaded), "rejected");
        assert_eq!(
            s.get(c(0, 0)),
            ChunkState::Active,
            "and the state did not move"
        );
    }

    #[test]
    fn waking_reports_how_long_the_chunk_slept() {
        let mut s = ChunkStates::new();
        s.set(c(1, 2), ChunkState::Active);
        s.sleep(c(1, 2), 100);
        assert_eq!(s.get(c(1, 2)), ChunkState::Dormant { since: 100 });

        let woken = s.wake(c(1, 2), 460).expect("it was dormant");
        assert_eq!(woken.elapsed, 360);
        assert_eq!(s.get(c(1, 2)), ChunkState::Active);
    }

    #[test]
    fn waking_an_active_chunk_has_nothing_to_catch_up() {
        let mut s = ChunkStates::new();
        s.set(c(0, 0), ChunkState::Active);
        assert_eq!(s.wake(c(0, 0), 500), None);
        assert_eq!(s.get(c(0, 0)), ChunkState::Active);
    }

    #[test]
    fn active_is_listed_in_position_order() {
        // The order feeds the world hash (Rule 1), so it is the positions' own
        // and not insertion order.
        let mut s = ChunkStates::new();
        for coord in [c(3, 1), c(-2, 0), c(0, 5)] {
            s.set(coord, ChunkState::Active);
        }
        let got: Vec<ChunkCoord> = s.active().collect();
        let mut want = got.clone();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn re_asserting_the_current_state_is_a_no_op_not_an_error() {
        // The streamer recomputes desired states every tick; making it diff
        // them itself would just move the bookkeeping.
        let mut s = ChunkStates::new();
        assert!(s.set(c(0, 0), ChunkState::Active));
        assert!(s.set(c(0, 0), ChunkState::Active));
        s.sleep(c(0, 0), 10);
        assert!(s.sleep(c(0, 0), 20), "still legal");
        assert_eq!(
            s.get(c(0, 0)),
            ChunkState::Dormant { since: 20 },
            "and it takes the newer timestamp"
        );
    }
}
