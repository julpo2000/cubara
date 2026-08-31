//! World generation, streaming policy, and node meshing.
//!
//! [`World`] is a stateless source of [`Chunk`](cubara_voxel::Chunk)s generated on
//! demand from seeded noise terrain ([`WorldGen`]). [`region`] is save/load's
//! on-disk half (block 1.9, `docs/PHASE1_ARCHITECTURE.md` §7) -- the header
//! (seed/tick/RNG/player) lives in `cubara-sim` instead, since this crate
//! must never know about the player. [`node`] is the LOD region node tree's
//! addressing and streaming policy (block 1.10, §6); [`mesh`] turns a
//! [`node::NodeKey`] into world-space geometry, synchronously or on a worker
//! pool (block 1.10, sub-issue #110) -- this crate owns node meshing because
//! it is pure CPU work on chunk/node data, and a renderer's inputs should be
//! meshes, origins and a camera, nothing that knows what a `World` is (§1).
//! [`streaming`] is the older, chunk-only equivalent of [`node`]/[`mesh`],
//! kept only as the radius-64 baseline comparison (`BENCHMARKS.md`) -- no
//! production code streams through it anymore.

pub mod block_entity;
pub mod chunk_state;
pub mod mesh;
pub mod node;
mod noise;
pub mod raycast;
pub mod region;
pub mod streaming;
mod world;
mod worldgen;

pub use block_entity::{BlockEntities, Furnace, FurnaceOutcome};
pub use chunk_state::{ChunkState, ChunkStates, Woken};
pub use raycast::{raycast, RayHit};
pub use world::World;
pub use worldgen::{OreGen, OreSet, TerrainBlocks, WorldGen, MAX_ORES, WORLDGEN_VERSION};
