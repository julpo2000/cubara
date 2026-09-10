//! Where everybody else is, as far as this client has been told.
//!
//! The other half of block 2.13's split, and the half that was left until there
//! was a reason to draw it. `docs/RESEARCH_MULTIPLAYER.md` §3.4:
//!
//! > A client predicts itself and is corrected, and interpolates everyone else,
//! > because it does not know their inputs.
//!
//! That sentence is the whole design. Your own player can be *predicted*,
//! because you know what you pressed. Somebody else's cannot: the only honest
//! thing to do with their pose is to draw it a little in the past and move
//! smoothly between the updates that arrive, rather than guessing forward and
//! being wrong in a way that looks like teleporting.
//!
//! # Why interpolate rather than extrapolate
//!
//! Extrapolation — continuing their last velocity — makes a walking player look
//! perfect and a stopping player overshoot and snap back. Interpolation makes
//! everyone consistently one update late, which is a fixed, small, honest cost.
//! Games that need the first do it because a hit has to register on what the
//! shooter saw; nothing here shoots.
//!
//! # What arrives, and when
//!
//! `Effect::PlayerMoved` is sent **only when a pose has changed** — that is what
//! block 2.15's idle-player fix bought, and it means updates are not a metronome.
//! A player standing still simply stops producing them, and their two remembered
//! poses converge, so interpolating between them is a no-op rather than a drift.
//! Nothing here may assume one update per tick.

use std::collections::BTreeMap;

use cubara_sim::PlayerId;
use cubara_voxel::{Angle, FixedVec3};

/// Where a player is and which way they are facing.
///
/// Fixed-point and binary angles, like everything else that crosses the seam:
/// this is a copy of what the server sent, not a rendering value (§3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pose {
    pub pos: FixedVec3,
    pub yaw: Angle,
    pub pitch: Angle,
}

/// The same, ready to draw: floats, because a sub-pixel disagreement about
/// where somebody else's shoulder is cannot desynchronise anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnPose {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

/// The last two poses this client was told about, per player.
#[derive(Clone, Copy, Debug)]
struct Track {
    previous: Pose,
    current: Pose,
}

/// Everyone this client can see, except itself.
///
/// Client-side, like [`Prediction`](crate::predict::Prediction), and in this
/// crate for the same reason: it is netcode rather than presentation, it needs
/// no GPU, and keeping it out of `cubara-app` means it can be tested to the
/// tick instead of to the frame.
#[derive(Debug, Default)]
pub struct OtherPlayers {
    /// In `PlayerId` order, so two clients given the same updates draw the same
    /// list in the same order (Rule 1's habit, applied to what is on screen).
    tracks: BTreeMap<PlayerId, Track>,
}

impl OtherPlayers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take an `Effect::PlayerMoved`.
    ///
    /// The first sighting sets both poses to the same value, so a player who
    /// appears is drawn where they are rather than sliding in from wherever the
    /// previous occupant of that slot happened to be.
    pub fn moved(&mut self, who: PlayerId, pos: FixedVec3, yaw: Angle, pitch: Angle) {
        let now = Pose { pos, yaw, pitch };
        match self.tracks.get_mut(&who) {
            Some(track) => {
                track.previous = track.current;
                track.current = now;
            }
            None => {
                self.tracks.insert(
                    who,
                    Track {
                        previous: now,
                        current: now,
                    },
                );
            }
        }
    }

    /// Take an `Effect::PlayerGone`: they left, or walked out of sight.
    ///
    /// Forgotten outright rather than faded: the server stops talking about
    /// them, so anything drawn after this would be this client's invention.
    pub fn gone(&mut self, who: PlayerId) {
        self.tracks.remove(&who);
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Everyone, positioned `alpha` of the way from their previous known pose to
    /// their current one — the same `alpha` the local player's camera uses.
    ///
    /// Angles are interpolated the **short way round**, through
    /// [`Angle::lerp`], because a player turning past north would otherwise spin
    /// most of the way round the compass to get one degree.
    pub fn drawn(&self, alpha: f32) -> Vec<(PlayerId, DrawnPose)> {
        let alpha = alpha.clamp(0.0, 1.0);
        self.tracks
            .iter()
            .map(|(&who, t)| {
                let lerp = |a: cubara_voxel::Fixed, b: cubara_voxel::Fixed| {
                    let (a, b) = (a.to_f32(), b.to_f32());
                    a + (b - a) * alpha
                };
                (
                    who,
                    DrawnPose {
                        pos: [
                            lerp(t.previous.pos.x, t.current.pos.x),
                            lerp(t.previous.pos.y, t.current.pos.y),
                            lerp(t.previous.pos.z, t.current.pos.z),
                        ],
                        yaw: t.previous.yaw.lerp(t.current.yaw, alpha).to_radians(),
                        pitch: t.previous.pitch.lerp(t.current.pitch, alpha).to_radians(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, yaw_raw: i32) -> (FixedVec3, Angle, Angle) {
        (
            FixedVec3::from_f32([x, 40.0, 0.0]),
            Angle::from_raw(yaw_raw),
            Angle::ZERO,
        )
    }

    /// A player seen for the first time is drawn where they are, not slid in
    /// from somewhere.
    ///
    /// The first sighting has no "previous" to interpolate from. Defaulting it
    /// to zero would make everyone who walks into view fly in from the origin.
    #[test]
    fn a_first_sighting_does_not_slide_in_from_anywhere() {
        let mut others = OtherPlayers::new();
        let (pos, yaw, pitch) = at(100.0, 0);
        others.moved(PlayerId(7), pos, yaw, pitch);

        for alpha in [0.0, 0.5, 1.0] {
            let drawn = others.drawn(alpha);
            assert_eq!(drawn.len(), 1);
            assert_eq!(
                drawn[0].1.pos[0], 100.0,
                "a newly seen player was drawn part-way to where they are"
            );
        }
    }

    /// Between two updates, a player is drawn between the two.
    #[test]
    fn a_moving_player_is_drawn_between_their_last_two_poses() {
        let mut others = OtherPlayers::new();
        let (a, ya, pa) = at(10.0, 0);
        let (b, yb, pb) = at(20.0, 0);
        others.moved(PlayerId(1), a, ya, pa);
        others.moved(PlayerId(1), b, yb, pb);

        assert_eq!(others.drawn(0.0)[0].1.pos[0], 10.0);
        assert_eq!(others.drawn(1.0)[0].1.pos[0], 20.0);
        assert_eq!(
            others.drawn(0.5)[0].1.pos[0],
            15.0,
            "half way between two updates is not half way between two poses"
        );
    }

    /// A player who stops moving stops producing updates, and must not drift.
    ///
    /// `Effect::PlayerMoved` is only sent when a pose changes, so standing still
    /// means silence rather than a stream of identical messages. Anything that
    /// assumed one update per tick would keep interpolating toward a target that
    /// never arrives.
    #[test]
    fn a_player_who_stopped_stays_where_they_stopped() {
        let mut others = OtherPlayers::new();
        let (a, ya, pa) = at(10.0, 0);
        let (b, yb, pb) = at(20.0, 0);
        others.moved(PlayerId(1), a, ya, pa);
        others.moved(PlayerId(1), b, yb, pb);
        // They stop: the server sends nothing more. Time passes anyway.
        others.moved(PlayerId(1), b, yb, pb);

        for alpha in [0.0, 0.25, 0.5, 1.0] {
            assert_eq!(
                others.drawn(alpha)[0].1.pos[0],
                20.0,
                "a stationary player drifted at alpha {alpha}"
            );
        }
    }

    /// Turning past north takes the short way round.
    ///
    /// A binary angle wraps, so 350° to 10° is twenty degrees, not three hundred
    /// and forty. Interpolating the raw numbers would spin a player almost all
    /// the way round the compass to look slightly right.
    #[test]
    fn a_turn_past_north_does_not_spin_the_long_way() {
        let mut others = OtherPlayers::new();
        // Either side of the wrap: `i32::MAX` and `i32::MIN` are adjacent on
        // the circle, both a hair short of half a turn from zero.
        let nearly_round = Angle::from_raw(i32::MAX - 1000);
        let just_past = Angle::from_raw(i32::MIN + 1000);
        let pos = FixedVec3::from_f32([0.0, 40.0, 0.0]);
        others.moved(PlayerId(1), pos, nearly_round, Angle::ZERO);
        others.moved(PlayerId(1), pos, just_past, Angle::ZERO);

        // Half way between two angles that are a whisker apart across the wrap
        // is *at* the wrap -- near ±pi. Going the long way round would pass
        // through zero, which is the failure this exists for.
        let half = others.drawn(0.5)[0].1.yaw;
        assert!(
            half.abs() > 3.0,
            "the halfway yaw was {half}, near zero: the turn went the long way \
             round instead of across the wrap"
        );
    }

    /// A player who left is forgotten, not faded.
    ///
    /// The server has stopped talking about them, so anything drawn after this
    /// would be the client's own invention.
    #[test]
    fn a_departed_player_is_forgotten() {
        let mut others = OtherPlayers::new();
        let (pos, yaw, pitch) = at(5.0, 0);
        others.moved(PlayerId(2), pos, yaw, pitch);
        assert_eq!(others.len(), 1);

        others.gone(PlayerId(2));
        assert!(others.is_empty(), "a departed player is still being drawn");
        assert!(others.drawn(0.5).is_empty());
    }

    /// Everyone comes back in `PlayerId` order, whatever order they arrived in.
    #[test]
    fn players_are_drawn_in_id_order() {
        let mut others = OtherPlayers::new();
        for id in [9u64, 2, 5] {
            let (pos, yaw, pitch) = at(id as f32, 0);
            others.moved(PlayerId(id), pos, yaw, pitch);
        }
        let ids: Vec<u64> = others.drawn(1.0).into_iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![2, 5, 9], "draw order depends on arrival order");
    }
}
