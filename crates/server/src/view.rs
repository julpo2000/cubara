//! What one client can perceive, and what it has already been told.
//!
//! Block 2.11, designed in `docs/PHASE2_MULTIPLAYER.md` §3 and §5.
//!
//! # The problem this exists for
//!
//! Before this, `Server` had **one** journal that the single in-process client
//! drained, and `snapshot()` handed a joining client every edit and every block
//! entity in the world. With one local player that is correct and free. With
//! many it is the shape `docs/RESEARCH_MULTIPLAYER.md` §3.2 names as the single
//! largest determinant of whether the player-count target is reachable:
//! everyone is told about everyone, so bandwidth grows with the square of the
//! player count and the world stops being able to hold people long before any
//! CPU does.
//!
//! A [`ClientView`] is the fix, and it is a small idea: a client is sent only
//! what happens where it can perceive it. Bytes to one client then depend on
//! **how much is happening near that client**, which is bounded, rather than on
//! how many other people exist, which is not.
//!
//! # Two radii, not one
//!
//! The issue for this block (#200) said to reuse `SIM_RADIUS_CHUNKS` and argued
//! that a second radius is "a bug with a delay fuse". **That was wrong, and the
//! existing tests caught it**: the client renders to a horizon 64 chunks away,
//! so an edit thirty chunks off has to reach it or the hole someone dug is not
//! drawn — while nothing out there is simulated, and nothing out there should
//! be. What a client can *see* and what the world *ticks* are different
//! questions with different answers, which is why every game in this genre has
//! both numbers.
//!
//! So [`VIEW_RADIUS`] is the replication radius and it is the render distance;
//! `SIM_RADIUS_CHUNKS` stays what ticks. They are allowed to disagree, and the
//! thing that would actually be a bug — replicating *less* than is simulated,
//! so a client is simulated at without being told — is asserted against by
//! `the_view_covers_everything_that_simulates`.
//!
//! # Why membership is arithmetic, and backfill walks the edits
//!
//! At radius 64 the set of chunks in view is about 83,000 of them. Materialising
//! that as a `BTreeSet`, and rebuilding it every time a player steps over a
//! chunk boundary, would cost more than everything else this file does put
//! together.
//!
//! So nothing is materialised. Membership is four comparisons against the
//! centre, and *backfill* — working out what has come into view — iterates the
//! world's **edits**, which are sparse, instead of the chunks, which are not.
//! That is the right way round: a world is mostly untouched, and the untouched
//! part is exactly the part the client can generate for itself.
//!
//! # Why terrain is never in here
//!
//! §3's load-bearing rule, and the reason the arithmetic works at all:
//!
//! > The server never sends terrain. It sends the seed once, and edits
//! > thereafter.
//!
//! `WorldGen::density` is a pure function of `(seed, x, y, z)`, and generation
//! being bit-identical across platforms is proven on every merged PR by the
//! fixture-hash test running on both CI runners. So a client generates the world
//! itself and applies an edit overlay — which is exactly what `World` already
//! is, and exactly what the client replica already does. What crosses the seam
//! scales with *how much players have changed the world*, not with how much
//! world they can see.
//!
//! # What this is not
//!
//! Not a transport. Nothing here opens a socket or encodes a packet; effects go
//! into a per-client queue drained by whoever drives that client, which today is
//! a function call in the same process. That is deliberate (§5): everything that
//! can be built and tested before the transport should be, because a headless
//! tick loop is testable to the tick where a networked one is testable to the
//! flake.

use cubara_voxel::ChunkCoord;

use crate::Effect;

/// The replication radius, in chunks: how far a client is told about changes.
///
/// **The render distance**, because that is what decides whether a client can
/// see the change. Phase 1's gate is stated at radius 64 and the renderer's far
/// plane covers its diagonal, so this is that number and moves with it.
///
/// Deliberately *not* `SIM_RADIUS_CHUNKS` — see the module docs. It must never
/// be smaller than it, which `the_view_covers_everything_that_simulates` checks.
pub const VIEW_RADIUS: i32 = 64;

/// The vertical half-height of the replication box, in chunks.
///
/// The world has no height limit since #175, so this is a band rather than a
/// range: generous enough to cover what a player can see above and below them,
/// and finite so that a client standing at the bottom of a cave is not told
/// about the sky ten thousand chunks up.
pub const VIEW_VERTICAL: i32 = 8;

/// One client's window onto the world.
///
/// Deliberately tiny: a centre, a radius, and a queue. Everything else is
/// derived, because the alternative — remembering which chunks it has been told
/// about — is a set with tens of thousands of entries per client, and there is
/// one of these per player.
#[derive(Debug, Clone)]
pub struct ClientView {
    /// The chunk this view is centred on. `None` until the first update, so a
    /// client that has not moved yet still gets its first backfill.
    centre: Option<ChunkCoord>,
    radius: i32,
    vertical: i32,
    /// Effects owed to this client, in the order they happened.
    pending: Vec<Effect>,
}

impl Default for ClientView {
    fn default() -> Self {
        Self {
            centre: None,
            radius: VIEW_RADIUS,
            vertical: VIEW_VERTICAL,
            pending: Vec::new(),
        }
    }
}

impl ClientView {
    pub fn new() -> Self {
        Self::default()
    }

    /// A view with a smaller window than the default.
    ///
    /// Clamped to [`VIEW_RADIUS`], because this will eventually be a number a
    /// client asks for, and §3.4's rule is that a client may never be believed:
    /// a client that could name its own radius could ask to be told about the
    /// whole world.
    pub fn with_radius(radius: i32, vertical: i32) -> Self {
        Self {
            radius: radius.clamp(0, VIEW_RADIUS),
            vertical: vertical.clamp(0, VIEW_VERTICAL),
            ..Self::default()
        }
    }

    pub fn centre(&self) -> Option<ChunkCoord> {
        self.centre
    }

    pub fn radius(&self) -> i32 {
        self.radius
    }

    /// Whether `chunk` is inside this view, as four comparisons rather than a
    /// set lookup. `false` before the first [`recentre`](Self::recentre): a view
    /// with no centre perceives nothing, which is the honest answer for a client
    /// whose player has not been placed yet.
    pub fn contains(&self, chunk: ChunkCoord) -> bool {
        let Some(c) = self.centre else {
            return false;
        };
        (chunk.x - c.x).abs() <= self.radius
            && (chunk.z - c.z).abs() <= self.radius
            && (chunk.y - c.y).abs() <= self.vertical
    }

    /// Whether this client can perceive the block at `pos`.
    pub fn perceives(&self, pos: [i32; 3]) -> bool {
        self.contains(ChunkCoord::from_block(pos[0], pos[1], pos[2]))
    }

    /// Whether `pos` is inside this view but was outside it when the view was
    /// centred on `previous` — that is, whether it has just come into sight.
    pub fn newly_visible(&self, previous: Option<ChunkCoord>, pos: [i32; 3]) -> bool {
        if !self.perceives(pos) {
            return false;
        }
        let before = Self {
            centre: previous,
            radius: self.radius,
            vertical: self.vertical,
            pending: Vec::new(),
        };
        !before.perceives(pos)
    }

    /// Queue an effect. Callers route; this does not decide.
    pub fn push(&mut self, effect: Effect) {
        self.pending.push(effect);
    }

    /// Take everything owed. Drained rather than read, for the reason the single
    /// journal was: a client that could *look* without taking could apply the
    /// same change twice, or miss one it had not applied yet.
    pub fn drain(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.pending)
    }

    /// How many effects are waiting. For tests, and for the scaling measurement.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Move the view to `centre`, reporting where it was so the caller can work
    /// out what has just come into sight.
    ///
    /// `None` when the centre has not moved, which is the common case — a player
    /// standing still changes nothing, and the caller can skip the whole
    /// backfill scan on that answer. The same shape `tick_furnaces` already uses
    /// to skip `update_simulation_radius`.
    pub fn recentre(&mut self, centre: ChunkCoord) -> Option<Option<ChunkCoord>> {
        if self.centre == Some(centre) {
            return None;
        }
        let previous = self.centre;
        self.centre = Some(centre);
        Some(previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(x, y, z)
    }

    /// The one relationship between the two radii that would actually be a bug.
    ///
    /// Replicating *more* than is simulated is fine and expected — that is the
    /// whole point of the render distance being larger. Replicating *less* would
    /// mean a client is simulated at without being told, which is a desync with
    /// a delay on it.
    #[test]
    fn the_view_covers_everything_that_simulates() {
        assert!(
            VIEW_RADIUS >= crate::SIM_RADIUS_CHUNKS,
            "the replication radius must not be smaller than the simulation radius"
        );
        assert!(
            VIEW_VERTICAL >= crate::SIM_HASH_VERTICAL_CHUNKS,
            "the replication band must not be shorter than the simulated band"
        );
    }

    #[test]
    fn a_view_with_no_centre_perceives_nothing() {
        let v = ClientView::new();
        assert!(!v.perceives([0, 0, 0]));
        assert!(!v.contains(at(0, 0, 0)));
    }

    #[test]
    fn standing_still_reports_no_move() {
        let mut v = ClientView::new();
        assert!(
            v.recentre(at(3, 0, 3)).is_some(),
            "the first centre is a move"
        );
        assert!(
            v.recentre(at(3, 0, 3)).is_none(),
            "a player who did not change chunk must not trigger a backfill scan"
        );
    }

    #[test]
    fn membership_is_the_box_around_the_centre() {
        let mut v = ClientView::with_radius(4, 2);
        v.recentre(at(0, 0, 0));
        assert!(v.contains(at(4, 2, -4)), "the far corner is inside");
        assert!(!v.contains(at(5, 0, 0)), "one past the radius is outside");
        assert!(!v.contains(at(0, 3, 0)), "one past the band is outside");
    }

    #[test]
    fn newly_visible_is_what_the_step_gained_and_nothing_else() {
        let mut v = ClientView::with_radius(4, 2);
        v.recentre(at(0, 0, 0));
        let previous = v.recentre(at(1, 0, 0)).expect("moved");

        // A chunk on the leading edge is new; one still behind us is not.
        let gained = ChunkCoord::new(5, 0, 0).world_offset();
        let kept = ChunkCoord::new(0, 0, 0).world_offset();
        let block = |o: [f32; 3]| [o[0] as i32, o[1] as i32, o[2] as i32];

        assert!(v.newly_visible(previous, block(gained)), "the leading edge");
        assert!(
            !v.newly_visible(previous, block(kept)),
            "a chunk that was already in view must not be re-sent"
        );
    }

    #[test]
    fn a_requested_radius_cannot_exceed_the_servers() {
        let v = ClientView::with_radius(VIEW_RADIUS * 10, VIEW_VERTICAL * 10);
        assert_eq!(
            v.radius(),
            VIEW_RADIUS,
            "a client may not name its own reach"
        );
    }
}
