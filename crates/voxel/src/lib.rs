//! Voxel data structures and CPU-side meshing.
//!
//! This crate is the lowest engine layer: a chunk of blocks ([`Chunk`]) and the
//! greedy mesher that turns it into an indexed triangle [`Mesh`] of [`Vertex`]es.
//! It knows nothing about the GPU beyond the vertex-buffer layout, so worldgen and
//! the renderer can share these types without depending on each other.

pub mod block;
pub mod bounds;
pub mod coord;
pub mod item;
pub mod mesh;
pub mod ore;
pub mod recipe;
pub mod registry;
pub mod smelt;
pub mod storage;
pub mod structure;
pub mod voxel;

pub use block::BlockId;
pub use bounds::{build_mesh_bounded, Aabb};
pub use coord::ChunkCoord;
pub use item::{
    ItemDef, ItemId, ItemRegistry, ItemRegistryError, ItemStack, ItemState, Rarity, StackError,
};
pub use mesh::{Face, Mesh, Vertex};
pub use ore::{OreDef, OreError, OreRegistry};
pub use recipe::{Recipe, RecipeBook, RecipeDef, RecipeError, RecipeOutputDef, MAX_GRID};
pub use registry::{
    BlockRegistry, DropRule, Faces, Interact, ItemDrop, Material, RegistryError, Shape,
};
pub use smelt::{SmeltBook, SmeltError, SmeltRecipe, SmeltRecipeDef};
pub use storage::{ChunkPayloadError, ChunkStorage};
pub use structure::{StructureDef, StructureError, StructureRegistry};
pub use voxel::{Chunk, MeshContext};
