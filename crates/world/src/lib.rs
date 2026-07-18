//! World generation and chunk storage.
//!
//! Builds and holds the grid of [`Chunk`](cubara_voxel::Chunk)s that make up the
//! world. For now it's a small fixed grid with simple heightmap terrain; streaming
//! and persistence layer on top of this later.

mod world;

pub use world::{PlacedChunk, World};
