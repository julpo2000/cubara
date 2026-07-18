//! Chunk coordinates.
//!
//! A [`ChunkCoord`] is the integer position of a chunk in the chunk grid. It is the
//! shared currency between world streaming ("which chunks should be loaded") and the
//! renderer ("which GPU buffers are resident"), so it lives in this bottom crate that
//! both depend on. Multiply by [`Chunk::SIZE`](crate::Chunk::SIZE) to get the
//! world-space block offset of a chunk's origin corner.

use crate::Chunk;

/// The integer grid position of a cubic chunk.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
}
