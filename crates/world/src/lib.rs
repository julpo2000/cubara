//! World generation and streaming policy.
//!
//! [`World`] is a stateless source of [`Chunk`](cubara_voxel::Chunk)s generated on
//! demand from deterministic heightmap terrain; [`streaming`] decides which chunks
//! should be resident around the camera. There is no stored world grid yet —
//! persistence layers on top of this later.

pub mod raycast;
pub mod streaming;
mod world;

pub use raycast::{raycast, RayHit};
pub use world::World;
