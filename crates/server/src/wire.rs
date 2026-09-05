//! The wire: how a message becomes bytes, and back.
//!
//! Block 2.12, designed in `docs/PHASE2_MULTIPLAYER.md` §5.
//!
//! # Hand-written, and why
//!
//! The project owner's standing constraint, 2026-09-05: *"altijd zoveel
//! mogelijk door kunnen bouwen op wat we hebben en weinig afhankelijkheden."*
//! So there is no `serde`, no `bincode`, no schema compiler — a few hundred
//! lines of `to_le_bytes` and a cursor, which the project owns outright and can
//! change without anyone's permission.
//!
//! That is not only a dependency argument. A hand-written codec makes every
//! field an explicit decision: nothing crosses this seam because a derive macro
//! found it, which for a protocol that will eventually face untrusted clients
//! (block 2.14) is the property worth having.
//!
//! # The rules this format holds to
//!
//! - **Little-endian, fixed width.** No varints. A `u32` is four bytes wherever
//!   it appears, so a decoder can bound its work before it does any.
//! - **A tag byte per enum**, and an unknown tag is an error rather than a skip.
//!   A decoder that silently ignores what it does not understand is how two
//!   versions of a client quietly disagree about the world.
//! - **`Option` is a presence byte then the payload**, never a sentinel value.
//!   The same discipline `WorldHash::write_inventory` already uses, and for the
//!   same reason: an absent thing and a zero thing must not encode alike.
//! - **Decoding is total.** Every `decode` returns a [`WireError`] rather than
//!   panicking, on truncated input, an unknown tag, or a length that overruns
//!   the buffer. This code will one day read bytes from a stranger.
//!
//! # Floats
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §3.5 says nothing crossing the wire should be
//! a float, and `look_delta` was migrated to [`Angle`] for exactly that. One
//! float remains: `InputFrame::move_axes`.
//!
//! It is encoded as **raw IEEE-754 bits** (`f32::to_bits`), which is exact — the
//! value that arrives is bit-for-bit the value that was sent, so the transport
//! introduces no divergence. §3.5's concern was never ordinary arithmetic, which
//! IEEE-754 pins exactly; it was `sin`/`cos`, where two platforms' libm
//! genuinely disagree, and there is none of that here.
//!
//! Migrating `move_axes` to fixed-point would still be tidier and would let this
//! module say "no floats" without a footnote. It changes `physics::step` and
//! every pinned hash with it, so it is not this block's business.

use cubara_voxel::{Angle, BlockId, Fixed, FixedVec3, ItemId};
use cubara_world::Furnace;

use cubara_sim::{InputFrame, PlayerId, PlayerState};

use crate::{Action, Effect, FurnaceSlot, Screen};

/// Why a buffer could not be read.
///
/// Named cases rather than a string: a caller that wants to log "this client
/// sent nonsense" and a caller that wants to wait for more bytes need to tell
/// those apart, and [`WireError::Truncated`] is the one that means "not yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The buffer ended mid-message. Over a stream this means *wait*, not
    /// *fail* — which is why it is its own case.
    Truncated,
    /// A tag byte no version of this protocol defines.
    BadTag(u8),
    /// A length prefix that would run past the end of the buffer.
    BadLength(u32),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Truncated => write!(f, "message ended early"),
            WireError::BadTag(t) => write!(f, "unknown tag byte {t}"),
            WireError::BadLength(n) => write!(f, "length prefix {n} overruns the buffer"),
        }
    }
}

impl std::error::Error for WireError {}

/// A reader that cannot run off the end.
///
/// Every `take` is checked, so a decoder is a sequence of `?` and never an index
/// that could panic on a hostile buffer.
pub struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    /// How many bytes have been consumed.
    pub fn position(&self) -> usize {
        self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Truncated)?;
        let slice = self.buf.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, WireError> {
        // Anything non-zero is true. A stricter reading would reject 2, and it
        // would buy nothing: both encoders write 0 or 1, and a client that sent
        // 2 has told us "true" unambiguously.
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, WireError> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn i64(&mut self) -> Result<i64, WireError> {
        Ok(self.u64()? as i64)
    }

    pub fn f32(&mut self) -> Result<f32, WireError> {
        // From the raw bits, so the value is exactly the one that was sent.
        Ok(f32::from_bits(self.u32()?))
    }

    fn pos(&mut self) -> Result<[i32; 3], WireError> {
        Ok([self.i32()?, self.i32()?, self.i32()?])
    }

    fn fixed_vec3(&mut self) -> Result<FixedVec3, WireError> {
        Ok(FixedVec3::new(
            Fixed::from_raw(self.i64()?),
            Fixed::from_raw(self.i64()?),
            Fixed::from_raw(self.i64()?),
        ))
    }

    fn angle(&mut self) -> Result<Angle, WireError> {
        Ok(Angle::from_raw(self.i32()?))
    }

    fn item_slot(&mut self) -> Result<Option<(ItemId, u8)>, WireError> {
        if !self.bool()? {
            return Ok(None);
        }
        Ok(Some((ItemId(self.u16()?), self.u8()?)))
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn put_pos(out: &mut Vec<u8>, p: [i32; 3]) {
    for v in p {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_fixed_vec3(out: &mut Vec<u8>, v: FixedVec3) {
    for f in [v.x, v.y, v.z] {
        out.extend_from_slice(&f.raw().to_le_bytes());
    }
}

fn put_item_slot(out: &mut Vec<u8>, slot: Option<(ItemId, u8)>) {
    match slot {
        None => out.push(0),
        Some((id, count)) => {
            out.push(1);
            out.extend_from_slice(&id.0.to_le_bytes());
            out.push(count);
        }
    }
}

/// A player's whole physics state (block 2.12b).
///
/// Every field `Sim::tick` reads, because a client replaying its input has to
/// start from all of it -- see `PlayerState`'s own docs for what dropping one
/// costs.
fn put_player_state(out: &mut Vec<u8>, s: &PlayerState) {
    put_fixed_vec3(out, s.pos);
    put_fixed_vec3(out, s.velocity);
    out.extend_from_slice(&s.yaw.raw().to_le_bytes());
    out.extend_from_slice(&s.pitch.raw().to_le_bytes());
    out.push(s.on_ground as u8);
    out.push(s.free_fly as u8);
    out.extend_from_slice(&s.fall_distance.raw().to_le_bytes());
    out.push(s.health);
    out.extend_from_slice(&s.ticks_since_damage.to_le_bytes());
}

fn get_player_state(c: &mut Cursor<'_>) -> Result<PlayerState, WireError> {
    Ok(PlayerState {
        pos: c.fixed_vec3()?,
        velocity: c.fixed_vec3()?,
        yaw: c.angle()?,
        pitch: c.angle()?,
        on_ground: c.bool()?,
        free_fly: c.bool()?,
        fall_distance: Fixed::from_raw(c.i64()?),
        health: c.u8()?,
        ticks_since_damage: c.u32()?,
    })
}

fn put_furnace(out: &mut Vec<u8>, f: &Furnace) {
    put_item_slot(out, f.input);
    put_item_slot(out, f.fuel);
    put_item_slot(out, f.output);
    out.extend_from_slice(&f.burning.to_le_bytes());
    out.extend_from_slice(&f.progress.to_le_bytes());
}

fn get_furnace(c: &mut Cursor<'_>) -> Result<Furnace, WireError> {
    Ok(Furnace {
        input: c.item_slot()?,
        fuel: c.item_slot()?,
        output: c.item_slot()?,
        burning: c.u32()?,
        progress: c.u32()?,
    })
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

impl Screen {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Screen::Bench => out.push(0),
            Screen::Furnace(pos) => {
                out.push(1);
                put_pos(out, *pos);
            }
        }
    }

    fn decode(c: &mut Cursor<'_>) -> Result<Self, WireError> {
        match c.u8()? {
            0 => Ok(Screen::Bench),
            1 => Ok(Screen::Furnace(c.pos()?)),
            t => Err(WireError::BadTag(t)),
        }
    }
}

// ---------------------------------------------------------------------------
// Effect — server to client
// ---------------------------------------------------------------------------

impl Effect {
    /// Append this effect's bytes to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Effect::Edit { pos, block } => {
                out.push(0);
                put_pos(out, *pos);
                out.extend_from_slice(&block.0.to_le_bytes());
            }
            Effect::BlockEntity { pos, furnace } => {
                out.push(1);
                put_pos(out, *pos);
                match furnace {
                    None => out.push(0),
                    Some(f) => {
                        out.push(1);
                        put_furnace(out, f);
                    }
                }
            }
            Effect::Open(screen) => {
                out.push(2);
                screen.encode(out);
            }
            Effect::CloseIfAt(pos) => {
                out.push(3);
                put_pos(out, *pos);
            }
            Effect::PlayerMoved {
                who,
                pos,
                yaw,
                pitch,
            } => {
                out.push(4);
                out.extend_from_slice(&who.0.to_le_bytes());
                put_fixed_vec3(out, *pos);
                out.extend_from_slice(&yaw.raw().to_le_bytes());
                out.extend_from_slice(&pitch.raw().to_le_bytes());
            }
            Effect::PlayerGone(who) => {
                out.push(5);
                out.extend_from_slice(&who.0.to_le_bytes());
            }
            Effect::SelfState { seq, state } => {
                out.push(6);
                out.extend_from_slice(&seq.to_le_bytes());
                put_player_state(out, state);
            }
        }
    }

    fn decode(c: &mut Cursor<'_>) -> Result<Self, WireError> {
        Ok(match c.u8()? {
            0 => Effect::Edit {
                pos: c.pos()?,
                block: BlockId(c.u16()?),
            },
            1 => {
                let pos = c.pos()?;
                let furnace = if c.bool()? {
                    Some(get_furnace(c)?)
                } else {
                    None
                };
                Effect::BlockEntity { pos, furnace }
            }
            2 => Effect::Open(Screen::decode(c)?),
            3 => Effect::CloseIfAt(c.pos()?),
            4 => Effect::PlayerMoved {
                who: PlayerId(c.u64()?),
                pos: c.fixed_vec3()?,
                yaw: c.angle()?,
                pitch: c.angle()?,
            },
            5 => Effect::PlayerGone(PlayerId(c.u64()?)),
            6 => Effect::SelfState {
                seq: c.u64()?,
                state: get_player_state(c)?,
            },
            t => return Err(WireError::BadTag(t)),
        })
    }

    /// Exactly how many bytes [`encode`](Self::encode) writes.
    ///
    /// Block 2.11 had a hand-tallied `wire_size` standing in for this while
    /// there was no codec, and said so. This is the real number, and it is
    /// measured rather than counted: the scaling criterion in
    /// `tests/client_view.rs` now weighs actual bytes, so it cannot drift away
    /// from what the socket does.
    pub fn wire_size(&self) -> usize {
        let mut buf = Vec::new();
        self.encode(&mut buf);
        buf.len()
    }
}

// ---------------------------------------------------------------------------
// Action and InputFrame — client to server
// ---------------------------------------------------------------------------

impl Action {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Action::Break => out.push(0),
            Action::Place => out.push(1),
            Action::Interact => out.push(2),
            Action::ClickFurnace { pos, slot } => {
                out.push(3);
                put_pos(out, *pos);
                out.push(match slot {
                    FurnaceSlot::Input => 0,
                    FurnaceSlot::Fuel => 1,
                    FurnaceSlot::Output => 2,
                });
            }
        }
    }

    fn decode(c: &mut Cursor<'_>) -> Result<Self, WireError> {
        Ok(match c.u8()? {
            0 => Action::Break,
            1 => Action::Place,
            2 => Action::Interact,
            3 => {
                let pos = c.pos()?;
                let slot = match c.u8()? {
                    0 => FurnaceSlot::Input,
                    1 => FurnaceSlot::Fuel,
                    2 => FurnaceSlot::Output,
                    t => return Err(WireError::BadTag(t)),
                };
                Action::ClickFurnace { pos, slot }
            }
            t => return Err(WireError::BadTag(t)),
        })
    }
}

fn put_input(out: &mut Vec<u8>, i: &InputFrame) {
    for axis in i.move_axes {
        out.extend_from_slice(&axis.to_bits().to_le_bytes());
    }
    out.extend_from_slice(&i.look_delta[0].raw().to_le_bytes());
    out.extend_from_slice(&i.look_delta[1].raw().to_le_bytes());
    out.push(i.jump as u8);
    out.push(i.toggle_fly as u8);
    out.push(i.breaking as u8);
}

fn get_input(c: &mut Cursor<'_>) -> Result<InputFrame, WireError> {
    Ok(InputFrame {
        move_axes: [c.f32()?, c.f32()?, c.f32()?],
        look_delta: [c.angle()?, c.angle()?],
        jump: c.bool()?,
        toggle_fly: c.bool()?,
        breaking: c.bool()?,
    })
}

// ---------------------------------------------------------------------------
// The messages themselves
// ---------------------------------------------------------------------------

/// What a client sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// Asking to join. Carries nothing: a client does not get to say who it is
    /// (§3.4), so the server names it in [`ServerMessage::Welcome`].
    Hello,
    /// One tick's controls, and which input this is.
    ///
    /// `seq` counts this client's inputs, starting at 0 and never reset. The
    /// server echoes the last one it applied in [`Effect::SelfState`], which is
    /// what lets a predicting client (block 2.13) know which of its outstanding
    /// inputs to discard and which to replay.
    ///
    /// A `u64` rather than a `u32` because at 60 Hz a `u32` wraps after about
    /// two and a bit years of uptime, and wrap-aware comparison is a bug factory
    /// bought for nothing.
    Input { seq: u64, frame: InputFrame },
    /// One thing the client is asking the world to do.
    Act(Action),
}

/// What a server sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    /// The join handshake: the seed the client generates terrain from, and which
    /// player it is driving.
    ///
    /// **The seed, not the terrain** (§3). This one `u64` replaces everything a
    /// naive protocol would send about the shape of the world.
    Welcome {
        seed: u64,
        you: PlayerId,
        /// Fingerprints of the two registries whose ids cross this wire.
        ///
        /// Ids cross this wire **raw**, and they are assigned from sorted names
        /// (`ItemRegistry::from_defs`), so two machines with identical assets
        /// agree exactly -- and two machines whose assets differ by one file
        /// disagree about the whole table from that name onward. Stone becomes
        /// iron, silently.
        ///
        /// The save format solved this by storing names and remapping on load
        /// (design §8.1). The wire refuses instead: remapping is more work and
        /// it *hides* that two people are running different versions, which is
        /// the thing they most need to be told. It also cannot fix it -- a
        /// renamed id can be translated, an item one side lacks cannot be
        /// invented.
        ///
        /// **Why these two and not recipes or smelting.** The rule is: hash
        /// every *id space* whose ids cross this wire, and those are blocks and
        /// items. (Smelting ids do cross, inside a `Furnace`'s three
        /// `Option<(ItemId, u8)>` slots -- but they are `ItemId`s, so the item
        /// fingerprint already covers them.)
        ///
        /// Recipes and smelting are rule tables, not id spaces. A difference
        /// there gives a wrong *prediction* -- the client previews a craft the
        /// server will not make -- rather than a wrong *identity*, which is
        /// stone silently becoming iron. The first is annoying and visible; the
        /// second is quiet corruption, and only the second earns a refusal to
        /// connect at all.
        ///
        /// Worth revisiting when crafting becomes an `Action`: a mismatch turns
        /// into a rejected action rather than a misleading preview, and that is
        /// a different trade.
        blocks: u64,
        items: u64,
    },
    /// Everything owed since the last message.
    Effects(Vec<Effect>),
    /// The tick this batch belongs to.
    Tick(u64),
}

impl ClientMessage {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ClientMessage::Hello => out.push(0),
            ClientMessage::Input { seq, frame } => {
                out.push(1);
                out.extend_from_slice(&seq.to_le_bytes());
                put_input(out, frame);
            }
            ClientMessage::Act(a) => {
                out.push(2);
                a.encode(out);
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        let c = &mut Cursor::new(buf);
        Ok(match c.u8()? {
            0 => ClientMessage::Hello,
            1 => ClientMessage::Input {
                seq: c.u64()?,
                frame: get_input(c)?,
            },
            2 => ClientMessage::Act(Action::decode(c)?),
            t => return Err(WireError::BadTag(t)),
        })
    }
}

impl ServerMessage {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ServerMessage::Welcome {
                seed,
                you,
                blocks,
                items,
            } => {
                out.push(0);
                out.extend_from_slice(&seed.to_le_bytes());
                out.extend_from_slice(&you.0.to_le_bytes());
                out.extend_from_slice(&blocks.to_le_bytes());
                out.extend_from_slice(&items.to_le_bytes());
            }
            ServerMessage::Effects(effects) => {
                out.push(1);
                // A u32 count, not a terminator: a decoder should know how much
                // work it is about to do before it starts doing it.
                out.extend_from_slice(&(effects.len() as u32).to_le_bytes());
                for e in effects {
                    e.encode(out);
                }
            }
            ServerMessage::Tick(t) => {
                out.push(2);
                out.extend_from_slice(&t.to_le_bytes());
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self, WireError> {
        let c = &mut Cursor::new(buf);
        Ok(match c.u8()? {
            0 => ServerMessage::Welcome {
                seed: c.u64()?,
                you: PlayerId(c.u64()?),
                blocks: c.u64()?,
                items: c.u64()?,
            },
            1 => {
                let n = c.u32()?;
                // Bound the allocation by what is actually left to read, so a
                // hostile "four billion effects" cannot make us reserve four
                // billion anythings before discovering the buffer is empty.
                let mut effects = Vec::new();
                for _ in 0..n {
                    effects.push(Effect::decode(c)?);
                }
                ServerMessage::Effects(effects)
            }
            2 => ServerMessage::Tick(c.u64()?),
            t => return Err(WireError::BadTag(t)),
        })
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// The largest message this protocol will read.
///
/// TCP is a stream, and a length prefix arriving from a stranger is a request to
/// allocate. Sixteen megabytes is far more than any message here needs — the
/// biggest is a join handshake full of edits — and small enough that a bad or
/// hostile prefix cannot exhaust memory before block 2.14 exists to be stricter.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// Wrap `payload` in its length prefix.
pub fn frame(payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

/// Split one complete frame off the front of `buf`.
///
/// `Ok(None)` means *not yet* — the frame is still arriving. That is the normal
/// case on a stream and deliberately not an error: a reader that treated a
/// partial frame as a failure would drop a connection every time a message
/// straddled a packet boundary, which is the classic first netcode bug.
pub fn unframe(buf: &[u8]) -> Result<Option<(&[u8], usize)>, WireError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if len > MAX_FRAME {
        return Err(WireError::BadLength(len));
    }
    let end = 4 + len as usize;
    if buf.len() < end {
        return Ok(None);
    }
    Ok(Some((&buf[4..end], end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_effect(e: Effect) {
        let mut buf = Vec::new();
        e.encode(&mut buf);
        assert_eq!(buf.len(), e.wire_size(), "wire_size disagrees with encode");
        let mut c = Cursor::new(&buf);
        let back = Effect::decode(&mut c).expect("decodes");
        assert_eq!(back, e, "an effect did not survive the round trip");
        assert_eq!(c.position(), buf.len(), "decode left bytes unread");
    }

    fn a_furnace() -> Furnace {
        Furnace {
            input: Some((ItemId(3), 7)),
            fuel: None,
            output: Some((ItemId(11), 1)),
            burning: 40,
            progress: 123,
        }
    }

    /// Every variant, because a codec is exactly as correct as its least-tested
    /// arm and the untested one is always the one that ships.
    #[test]
    fn every_effect_survives_a_round_trip() {
        round_trip_effect(Effect::Edit {
            pos: [-1, 2_000, 3],
            block: BlockId(9),
        });
        round_trip_effect(Effect::BlockEntity {
            pos: [4, -5, 6],
            furnace: Some(a_furnace()),
        });
        round_trip_effect(Effect::BlockEntity {
            pos: [4, -5, 6],
            furnace: None,
        });
        round_trip_effect(Effect::Open(Screen::Bench));
        round_trip_effect(Effect::Open(Screen::Furnace([7, 8, 9])));
        round_trip_effect(Effect::CloseIfAt([-3, -4, -5]));
        round_trip_effect(Effect::PlayerMoved {
            who: PlayerId(42),
            pos: FixedVec3::from_f32([1.5, -20.25, 3.75]),
            yaw: Angle::from_raw(123_456),
            pitch: Angle::from_raw(-7_654),
        });
        round_trip_effect(Effect::PlayerGone(PlayerId(u64::MAX)));
        round_trip_effect(Effect::SelfState {
            seq: u64::MAX,
            state: a_player_state(),
        });
    }

    /// Every field of a correction survives, checked one at a time.
    ///
    /// A round trip of a struct where every field happens to be zero passes
    /// whether or not the encoder wrote them, so each field is given a value
    /// nothing else has. Dropping `velocity` here is the failure block 2.13
    /// would meet as a permanent twitch rather than as a test.
    #[test]
    fn a_correction_carries_every_field_physics_reads() {
        let s = a_player_state();
        let mut buf = Vec::new();
        Effect::SelfState { seq: 7, state: s }.encode(&mut buf);
        let back = Effect::decode(&mut Cursor::new(&buf)).expect("decodes");
        let Effect::SelfState { seq, state } = back else {
            panic!("wrong variant");
        };
        assert_eq!(seq, 7);
        assert_eq!(state.pos, s.pos, "pos");
        assert_eq!(state.velocity, s.velocity, "velocity");
        assert_eq!(state.yaw, s.yaw, "yaw");
        assert_eq!(state.pitch, s.pitch, "pitch");
        assert_eq!(state.on_ground, s.on_ground, "on_ground");
        assert_eq!(state.free_fly, s.free_fly, "free_fly");
        assert_eq!(state.fall_distance, s.fall_distance, "fall_distance");
        assert_eq!(state.health, s.health, "health");
        assert_eq!(
            state.ticks_since_damage, s.ticks_since_damage,
            "ticks_since_damage"
        );
    }

    /// Distinct, non-default values throughout, so a field the encoder forgot
    /// cannot pass by coincidence.
    fn a_player_state() -> PlayerState {
        PlayerState {
            pos: FixedVec3::from_f32([1.5, -20.25, 3.75]),
            velocity: FixedVec3::from_f32([-0.5, 9.0, 0.125]),
            yaw: Angle::from_raw(123_456),
            pitch: Angle::from_raw(-7_654),
            on_ground: true,
            free_fly: false,
            fall_distance: Fixed::from_f32(12.5),
            health: 13,
            ticks_since_damage: 4_242,
        }
    }

    #[test]
    fn every_client_message_survives_a_round_trip() {
        let messages = [
            ClientMessage::Hello,
            ClientMessage::Input {
                seq: u64::MAX,
                frame: InputFrame {
                    move_axes: [-1.0, 0.25, 1.0],
                    look_delta: [Angle::from_raw(9), Angle::from_raw(-9)],
                    jump: true,
                    toggle_fly: false,
                    breaking: true,
                },
            },
            ClientMessage::Act(Action::Break),
            ClientMessage::Act(Action::ClickFurnace {
                pos: [1, 2, 3],
                slot: FurnaceSlot::Output,
            }),
        ];
        for m in messages {
            let mut buf = Vec::new();
            m.encode(&mut buf);
            assert_eq!(ClientMessage::decode(&buf).expect("decodes"), m);
        }
    }

    #[test]
    fn every_server_message_survives_a_round_trip() {
        let messages = [
            ServerMessage::Welcome {
                seed: 0x00C0_FFEE_D0D0,
                you: PlayerId(2),
                blocks: 0xABCD,
                items: 0x1234,
            },
            ServerMessage::Tick(1_234_567),
            ServerMessage::Effects(vec![
                Effect::Edit {
                    pos: [0, 0, 0],
                    block: BlockId(1),
                },
                Effect::PlayerGone(PlayerId(5)),
            ]),
            ServerMessage::Effects(Vec::new()),
        ];
        for m in messages {
            let mut buf = Vec::new();
            m.encode(&mut buf);
            assert_eq!(ServerMessage::decode(&buf).expect("decodes"), m);
        }
    }

    /// Movement axes cross as raw bits, so the value that arrives is the value
    /// that was sent -- including the awkward ones.
    #[test]
    fn move_axes_cross_exactly() {
        for axis in [0.1_f32, -0.1, 1.0 / 3.0, f32::MIN_POSITIVE, -0.0] {
            let m = ClientMessage::Input {
                seq: 0,
                frame: InputFrame {
                    move_axes: [axis, 0.0, 0.0],
                    ..InputFrame::default()
                },
            };
            let mut buf = Vec::new();
            m.encode(&mut buf);
            let ClientMessage::Input { frame: back, .. } =
                ClientMessage::decode(&buf).expect("decodes")
            else {
                panic!("wrong variant");
            };
            assert_eq!(
                back.move_axes[0].to_bits(),
                axis.to_bits(),
                "{axis} did not survive as the same bits"
            );
        }
    }

    // -- the hostile cases ---------------------------------------------------

    /// Every prefix of a valid message must be refused, not misread.
    ///
    /// The loop is the point: it is easy to handle "empty buffer" and miss
    /// "buffer that ends one byte into a position", and the second is what a
    /// real stream produces constantly.
    #[test]
    fn every_truncation_is_refused() {
        let m = ServerMessage::Effects(vec![Effect::BlockEntity {
            pos: [1, 2, 3],
            furnace: Some(a_furnace()),
        }]);
        let mut buf = Vec::new();
        m.encode(&mut buf);

        for cut in 0..buf.len() {
            assert!(
                ServerMessage::decode(&buf[..cut]).is_err(),
                "a message truncated to {cut} of {} bytes decoded anyway",
                buf.len()
            );
        }
        assert!(
            ServerMessage::decode(&buf).is_ok(),
            "the whole thing decodes"
        );
    }

    #[test]
    fn an_unknown_tag_is_an_error_not_a_skip() {
        assert_eq!(ClientMessage::decode(&[200]), Err(WireError::BadTag(200)));
        assert_eq!(ServerMessage::decode(&[200]), Err(WireError::BadTag(200)));
        // And nested: a valid outer tag with a nonsense inner one.
        assert_eq!(
            ClientMessage::decode(&[2, 200]),
            Err(WireError::BadTag(200)),
            "an unknown action tag was accepted"
        );
    }

    #[test]
    fn a_count_bigger_than_the_buffer_is_refused() {
        // Tag 1 (Effects) claiming four billion effects, with nothing after it.
        let mut buf = vec![1u8];
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(ServerMessage::decode(&buf), Err(WireError::Truncated));
    }

    // -- framing -------------------------------------------------------------

    #[test]
    fn a_frame_that_has_not_arrived_yet_is_not_an_error() {
        let payload = [1u8, 2, 3, 4, 5];
        let mut framed = Vec::new();
        frame(&payload, &mut framed);

        for partial in 0..framed.len() {
            assert_eq!(
                unframe(&framed[..partial]),
                Ok(None),
                "a partial frame of {partial} bytes was treated as a failure"
            );
        }
        assert_eq!(
            unframe(&framed),
            Ok(Some((&payload[..], framed.len()))),
            "a complete frame did not come back"
        );
    }

    #[test]
    fn two_frames_in_one_buffer_come_out_one_at_a_time() {
        let mut buf = Vec::new();
        frame(&[1, 2], &mut buf);
        frame(&[3, 4, 5], &mut buf);

        let (first, used) = unframe(&buf).unwrap().expect("the first frame");
        assert_eq!(first, &[1, 2]);
        let (second, _) = unframe(&buf[used..]).unwrap().expect("the second frame");
        assert_eq!(second, &[3, 4, 5]);
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_before_allocating() {
        let mut buf = (MAX_FRAME + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(&[0; 8]);
        assert_eq!(unframe(&buf), Err(WireError::BadLength(MAX_FRAME + 1)));
    }
}
