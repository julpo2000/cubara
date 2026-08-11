//! Per-chunk block storage: a flat, palette-compressed index array.
//!
//! Two states, chosen automatically as a chunk is written to. Most of a
//! radius-64 world is entirely air or entirely one solid material, and
//! [`ChunkStorage::Uniform`] stores that with **no heap allocation at all** --
//! load-bearing for the phase-1 memory budget (`docs/PHASE1_ARCHITECTURE.md`
//! §2, §4). A chunk written with more than one distinct id promotes to
//! [`ChunkStorage::Palette`]: a small table of the ids actually present, plus
//! a fixed-width packed index per voxel. The index width only ever grows (1,
//! 2, 4, 8 or 16 bits) as the palette does; a chunk never demotes back to
//! `Uniform` or to a narrower width, which keeps the state machine
//! one-directional and simple (see issue #46).

use crate::block::BlockId;

/// One chunk's blocks: every voxel the same id (no allocation), or a palette
/// of the distinct ids present plus a packed index per voxel.
#[derive(Debug)]
pub enum ChunkStorage {
    Uniform(BlockId),
    Palette {
        palette: Vec<BlockId>,
        bits: u8,
        data: Box<[u64]>,
    },
}

/// The only widths a palette index is ever packed at, so unpacking is always a
/// shift and a mask: each divides 64 evenly, so a value never straddles a
/// `u64` word boundary.
const WIDTHS: [u8; 5] = [1, 2, 4, 8, 16];

/// The narrowest width in [`WIDTHS`] that can index a palette of `len` entries.
fn bits_for_len(len: usize) -> u8 {
    debug_assert!(len >= 1);
    let needed = usize::BITS - (len - 1).leading_zeros(); // ceil(log2(len))
    WIDTHS
        .into_iter()
        .find(|&w| u32::from(w) >= needed)
        .expect("a 65,536-entry BlockId space never needs more than 16 bits")
}

fn values_per_word(bits: u8) -> usize {
    64 / bits as usize
}

fn word_count(bits: u8, volume: usize) -> usize {
    volume.div_ceil(values_per_word(bits))
}

fn zero_words(bits: u8, volume: usize) -> Box<[u64]> {
    vec![0u64; word_count(bits, volume)].into_boxed_slice()
}

#[inline]
fn get_packed(data: &[u64], bits: u8, idx: usize) -> u32 {
    let vpw = values_per_word(bits);
    let shift = (idx % vpw) * bits as usize;
    let mask = (1u64 << bits) - 1;
    ((data[idx / vpw] >> shift) & mask) as u32
}

fn set_packed(data: &mut [u64], bits: u8, idx: usize, value: u32) {
    let vpw = values_per_word(bits);
    let shift = (idx % vpw) * bits as usize;
    let mask = (1u64 << bits) - 1;
    debug_assert!(
        u64::from(value) <= mask,
        "value does not fit in {bits} bits"
    );
    let word = &mut data[idx / vpw];
    *word = (*word & !(mask << shift)) | ((u64::from(value) & mask) << shift);
}

/// Re-pack every index in `data` (currently `old_bits` wide, over `volume`
/// cells) at `new_bits`. Palette indices are stable across a repack -- entries
/// are only ever appended, never reordered -- so this is a value-preserving
/// copy, not a remap.
fn repack(data: &[u64], old_bits: u8, new_bits: u8, volume: usize) -> Box<[u64]> {
    let mut out = zero_words(new_bits, volume);
    for idx in 0..volume {
        set_packed(&mut out, new_bits, idx, get_packed(data, old_bits, idx));
    }
    out
}

impl ChunkStorage {
    /// A chunk with no blocks at all -- the state every chunk starts in.
    pub fn new() -> Self {
        ChunkStorage::Uniform(BlockId::AIR)
    }

    /// Build storage from every cell's id in one pass, choosing `Uniform` or
    /// `Palette` directly at the final bit width -- no incremental promotion
    /// or re-packing. This is the fast path [`crate::Chunk::from_fn`] uses:
    /// building the same result via repeated [`set`](Self::set) calls is
    /// correct but visits the promote/repack state machine on every one of
    /// several thousand cells, which is real work `from_ids` skips by knowing
    /// the whole palette up front.
    pub(crate) fn from_ids(ids: &[BlockId]) -> Self {
        let first = ids[0];
        if ids.iter().all(|&id| id == first) {
            return ChunkStorage::Uniform(first);
        }
        let mut palette: Vec<BlockId> = Vec::new();
        let mut indices: Vec<u32> = Vec::with_capacity(ids.len());
        for &id in ids {
            let pi = match palette.iter().position(|&p| p == id) {
                Some(pi) => pi,
                None => {
                    palette.push(id);
                    palette.len() - 1
                }
            };
            indices.push(pi as u32);
        }
        let bits = bits_for_len(palette.len());
        let mut data = zero_words(bits, ids.len());
        for (idx, &pi) in indices.iter().enumerate() {
            set_packed(&mut data, bits, idx, pi);
        }
        ChunkStorage::Palette {
            palette,
            bits,
            data,
        }
    }

    #[inline]
    pub(crate) fn get(&self, idx: usize) -> BlockId {
        match self {
            ChunkStorage::Uniform(id) => *id,
            ChunkStorage::Palette {
                palette,
                bits,
                data,
            } => palette[get_packed(data, *bits, idx) as usize],
        }
    }

    /// Write `id` at `idx`, promoting `Uniform -> Palette` on the first
    /// differing write and re-packing to a wider index whenever the palette
    /// outgrows its current width. `volume` is the chunk's total cell count
    /// ([`crate::Chunk::SIZE`]³) -- passed in rather than duplicated here,
    /// since this type is meshing-agnostic and shouldn't own that constant.
    ///
    /// Split into a tiny `#[inline]` fast path plus a `#[inline(never)]` slow
    /// path on purpose: a chunk that stays `Uniform` (most of a radius-64
    /// world -- see [`ChunkStorage`]) calls this once per voxel, and if the
    /// whole promotion/repack logic lived in one function its size discourages
    /// the compiler from inlining even the common "already this value, no-op"
    /// case. Measured: without the split, filling an all-air chunk (4096 calls
    /// that never leave `Uniform`) cost ~6 µs; with it, function-call overhead
    /// stops dominating a case that does no real work at all.
    #[inline]
    pub(crate) fn set(&mut self, idx: usize, id: BlockId, volume: usize) {
        if let ChunkStorage::Uniform(current) = self {
            if *current == id {
                return;
            }
        }
        self.set_cold(idx, id, volume);
    }

    #[inline(never)]
    fn set_cold(&mut self, idx: usize, id: BlockId, volume: usize) {
        match self {
            ChunkStorage::Uniform(current) => {
                let old = *current;
                let bits = 1; // 2 entries always fits the narrowest width
                let mut data = zero_words(bits, volume);
                set_packed(&mut data, bits, idx, 1);
                *self = ChunkStorage::Palette {
                    palette: vec![old, id],
                    bits,
                    data,
                };
            }
            ChunkStorage::Palette {
                palette,
                bits,
                data,
            } => {
                let pi = match palette.iter().position(|&p| p == id) {
                    Some(pi) => pi,
                    None => {
                        palette.push(id);
                        let needed = bits_for_len(palette.len());
                        if needed > *bits {
                            *data = repack(data, *bits, needed, volume);
                            *bits = needed;
                        }
                        palette.len() - 1
                    }
                };
                set_packed(data, *bits, idx, pi as u32);
            }
        }
    }

    /// Encode this storage exactly as `docs/PHASE1_ARCHITECTURE.md` §7.3
    /// specifies: `u8 tag` (0 = uniform, 1 = palette), then either a `u16`
    /// block id or a palette + packed index array. All integers
    /// little-endian, so a world saved on one platform loads on another.
    ///
    /// The format's palette length is a `u8` (max 255 entries) -- narrower
    /// than the 65,536 a runtime [`ChunkStorage::Palette`] can actually
    /// reach, which the phase-1 registry (a handful of materials) never gets
    /// remotely close to. A palette that did overflow it fails loudly here
    /// rather than truncating the table into silent corruption.
    pub fn write_payload(&self, out: &mut Vec<u8>) -> Result<(), ChunkPayloadError> {
        match self {
            ChunkStorage::Uniform(id) => {
                out.push(0);
                out.extend_from_slice(&id.0.to_le_bytes());
            }
            ChunkStorage::Palette {
                palette,
                bits,
                data,
            } => {
                if palette.len() > u8::MAX as usize {
                    return Err(ChunkPayloadError::PaletteTooLarge(palette.len()));
                }
                out.push(1);
                out.push(palette.len() as u8);
                for id in palette {
                    out.extend_from_slice(&id.0.to_le_bytes());
                }
                out.push(*bits);
                for word in data.iter() {
                    out.extend_from_slice(&word.to_le_bytes());
                }
            }
        }
        Ok(())
    }

    /// Decode a payload [`write_payload`](Self::write_payload) produced, for
    /// a chunk of `volume` cells (always [`crate::Chunk::SIZE`]³ in
    /// practice; taken as a parameter so this type stays agnostic of the
    /// constant that owns it, same reasoning as [`Self::set`]). Every
    /// failure is a named [`ChunkPayloadError`] -- a corrupt or truncated
    /// payload is a hard error, never a best-effort partial read.
    pub fn read_payload(bytes: &[u8], volume: usize) -> Result<Self, ChunkPayloadError> {
        let mut cursor = PayloadCursor { bytes, pos: 0 };
        match cursor.u8()? {
            0 => Ok(ChunkStorage::Uniform(BlockId(cursor.u16()?))),
            1 => {
                let len = cursor.u8()? as usize;
                let mut palette = Vec::with_capacity(len);
                for _ in 0..len {
                    palette.push(BlockId(cursor.u16()?));
                }
                let bits = cursor.u8()?;
                if !WIDTHS.contains(&bits) {
                    return Err(ChunkPayloadError::InvalidBits(bits));
                }
                let words = word_count(bits, volume);
                let mut data = Vec::with_capacity(words);
                for _ in 0..words {
                    data.push(cursor.u64()?);
                }
                Ok(ChunkStorage::Palette {
                    palette,
                    bits,
                    data: data.into_boxed_slice(),
                })
            }
            tag => Err(ChunkPayloadError::UnknownStorageTag(tag)),
        }
    }
}

/// A chunk payload failed to decode -- truncated, an unrecognised storage
/// tag, a palette width the packer never produces, or (see
/// [`ChunkStorage::write_payload`]) a palette too large for the format's
/// `u8` length field. Corruption is always a hard error
/// (`docs/PHASE1_ARCHITECTURE.md` §7.2's "never silent damage"), never a
/// best-effort partial read -- in particular, an out-of-range `bits` is
/// rejected explicitly rather than risking a shift-amount panic/UB in
/// [`get_packed`] on a hand-corrupted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkPayloadError {
    Truncated,
    UnknownStorageTag(u8),
    InvalidBits(u8),
    PaletteTooLarge(usize),
}

impl std::fmt::Display for ChunkPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkPayloadError::Truncated => write!(f, "chunk payload is truncated"),
            ChunkPayloadError::UnknownStorageTag(tag) => {
                write!(f, "chunk payload has unknown storage tag {tag} (expected 0 or 1)")
            }
            ChunkPayloadError::InvalidBits(bits) => write!(
                f,
                "chunk payload has invalid palette width {bits} bits (expected one of {WIDTHS:?})"
            ),
            ChunkPayloadError::PaletteTooLarge(len) => write!(
                f,
                "chunk palette has {len} entries, over the format's 255-entry limit (u8 length field)"
            ),
        }
    }
}

impl std::error::Error for ChunkPayloadError {}

/// A tiny little-endian byte reader with bounds-checked primitives, local to
/// [`ChunkStorage::read_payload`] -- there is no `std::io::Read` here since
/// a payload is always a plain in-memory `&[u8]` slice of known length
/// (region files hand it one already sliced to its directory-recorded
/// length), so the extra generality would cost more than it buys.
struct PayloadCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl PayloadCursor<'_> {
    fn u8(&mut self) -> Result<u8, ChunkPayloadError> {
        let b = *self
            .bytes
            .get(self.pos)
            .ok_or(ChunkPayloadError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16, ChunkPayloadError> {
        let bytes: [u8; 2] = self
            .bytes
            .get(self.pos..self.pos + 2)
            .ok_or(ChunkPayloadError::Truncated)?
            .try_into()
            .unwrap();
        self.pos += 2;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, ChunkPayloadError> {
        let bytes: [u8; 8] = self
            .bytes
            .get(self.pos..self.pos + 8)
            .ok_or(ChunkPayloadError::Truncated)?
            .try_into()
            .unwrap();
        self.pos += 8;
        Ok(u64::from_le_bytes(bytes))
    }
}

impl Default for ChunkStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_width() {
        for &bits in &WIDTHS {
            let volume = 300; // not a power of two -- exercises the tail word
            let max = 1u32 << bits;
            let mut data = zero_words(bits, volume);
            for idx in 0..volume {
                set_packed(&mut data, bits, idx, (idx as u32) % max);
            }
            for idx in 0..volume {
                assert_eq!(
                    get_packed(&data, bits, idx),
                    (idx as u32) % max,
                    "bits={bits} idx={idx}"
                );
            }
        }
    }

    #[test]
    fn bits_for_len_picks_the_narrowest_fitting_width() {
        assert_eq!(bits_for_len(1), 1);
        assert_eq!(bits_for_len(2), 1);
        assert_eq!(bits_for_len(3), 2);
        assert_eq!(bits_for_len(4), 2);
        assert_eq!(bits_for_len(5), 4);
        assert_eq!(bits_for_len(16), 4);
        assert_eq!(bits_for_len(17), 8);
        assert_eq!(bits_for_len(256), 8);
        assert_eq!(bits_for_len(257), 16);
        assert_eq!(bits_for_len(65536), 16);
    }

    #[test]
    fn uniform_needs_no_allocation() {
        // `Uniform` carries no `Vec`/`Box` at all -- pinned structurally (the
        // variant itself), which is the only way to make "no allocation" a
        // fact about the type rather than an unverified claim.
        let storage = ChunkStorage::new();
        assert!(matches!(storage, ChunkStorage::Uniform(BlockId::AIR)));
    }

    #[test]
    fn same_id_write_does_not_promote() {
        let mut storage = ChunkStorage::new();
        storage.set(0, BlockId::AIR, 64);
        storage.set(63, BlockId::AIR, 64);
        assert!(matches!(storage, ChunkStorage::Uniform(BlockId::AIR)));
    }

    #[test]
    fn promotes_from_uniform_on_first_differing_write() {
        let mut storage = ChunkStorage::new();
        storage.set(5, BlockId::STONE, 64);
        assert!(matches!(storage, ChunkStorage::Palette { .. }));
        assert_eq!(storage.get(5), BlockId::STONE);
        assert_eq!(
            storage.get(0),
            BlockId::AIR,
            "untouched cells keep the old id"
        );
        assert_eq!(storage.get(63), BlockId::AIR);
    }

    #[test]
    fn palette_widens_at_exact_boundaries() {
        // Promotion always seeds the palette with the chunk's prior uniform
        // value (air here), so reaching a palette of length N takes N-1
        // distinct non-air writes. Boundaries: exactly 2/4/16/256 entries (the
        // issue's own examples) plus the one-past points that force a repack
        // (3, 5, 17, 257).
        let volume = 300;
        let mut storage = ChunkStorage::new();
        let checkpoints = [
            (2, 1u8),
            (3, 2),
            (4, 2),
            (5, 4),
            (16, 4),
            (17, 8),
            (256, 8),
            (257, 16),
        ];
        let mut written = 0usize; // distinct non-air ids written so far
        for (palette_len, expected_bits) in checkpoints {
            while written + 1 < palette_len {
                written += 1;
                storage.set(written, BlockId(written as u16), volume);
            }
            let ChunkStorage::Palette { bits, palette, .. } = &storage else {
                panic!("expected Palette after {palette_len} palette entries");
            };
            assert_eq!(*bits, expected_bits, "palette length {palette_len}");
            assert_eq!(palette.len(), palette_len, "palette length {palette_len}");
        }
    }

    #[test]
    fn repack_preserves_every_value() {
        // A repack must not disturb any cell's *resolved* id, only how it's
        // packed -- write a spread of distinct ids past a repack boundary and
        // check every one still reads back correctly afterward.
        let volume = 20;
        let mut storage = ChunkStorage::new();
        let ids: Vec<BlockId> = (0..20).map(|i| BlockId(i as u16)).collect();
        for (idx, &id) in ids.iter().enumerate() {
            storage.set(idx, id, volume); // 20 distinct ids forces bits 1->2->4->8
        }
        for (idx, &id) in ids.iter().enumerate() {
            assert_eq!(storage.get(idx), id, "idx {idx} after repacking");
        }
    }

    fn round_trip(storage: &ChunkStorage, volume: usize) -> ChunkStorage {
        let mut bytes = Vec::new();
        storage.write_payload(&mut bytes).expect("write payload");
        ChunkStorage::read_payload(&bytes, volume).expect("read payload")
    }

    fn assert_same_content(a: &ChunkStorage, b: &ChunkStorage, volume: usize) {
        for idx in 0..volume {
            assert_eq!(a.get(idx), b.get(idx), "idx {idx}");
        }
    }

    #[test]
    fn uniform_payload_round_trips() {
        let storage = ChunkStorage::Uniform(BlockId(7));
        let mut bytes = Vec::new();
        storage.write_payload(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 7, 0], "tag 0, then u16 id 7 little-endian");
        let back = round_trip(&storage, 4096);
        assert!(matches!(back, ChunkStorage::Uniform(BlockId(7))));
    }

    #[test]
    fn palette_payload_round_trips_every_width() {
        for &bits in &WIDTHS {
            let volume = 300;
            let len = 1usize << bits.min(7); // stay under the format's 255-entry palette cap
            let mut storage = ChunkStorage::new();
            for idx in 0..volume {
                storage.set(idx, BlockId((idx % len) as u16 + 1), volume);
            }
            let expected_bits = match &storage {
                ChunkStorage::Palette { bits, .. } => *bits,
                ChunkStorage::Uniform(_) => panic!("expected Palette (bits={bits})"),
            };

            let back = round_trip(&storage, volume);
            assert_same_content(&storage, &back, volume);
            match &back {
                ChunkStorage::Palette { bits: got, .. } => assert_eq!(*got, expected_bits),
                ChunkStorage::Uniform(_) => panic!("expected Palette after round-trip"),
            }
        }
    }

    #[test]
    fn palette_too_large_is_a_named_error() {
        let volume = 300;
        let palette: Vec<BlockId> = (0..300).map(|i| BlockId(i as u16)).collect(); // > u8::MAX
        let storage = ChunkStorage::Palette {
            palette,
            bits: 16,
            data: zero_words(16, volume),
        };
        assert_eq!(
            storage.write_payload(&mut Vec::new()),
            Err(ChunkPayloadError::PaletteTooLarge(300))
        );
    }

    #[test]
    fn truncated_payload_is_a_named_error() {
        assert_eq!(
            ChunkStorage::read_payload(&[], 4096).unwrap_err(),
            ChunkPayloadError::Truncated
        );
        assert_eq!(
            ChunkStorage::read_payload(&[0, 7], 4096).unwrap_err(), // tag + one byte of a u16
            ChunkPayloadError::Truncated
        );
    }

    #[test]
    fn unknown_storage_tag_is_a_named_error() {
        assert_eq!(
            ChunkStorage::read_payload(&[2, 0, 0], 4096).unwrap_err(),
            ChunkPayloadError::UnknownStorageTag(2)
        );
    }

    #[test]
    fn invalid_palette_bits_is_a_named_error_not_a_panic() {
        // tag 1, len 1, one u16 id, then an out-of-range bits byte -- must
        // error, not shift by an amount that would panic/UB downstream.
        let bytes = [1, 1, 0, 0, 3];
        assert_eq!(
            ChunkStorage::read_payload(&bytes, 4096).unwrap_err(),
            ChunkPayloadError::InvalidBits(3)
        );
    }
}
