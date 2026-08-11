//! World generation and streaming policy.
//!
//! [`World`] is a stateless source of [`Chunk`](cubara_voxel::Chunk)s generated on
//! demand from seeded noise terrain ([`WorldGen`]); [`streaming`] decides which
//! chunks should be resident around the camera. [`region`] is save/load's
//! on-disk half (block 1.9, `docs/PHASE1_ARCHITECTURE.md` §7) -- the header
//! (seed/tick/RNG/player) lives in `cubara-sim` instead, since this crate
//! must never know about the player.

mod noise;
pub mod raycast;
pub mod region;
pub mod streaming;
mod world;
mod worldgen;

pub use raycast::{raycast, RayHit};
pub use world::World;
pub use worldgen::{TerrainBlocks, WorldGen, WORLDGEN_VERSION};
