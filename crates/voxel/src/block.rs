//! Block identity.
//!
//! A [`BlockId`] is a *runtime* index into the block registry -- stable for
//! the lifetime of one process, never persisted or compared across runs (the
//! registry that assigns these ids, and the rule that names rather than
//! numbers are identity, is block 1.3 / issue #54; see
//! `docs/PHASE1_ARCHITECTURE.md` §3.4). Id 0 is always air.

/// A runtime block-type index. 65,536 possible types; `AIR` (0) always means
/// "no block".
/// `Default` is `AIR`, matching id 0 -- the only sensible "nothing yet" value,
/// and the one a `#[derive(Default)]` on a containing struct should get.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct BlockId(pub u16);

impl BlockId {
    /// Always "no block". Every chunk starts out entirely this id.
    pub const AIR: BlockId = BlockId(0);

    // A generically useful "some solid, non-air block" id for tests and
    // fixtures that build their own single-material (or matching) registry --
    // air is always 0, so a registry's one other material always lands here.
    // Not safe to assume against a real, multi-material registry: ids are
    // assigned by sorted name (§3.4), so "cubara:stone" is not id 1 in the
    // real `assets/blocks` registry (`cubara:grass` sorts first). Block
    // 1.4b's per-face resolution is what surfaced `World::chunk_at` wrongly
    // assuming otherwise; that call site now takes its solid id as a
    // parameter, resolved from the real registry by name, instead of this
    // constant.
    pub const STONE: BlockId = BlockId(1);
}
