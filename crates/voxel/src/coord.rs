//! Chunk coordinates.
//!
//! A [`ChunkCoord`] is the integer position of a chunk in the chunk grid. It is the
//! shared currency between world streaming ("which chunks should be loaded") and the
//! renderer ("which GPU buffers are resident"), so it lives in this bottom crate that
//! both depend on. Multiply by [`Chunk::SIZE`](crate::Chunk::SIZE) to get the
//! world-space block offset of a chunk's origin corner.

use crate::Chunk;

/// The integer grid position of a cubic chunk.
///
/// `Ord` is derived, giving lexicographic `(x, y, z)` ordering. The exact order is
/// arbitrary; what matters is that it is *total and stable*, so anything that
/// iterates chunks in coordinate order — the arena's draw list, a save file —
/// produces the same sequence every run (`ARCHITECTURE.md` Rule 1). See issue #81.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ChunkCoord {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl ChunkCoord {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// World-space block offset of this chunk's origin corner.
    pub fn world_offset(self) -> [f32; 3] {
        let size = Chunk::SIZE as f32;
        [
            self.x as f32 * size,
            self.y as f32 * size,
            self.z as f32 * size,
        ]
    }

    /// The chunk that contains a world-space position (inverse of
    /// [`world_offset`](Self::world_offset), rounding toward negative infinity so it
    /// stays correct for negative coordinates).
    pub fn from_world_pos(pos: [f32; 3]) -> Self {
        let size = Chunk::SIZE as f32;
        Self::new(
            (pos[0] / size).floor() as i32,
            (pos[1] / size).floor() as i32,
            (pos[2] / size).floor() as i32,
        )
    }

    /// The chunk that contains an integer world-block coordinate. Exact --
    /// `div_euclid` floors toward negative infinity for negative inputs the
    /// same way [`from_world_pos`](Self::from_world_pos) does, but without a
    /// float round-trip, which is what save/load's dirty-chunk bookkeeping
    /// (block 1.9) needs: an edit's coordinate is already an `i32`, and nothing
    /// about "which chunk owns this voxel" should ever depend on floating-point
    /// rounding.
    pub fn from_block(x: i32, y: i32, z: i32) -> Self {
        let size = Chunk::SIZE as i32;
        Self::new(x.div_euclid(size), y.div_euclid(size), z.div_euclid(size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_offset_scales_by_chunk_size() {
        assert_eq!(ChunkCoord::new(1, 2, 3).world_offset(), [16.0, 32.0, 48.0]);
        assert_eq!(ChunkCoord::new(0, 0, 0).world_offset(), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn from_world_pos_is_inverse_and_floors() {
        // Round-trips a chunk origin.
        assert_eq!(
            ChunkCoord::from_world_pos([16.0, 32.0, 48.0]),
            ChunkCoord::new(1, 2, 3)
        );
        // Any position inside a chunk maps to that chunk.
        assert_eq!(
            ChunkCoord::from_world_pos([31.9, 0.5, 0.0]),
            ChunkCoord::new(1, 0, 0)
        );
        // Negative positions floor toward -inf, not toward zero.
        assert_eq!(
            ChunkCoord::from_world_pos([-0.1, -16.0, -17.0]),
            ChunkCoord::new(-1, -1, -2)
        );
    }

    #[test]
    fn from_block_matches_from_world_pos_exactly() {
        // The integer and float paths must agree everywhere -- this is the
        // property that makes `from_block` a safe, exact substitute, not
        // just "close enough."
        for x in -40..40 {
            for z in [-33, -17, -1, 0, 1, 16, 31, 32] {
                let y = x * 3 - z;
                assert_eq!(
                    ChunkCoord::from_block(x, y, z),
                    ChunkCoord::from_world_pos([x as f32, y as f32, z as f32]),
                    "x={x} y={y} z={z}"
                );
            }
        }
    }

    #[test]
    fn from_block_floors_toward_negative_infinity() {
        assert_eq!(
            ChunkCoord::from_block(-1, -16, -17),
            ChunkCoord::new(-1, -1, -2)
        );
        assert_eq!(ChunkCoord::from_block(0, 15, 16), ChunkCoord::new(0, 0, 1));
    }
}
