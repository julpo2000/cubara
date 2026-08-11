//! World generation and streaming policy.
//!
//! [`World`] is a stateless source of [`Chunk`](cubara_voxel::Chunk)s generated on
//! demand from seeded noise terrain ([`WorldGen`]); [`streaming`] decides which
//! chunks should be resident around the camera. There is no stored world grid yet —
//! persistence layers on top of this later.

mod noise;
pub mod raycast;
pub mod streaming;
mod world;
mod worldgen;

pub use raycast::{raycast, RayHit};
pub use world::World;
pub use worldgen::{TerrainBlocks, WorldGen};
