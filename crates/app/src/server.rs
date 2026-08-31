//! The authoritative half: the world, the simulation, and everything that
//! decides what is true.
//!
//! # Why this is a separate type
//!
//! `docs/RESEARCH_MULTIPLAYER.md` §8. Multiplayer here is an authoritative
//! server (§3), and *"privé en public"* is one architecture in two deployments
//! (§3.3): private play runs the server in-process, public runs it standalone.
//! That means the client cannot own the world and edit it directly — which is
//! exactly what `Game` did, with twenty-four fields spanning both sides of a
//! seam that did not exist.
//!
//! This is that seam, drawn where §8.1 says it goes. It is deliberately drawn
//! **before** there is any networking: at one player and no netcode the seam
//! moves by changing which struct owns a field, and after netcode exists it does
//! not (§8.5).
//!
//! # What is not here yet
//!
//! Messages (§8.3) and a client-side replica world (§8.2). This step establishes
//! *ownership*; the client still calls the server directly rather than sending
//! it an `Action`. That is the next step, and it is much cheaper once the fields
//! are already on the right side.

use std::sync::Arc;

use cubara_sim::Sim;
use cubara_voxel::{BlockRegistry, ChunkCoord, ItemRegistry, RecipeBook, SmeltBook};
use cubara_world::{TerrainBlocks, World};

/// Everything the simulation is authoritative about.
///
/// The registries are here because the server decides what a block *means* —
/// what it drops, what tier it needs, how long it takes to break. A client
/// needs the same definitions to draw and to predict, and will be given them;
/// it does not get to disagree about them.
pub struct Server {
    /// The world being simulated. Behind an [`Arc`] so meshing jobs can carry
    /// the exact snapshot they were queued against; an edit publishes a new one.
    pub world: Arc<World>,
    pub sim: Sim,
    pub blocks_registry: Option<Arc<BlockRegistry>>,
    pub terrain: Option<TerrainBlocks>,
    pub items: Option<ItemRegistry>,
    pub recipes: Option<RecipeBook>,
    pub smelting: Option<SmeltBook>,
    /// The chunk the simulation radius was last updated around (§11). Which
    /// chunks tick is an authority question, so it lives here.
    pub sim_centre: Option<ChunkCoord>,
}

impl Server {
    /// Which ids the terrain is made of, or a treeless default before assets
    /// are set.
    ///
    /// Trees are solid, so physics and raycasting need this -- `is_solid_at`
    /// cannot answer from the density field alone any more. The fallback is a
    /// world with no trees rather than a panic: `Game::new()` runs before a
    /// window exists, and a headless test that never sets assets should still
    /// be able to walk around.
    pub fn terrain(&self) -> TerrainBlocks {
        self.terrain.unwrap_or(TerrainBlocks {
            oak: None,
            ores: cubara_world::OreSet::EMPTY,
            grass: cubara_voxel::BlockId::AIR,
            soil: cubara_voxel::BlockId::AIR,
            stone: cubara_voxel::BlockId::AIR,
        })
    }
}
