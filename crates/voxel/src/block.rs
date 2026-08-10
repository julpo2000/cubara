//! Block identity.
//!
//! A [`BlockId`] is a *runtime* index into the block registry -- stable for
//! the lifetime of one process, never persisted or compared across runs (the
//! registry that assigns these ids, and the rule that names rather than
//! numbers are identity, is block 1.3 / issue #54; see
//! `docs/PHASE1_ARCHITECTURE.md` §3.4). Id 0 is always air.

/// A runtime block-type index. 65,536 possible types; `AIR` (0) always means
/// "no block".
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BlockId(pub u16);

impl BlockId {
    /// Always "no block". Every chunk starts out entirely this id.
    pub const AIR: BlockId = BlockId(0);

    // TODO(#54): placeholder until the block registry exists. Every non-air
    // block in phase 1 is this one id; the registry (names as identity,
    // numbers assigned per world) replaces it, and per-face materials/textures
    // arrive in block 1.4.
    pub const STONE: BlockId = BlockId(1);
}
