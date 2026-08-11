//! Terrain generation + player edits.
//!
//! The base world is a deterministic function of position, so any chunk is
//! generated on demand from its [`ChunkCoord`] alone. On top of that sits a small
//! **edit overlay** — blocks the player has placed or broken — so
//! [`World::chunk_at`] returns terrain *plus* edits.
//!
//! A [`World`] is a plain value that owns its overlay (`ARCHITECTURE.md` Rule 2):
//! there is no global, so a process can hold several worlds and tests are
//! independent of each other. Meshing workers read a world through an
//! [`Arc`](std::sync::Arc) snapshot rather than reaching for shared mutable state
//! — see [`crate::World::set_block`] on how edits publish a new snapshot.

use std::collections::BTreeMap;

use cubara_voxel::{BlockId, Chunk, ChunkCoord};

use crate::worldgen::{TerrainBlocks, WorldGen};

/// The seed [`World::new`] uses -- a fixed constant, so callers that don't
/// need a *specific* seed (most tests, the benches, and the live game, which
/// has no "new game" seed picker yet) still get fully deterministic terrain
/// without threading a seed through call sites that have no business
/// choosing one. [`World::with_seed`] is the escape hatch for the ones that
/// do -- a future new-game flow, save/load (§7.2's `seed` header field), and
/// tests that want a specific, named seed.
#[allow(clippy::unusual_byte_groupings)] // grouped to spell "seed" / "coffee", not by nibble
const DEFAULT_SEED: u64 = 0x5EED_0000_C0FF_EE;

/// Deterministic terrain source, overlaid with player [edits](World::set_block).
///
/// Cloning is how an edit publishes a new snapshot to readers; the overlay is the
/// only owned state, so a clone costs one map copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct World {
    worldgen: WorldGen,
    /// World block coord → solid?, overriding terrain.
    ///
    /// `BTreeMap`, not `HashMap`: iteration order is part of the world's
    /// observable state once this is saved or hashed, and `ARCHITECTURE.md` Rule 1
    /// forbids results that depend on unordered iteration.
    edits: BTreeMap<[i32; 3], bool>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// An unedited world with the default seed (see [`DEFAULT_SEED`]).
    pub fn new() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }

    /// An unedited world with a specific seed.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            worldgen: WorldGen::new(seed),
            edits: BTreeMap::new(),
        }
    }

    /// The seed this world's terrain is generated from.
    pub fn seed(&self) -> u64 {
        self.worldgen.seed()
    }

    /// How many blocks the player has placed or broken. The only owned state, so
    /// this is also what a clone costs.
    pub fn edit_count(&self) -> usize {
        self.edits.len()
    }

    /// Whether the block at world coordinates `(x, y, z)` is solid — a player edit if
    /// one exists there, otherwise the terrain. Samples a single block without
    /// generating a whole chunk; the primitive [`raycast`](crate::raycast) queries
    /// the world through this.
    pub fn is_solid_at(&self, x: i32, y: i32, z: i32) -> bool {
        match self.edits.get(&[x, y, z]) {
            Some(&solid) => solid,
            None => self.worldgen.is_solid(x, y, z),
        }
    }

    /// Place (`solid = true`) or break (`false`) the block at world `(x, y, z)`,
    /// recording it in the edit overlay so future [`chunk_at`](Self::chunk_at) /
    /// [`is_solid_at`](Self::is_solid_at) reflect it. Returns the [`ChunkCoord`] that
    /// contains the block, so the caller can re-mesh it.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, solid: bool) -> ChunkCoord {
        self.edits.insert([x, y, z], solid);
        ChunkCoord::from_world_pos([x as f32, y as f32, z as f32])
    }

    /// Cast a ray through the world (terrain + edits) and return the first solid
    /// block hit (see [`raycast`](crate::raycast)) — the basis for targeting a block
    /// to break/place.
    pub fn raycast(&self, origin: [f32; 3], dir: [f32; 3], max_dist: f32) -> Option<crate::RayHit> {
        crate::raycast(origin, dir, max_dist, |b| {
            self.is_solid_at(b[0], b[1], b[2])
        })
    }

    /// Generate the chunk at `coord` from terrain overlaid with any player edits, or
    /// `None` if it ends up with no solid blocks. Unedited terrain comes from this
    /// world's [`WorldGen`] (seeded noise + caves, block 1.5), layered by depth
    /// below the surface into `blocks.grass`/`soil`/`stone` -- but the caller
    /// supplies which ids those are (resolved from its own loaded registry by
    /// name, e.g. `registry.id_of("cubara:grass")`) rather than this crate
    /// assuming fixed numbers. `BlockId`s are assigned by sorted name per
    /// registry (§3.4), so a hardcoded id here would silently mean a different
    /// material -- or a `Sided` one with an entirely different look -- the moment
    /// the loaded registry's material set changed; that exact mismatch (id 1
    /// meaning `cubara:grass`, not `cubara:stone`, in the real 3-material
    /// registry) is what block 1.4b's per-face resolution surfaced.
    pub fn chunk_at(&self, coord: ChunkCoord, blocks: TerrainBlocks) -> Option<Chunk> {
        let size = Chunk::SIZE as i32;
        let chunk = Chunk::from_fn(|lx, ly, lz| {
            let wx = coord.x * size + lx as i32;
            let wy = coord.y * size + ly as i32;
            let wz = coord.z * size + lz as i32;
            self.block_at(wx, wy, wz, blocks)
        });
        (!chunk.is_empty()).then_some(chunk)
    }

    /// The block at world `(x, y, z)`: a player edit if one exists there
    /// (always `blocks.stone` when placed -- edits don't carry a material
    /// choice yet), otherwise this world's [`WorldGen`] at that position.
    ///
    /// Kept as a per-cell method (not built from [`WorldGen::generate`],
    /// which knows nothing about edits) so `chunk_at`'s edit overlay stays a
    /// single per-cell lookup, matching how [`is_solid_at`](Self::is_solid_at)
    /// already does it -- not a second pass over `self.edits` per chunk.
    fn block_at(&self, x: i32, y: i32, z: i32, blocks: TerrainBlocks) -> BlockId {
        match self.edits.get(&[x, y, z]) {
            Some(&true) => blocks.stone,
            Some(&false) => BlockId::AIR,
            None => self
                .worldgen
                .block_at(x, y, z, blocks)
                .unwrap_or(BlockId::AIR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming;
    use cubara_voxel::{BlockRegistry, Faces, Material, MeshContext, Shape};

    /// A registry with a single solid material -- air (0) plus one other
    /// material sorts to id 1, matching `BlockId::STONE`, which is what
    /// `World::chunk_at` actually assigns today. This crate's tests only need
    /// solid-vs-air, not texturing, so `layer_of` is a constant stub -- the
    /// real registry (from RON) and texture layers (from the block registry's
    /// texture names) both live with the app, in `cubara-render`.
    fn test_registry() -> BlockRegistry {
        BlockRegistry::from_materials(vec![(
            std::path::PathBuf::from("test-fixture.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
            },
        )])
        .expect("fixture registry is valid")
    }

    fn zero_layer(_: &str) -> u32 {
        0
    }

    /// `TerrainBlocks` for tests that only care whether a chunk is empty or
    /// not, not which material it renders as.
    fn stone_blocks() -> TerrainBlocks {
        TerrainBlocks {
            grass: BlockId::STONE,
            soil: BlockId::STONE,
            stone: BlockId::STONE,
        }
    }

    fn test_ctx(registry: &BlockRegistry) -> MeshContext<'_> {
        MeshContext {
            registry,
            layer_of: &zero_layer,
        }
    }

    #[test]
    fn region_mesh_output_is_stable() {
        // Deterministic regression guard (runs in CI, no GPU): worldgen + greedy
        // meshing over a fixed region must keep producing exactly this many chunks
        // and triangles. A change here means terrain or the mesher changed — fine
        // if intended, but it should never move by accident.
        //
        // Triangle count jumped from 8,482 with per-vertex ambient occlusion (#45):
        // AO-varying cells can no longer merge into the same quad, so bumpy terrain
        // splits into more, smaller quads. Expected and accepted for the AO quality.
        //
        // Chunks/tris moved again, 52/14,716 -> 50/11,682 -> 50/13,510, with seeded
        // noise terrain (grass/soil/stone height field, plus caves) replacing the
        // fixed sin/cos rolling-hills formula (block 1.5 / #48): a different height
        // field at the same `World::new()` default seed changed which chunks in
        // this fixed 5x5x3 region have solid blocks at all (first move), and tuning
        // `CAVE_OCTAVES` down from 3 to 1 for generation speed (see BENCHMARKS.md)
        // changed the cave shape enough to move the triangle count again without
        // changing which chunks are non-empty (second move). Not a regression --
        // this test's job is catching an *accidental* change, and neither was.
        let world = World::new();
        let registry = test_registry();
        let ctx = test_ctx(&registry);
        // A single-material fixture, so `TerrainBlocks` deliberately points
        // grass/soil/stone at the *same* id -- this test pins geometry, not
        // the depth rule (that's `terrain_layers_by_depth` below), and using
        // one id keeps the depth boundaries from introducing new unmerged
        // quads that would move the triangle count for the wrong reason.
        let stone = registry.id_of("cubara:stone").unwrap();
        let blocks = TerrainBlocks {
            grass: stone,
            soil: stone,
            stone,
        };
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 2, 0..=2);
        let mut chunks = 0usize;
        let mut tris = 0usize;
        for coord in coords {
            if let Some(chunk) = world.chunk_at(coord, blocks) {
                chunks += 1;
                tris += chunk.build_mesh(&ctx).triangle_count();
            }
        }
        assert_eq!((chunks, tris), (50, 13510));
    }

    #[test]
    fn edits_override_worldgen_regardless_of_underlying_terrain() {
        // World::block_at's edit branch matches the overlay first, before
        // ever consulting WorldGen -- so a placed/broken block's material is
        // exactly `blocks.stone`/air regardless of what the underlying
        // terrain there actually is. The depth rule itself (grass/soil/stone
        // by depth) is WorldGen's own contract, pinned in
        // `worldgen::tests::terrain_layers_by_depth`.
        let stone = BlockId(3);
        let other = BlockId(4);
        let blocks = TerrainBlocks {
            grass: other,
            soil: other,
            stone,
        };

        let mut world = World::new();
        world.set_block(0, 500, 0, true); // "place" -- far above any plausible surface
        assert_eq!(
            world.block_at(0, 500, 0, blocks),
            stone,
            "a placed block is always stone -- edits carry no material choice yet"
        );
        world.set_block(0, -500, 0, false); // "break" -- far below any plausible surface
        assert_eq!(
            world.block_at(0, -500, 0, blocks),
            BlockId::AIR,
            "a broken block is air regardless of what worldgen would say"
        );
    }

    #[test]
    fn raycast_down_hits_the_terrain_surface() {
        // Straight down from high above (0,0): the first solid block is the surface,
        // entered through its top face.
        let world = World::new();
        let hit = world
            .raycast([0.5, 200.0, 0.5], [0.0, -1.0, 0.0], 400.0)
            .expect("a downward ray must hit the ground");
        assert_eq!(hit.normal, [0, 1, 0], "entered through the top face");
        let [x, y, z] = hit.block;
        assert!(world.is_solid_at(x, y, z), "hit block is solid");
        assert!(!world.is_solid_at(x, y + 1, z), "the block above it is air");
    }

    #[test]
    fn raycast_up_from_underground_returns_the_containing_block() {
        // Starting inside the terrain reports that block immediately (distance 0).
        let world = World::new();
        let hit = world
            .raycast([0.5, 0.0, 0.5], [0.0, 1.0, 0.0], 100.0)
            .expect("underground origin is inside a solid block");
        assert_eq!(hit.distance, 0.0);
        assert_eq!(hit.block, [0, 0, 0]);
    }

    #[test]
    fn placing_and_breaking_override_terrain() {
        // Each test owns its world, so edits at the origin cannot leak into the
        // region regression test above. Under the old global overlay these
        // coordinates had to be pushed 500_000 blocks out to stay clear of it.
        let mut world = World::new();
        assert!(!world.is_solid_at(0, 120, 0));
        world.set_block(0, 120, 0, true);
        assert!(world.is_solid_at(0, 120, 0));

        assert!(world.is_solid_at(16, 0, 16));
        world.set_block(16, 0, 16, false);
        assert!(!world.is_solid_at(16, 0, 16));
    }

    #[test]
    fn set_block_returns_containing_chunk() {
        // Block (16, 33, -1) lives in chunk (1, 2, -1) (16-block chunks).
        let mut world = World::new();
        assert_eq!(world.set_block(16, 33, -1, true), ChunkCoord::new(1, 2, -1));
    }

    #[test]
    fn chunk_at_reflects_a_placed_block() {
        // A chunk high in the air is empty (None) until we place a block in it.
        let mut world = World::new();
        let cc = ChunkCoord::new(0, 8, 0); // blocks y 128..143
        assert!(
            world.chunk_at(cc, stone_blocks()).is_none(),
            "air chunk starts empty"
        );
        world.set_block(0, 130, 0, true);
        assert!(
            world.chunk_at(cc, stone_blocks()).is_some(),
            "placing a block makes the chunk non-empty"
        );
    }

    #[test]
    fn worlds_are_independent() {
        // The property the global overlay made impossible: two worlds in one
        // process, neither seeing the other's edits.
        let mut a = World::new();
        let b = World::new();
        a.set_block(0, 120, 0, true);
        assert!(a.is_solid_at(0, 120, 0));
        assert!(!b.is_solid_at(0, 120, 0), "b must not see a's edit");
    }

    #[test]
    fn edits_are_ordered_for_determinism() {
        // Rule 1: the overlay iterates in a fixed order regardless of insertion
        // order, so anything derived from it (a save, a state hash) is stable.
        let mut a = World::new();
        let mut b = World::new();
        for c in [[5, 1, 2], [0, 0, 0], [-3, 9, 4]] {
            a.set_block(c[0], c[1], c[2], true);
        }
        for c in [[-3, 9, 4], [5, 1, 2], [0, 0, 0]] {
            b.set_block(c[0], c[1], c[2], true);
        }
        let keys = |w: &World| w.edits.keys().copied().collect::<Vec<_>>();
        assert_eq!(keys(&a), keys(&b));
        assert_eq!(a, b, "same edits in any order produce equal worlds");
    }
}
