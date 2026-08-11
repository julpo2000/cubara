//! Voxel data structures and CPU-side meshing.
//!
//! This crate is the lowest engine layer: a chunk of blocks ([`Chunk`]) and the
//! greedy mesher that turns it into an indexed triangle [`Mesh`] of [`Vertex`]es.
//! It knows nothing about the GPU beyond the vertex-buffer layout, so worldgen and
//! the renderer can share these types without depending on each other.

pub mod block;
pub mod coord;
pub mod mesh;
pub mod registry;
pub mod storage;
pub mod voxel;

pub use block::BlockId;
pub use coord::ChunkCoord;
pub use mesh::{Face, Mesh, Vertex};
pub use registry::{BlockRegistry, Faces, Material, RegistryError, Shape};
pub use storage::{ChunkPayloadError, ChunkStorage};
pub use voxel::{Chunk, MeshContext};
