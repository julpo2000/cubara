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
}
