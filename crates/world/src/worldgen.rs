//! Seeded terrain + cave generation (`docs/PHASE1_ARCHITECTURE.md` §8).
//!
//! [`WorldGen`] replaces the old fixed rolling-hills formula with a seeded
//! noise height field, plus a second 3D noise field subtracted from it for
//! caves (§8.3). Every sample [`WorldGen::generate`] produces is a pure
//! function of `(seed, x, y, z)` alone -- no neighbouring chunk's state, no
//! generation order, no shared scratch (§8.1) -- which is what lets an
//! unedited chunk be regenerated instead of saved (§7.4) and a far LOD node
//! sample its own volume with no neighbours to ask (§6).

use cubara_voxel::{BlockId, BlockRegistry, Chunk, StructureRegistry};

use crate::noise::{fbm2, fbm3};

/// The three block ids unedited terrain is layered with by depth below the
/// surface -- resolved by the caller from its own loaded registry by name
/// (e.g. `registry.id_of("cubara:grass")`) rather than assumed as fixed
/// numbers. `BlockId`s are assigned by sorted name per registry (§3.4), so a
/// hardcoded id would silently mean a different material the moment the
/// loaded registry's material set changed -- exactly the bug block 1.4b
/// found and fixed in the old placeholder terrain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainBlocks {
    /// The exposed surface block of unedited terrain.
    pub grass: BlockId,
    /// The `SOIL_DEPTH` blocks directly beneath the surface.
    pub soil: BlockId,
    /// Everything deeper than that (including cave walls, which sit well
    /// past `SOIL_DEPTH`), and every player-placed block -- edits don't
    /// carry a material choice yet, since there's no inventory/build system
    /// to choose one from.
    pub stone: BlockId,
    /// The oak's trunk and canopy, and the shape it grows in. `None` when no
    /// structure data was supplied -- a world with no trees, which every test
    /// that predates block 2.3 still expects.
    pub oak: Option<Oak>,
}

/// The oak, resolved: ids instead of names, and the shape numbers from
/// `assets/structures/oak.ron`.
///
/// Shape lives in data (`REQUIREMENTS.md` #3); *placement* does not, and is
/// the structure pass below (`PHASE1_ARCHITECTURE.md` §8.4) -- an algorithm
/// with a declared radius, not a number to tune.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Oak {
    pub log: BlockId,
    pub leaves: BlockId,
    /// Only grows on this block, which keeps trees off stone and out of caves.
    pub grows_on: BlockId,
    /// Inclusive trunk height range.
    pub height: (i32, i32),
    /// How far the canopy reaches from the trunk, in blocks.
    pub canopy_radius: i32,
    /// One in `density` chunk columns carries a tree. An integer so the
    /// decision is bit-identical across platforms (§8.5).
    pub density: u32,
}

impl TerrainBlocks {
    /// Add the oak, resolved from `structures` and `registry` by name.
    ///
    /// A builder rather than a parameter on [`from_registry`](Self::from_registry)
    /// so that the many callers who do not care about trees -- tests pinning
    /// terrain shape, save round-trips, the determinism fixture -- keep a
    /// one-argument constructor and a treeless world. The callers that render
    /// or play the real world opt in.
    ///
    /// Returns `self` unchanged if the structure or any of its blocks are
    /// missing: a world with no oaks is a worse world, not a broken one, and
    /// panicking here would make a missing data file fatal at startup for
    /// something the game can do without.
    pub fn with_oak(mut self, structures: &StructureRegistry, registry: &BlockRegistry) -> Self {
        let Some(def) = structures.get("cubara:oak") else {
            return self;
        };
        let (Some(log), Some(leaves), Some(grows_on)) = (
            registry.id_of(&def.trunk.block),
            registry.id_of(&def.canopy.block),
            registry.id_of(&def.grows_on),
        ) else {
            // No logging: this crate has no logger dependency, and a
            // missing block is caught by the registry tests long before here.
            return self;
        };
        self.oak = Some(Oak {
            log,
            leaves,
            grows_on,
            height: def.trunk.height,
            canopy_radius: def.canopy.radius,
            density: def.density,
        });
        self
    }

    /// Resolve `cubara:grass`/`cubara:soil`/`cubara:stone` from `registry` by
    /// name -- the one place every real caller (the live renderer, the
    /// headless bench/screenshot paths) gets its `TerrainBlocks` from, so the
    /// "resolve by name, not a hardcoded id" rule lives in one spot instead
    /// of being repeated at each call site.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let id = |name: &str| {
            registry
                .id_of(name)
                .unwrap_or_else(|| panic!("assets/blocks must define {name}"))
        };
        Self {
            oak: None,
            grass: id("cubara:grass"),
            soil: id("cubara:soil"),
            stone: id("cubara:stone"),
        }
    }
}

/// How many blocks of soil separate the grass surface from stone beneath --
/// a simple, fixed depth rule (carried over from block 1.4c). Real layered
/// biomes with varying depth are out of phase 1's scope (§8's own "out of
/// scope" list).
const SOIL_DEPTH: i32 = 3;

// Terrain shape. Frequency is in noise-cycles-per-block; a smaller number is
// broader, gentler hills. Tuned by eye against the old rolling-hills formula
// (`terrain_height`, ~±11 blocks of relief over a ~20-30 block wavelength)
// so the visible scene doesn't change wildly from block 1.4's golden images.
const TERRAIN_FREQ: f32 = 0.02;
const TERRAIN_OCTAVES: u32 = 4;
const TERRAIN_LACUNARITY: f32 = 2.0;
const TERRAIN_GAIN: f32 = 0.5;
const TERRAIN_AMPLITUDE: f32 = 14.0;
const TERRAIN_BASE_HEIGHT: i32 = 24;

// Cave shape. `CAVE_FREQ` sets tunnel scale; `CAVE_THRESHOLD` sets how much
// of the noise field's ~[-1,1] range counts as "inside a tunnel" -- higher
// means rarer, narrower caves. `CAVE_SEED_MIX` keeps cave noise from being
// the exact same pattern as terrain noise reused at a different frequency.
const CAVE_FREQ: f32 = 0.045;
const CAVE_OCTAVES: u32 = 1;
const CAVE_LACUNARITY: f32 = 2.0;
const CAVE_GAIN: f32 = 0.5;
const CAVE_THRESHOLD: f32 = 0.6;
/// Large enough to flip `density` negative regardless of how far below the
/// surface a cell is -- `density`'s terrain term never approaches this
/// magnitude within any loaded region.
const CAVE_CARVE_AMOUNT: f32 = 1_000_000.0;
const CAVE_SEED_MIX: u64 = 0xD6E8_FEB8_6659_FD93;

/// Bumped whenever a change in this file could change already-generated
/// terrain's shape -- any of the constants above, `density_at`,
/// `surface_height`, or the noise functions themselves. Save/load (block
/// 1.9, §7.2) refuses to load a world whose header disagrees with this: an
/// old save's *unedited* chunks are regenerated on load (§7.4), so a
/// changed generator would silently reshape the world around the player's
/// edits if this weren't checked -- the failure mode §7.4 names directly.
pub const WORLDGEN_VERSION: u32 = 1;

/// Seeded terrain + cave generator. See the module docs and
/// `docs/PHASE1_ARCHITECTURE.md` §8 for the contract this must hold to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldGen {
    seed: u64,
}

/// One placed tree: where its trunk stands, and how tall it is.
///
/// Resolved from a hash of `(seed, chunk column)` alone, so it is a pure
/// function of the seed and never depends on what has been generated (§8.1).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlacedTree {
    x: i32,
    z: i32,
    base: i32,
    height: i32,
}

/// The at-most-nine trees that can reach one chunk column.
///
/// A fixed array rather than a `Vec`: this is built per chunk (and per voxel
/// on the single-block paths), and allocating there would put a heap call in
/// the middle of worldgen. Nine is a proven bound, not a guess -- see
/// [`WorldGen::trees_near`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TreeSet {
    trees: [Option<PlacedTree>; 9],
    len: usize,
}

impl TreeSet {
    fn new() -> Self {
        Self {
            trees: [None; 9],
            len: 0,
        }
    }

    fn push(&mut self, t: PlacedTree) {
        if self.len < self.trees.len() {
            self.trees[self.len] = Some(t);
            self.len += 1;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(test)]
    fn count_for_test(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &PlacedTree> {
        self.trees[..self.len].iter().flatten()
    }
}

impl WorldGen {
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The terrain height field in blocks: a fractal noise surface that
    /// [`density`](Self::density)'s solid/air split (before caves) is
    /// measured against. A pure function of `(seed, x, z)` alone (§8.1).
    fn surface_height(&self, x: i32, z: i32) -> i32 {
        let n = fbm2(
            self.seed,
            x as f32 * TERRAIN_FREQ,
            z as f32 * TERRAIN_FREQ,
            TERRAIN_OCTAVES,
            TERRAIN_LACUNARITY,
            TERRAIN_GAIN,
        );
        (TERRAIN_BASE_HEIGHT as f32 + n * TERRAIN_AMPLITUDE).round() as i32
    }

    /// [`density`](Self::density), given an already-known `surface_height(x,
    /// z)` -- the shared body every per-voxel query below funnels through,
    /// so the 2D height-field noise (expensive: [`TERRAIN_OCTAVES`] octaves
    /// of hashing) is computed by the caller once per column and passed in,
    /// not recomputed per voxel. See [`generate`](Self::generate)'s doc
    /// comment for why that matters.
    fn density_at(&self, x: i32, y: i32, z: i32, surface: i32) -> f32 {
        let terrain = (surface - y + 1) as f32;
        if terrain <= 0.0 {
            // Already air from the height field alone. `carved` below is
            // always >= 0 (it's either 0 or CAVE_CARVE_AMOUNT), so caves can
            // only ever *remove* solid mass, never add it -- this cell can't
            // become solid no matter what the cave noise says, so there's no
            // need to sample it. Skipping this is a real, load-bearing
            // optimization, not a micro one: roughly half of any generated
            // region's volume is above the surface, and the cave noise is by
            // far the most expensive part of this function (CAVE_OCTAVES
            // rounds of 3D value noise, 8 hashed corners each) -- see
            // `generate`'s doc comment for the regression this and the
            // surface-amortization above it together fix.
            return terrain;
        }
        let carve = fbm3(
            self.seed ^ CAVE_SEED_MIX,
            x as f32 * CAVE_FREQ,
            y as f32 * CAVE_FREQ,
            z as f32 * CAVE_FREQ,
            CAVE_OCTAVES,
            CAVE_LACUNARITY,
            CAVE_GAIN,
        );
        let carved = if carve > CAVE_THRESHOLD {
            CAVE_CARVE_AMOUNT
        } else {
            0.0
        };
        terrain - carved
    }

    /// Terrain density at a world position: positive means solid. The
    /// height field minus a threshold-gated 3D cave noise field (§8.3) -- a
    /// pure function of `(seed, x, y, z)` alone, so no chunk this feeds
    /// depends on any other chunk, generated in any order (§8.1). Caves are
    /// noise, not carved tunnels, specifically so a cave crossing a chunk
    /// boundary agrees with itself on both sides for free.
    ///
    /// `+ 1` on the height-field term: `surface_height` names the *last
    /// solid row*, so at `y == surface_height` density must already be
    /// positive (solid), not exactly zero -- zero is reserved for the first
    /// *air* row, one above the surface. Without the offset "positive means
    /// solid" and "the surface block is solid" contradict each other by
    /// exactly one block.
    pub fn density(&self, x: i32, y: i32, z: i32) -> f32 {
        self.density_at(x, y, z, self.surface_height(x, z))
    }

    /// Shorthand for `density(x, y, z) > 0.0` -- what a single-block
    /// solidity query (a raycast step, [`World::is_solid_at`]) needs,
    /// without pulling in [`TerrainBlocks`] just to ask a yes/no question.
    ///
    /// [`World::is_solid_at`]: crate::World::is_solid_at
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.density(x, y, z) > 0.0
    }

    /// The material a solid position should render as, given an
    /// already-known `surface_height(x, z)` (see [`density_at`](Self::density_at)):
    /// `blocks.grass` at the surface, `blocks.soil` for the next
    /// [`SOIL_DEPTH`] blocks, `blocks.stone` deeper than that -- including
    /// cave walls, which sit far enough below the surface height that this
    /// never mistakes one for topsoil (`SOIL_DEPTH` is shallow, caves are
    /// not near the surface except at the rare mouth where a tunnel
    /// breaches it, and a breach is exactly where grass at a cave wall
    /// would be correct anyway).
    fn material_at(&self, y: i32, surface: i32, blocks: TerrainBlocks) -> BlockId {
        let depth = surface - y;
        if depth <= 0 {
            blocks.grass
        } else if depth <= SOIL_DEPTH {
            blocks.soil
        } else {
            blocks.stone
        }
    }

    /// The trees whose volume can reach anything in this chunk column: at most
    /// one per neighbouring chunk column, so at most nine.
    ///
    /// **Why this takes a chunk column rather than a voxel.** Which trees can
    /// reach a voxel depends only on its chunk, not on the voxel, so this is
    /// computed *once per chunk* by `World::build_chunk` and once per query by
    /// the single-voxel paths. One rule, hoisted differently -- a naive
    /// per-voxel version would cost nine hashes 8,192 times per chunk and blow
    /// `radius_64_smoke`'s budget.
    ///
    /// Nine is the bound because a tree reaches at most `canopy_radius` blocks
    /// sideways, and one chunk is 16 -- so only immediately adjacent chunk
    /// columns can ever overlap this one, and each carries at most one tree.
    pub(crate) fn trees_near(&self, chunk_x: i32, chunk_z: i32, blocks: TerrainBlocks) -> TreeSet {
        let mut out = TreeSet::new();
        if blocks.oak.is_none() {
            return out;
        }
        for cz in chunk_z - 1..=chunk_z + 1 {
            for cx in chunk_x - 1..=chunk_x + 1 {
                if let Some(t) = self.tree_in_column(cx, cz, blocks) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// The tree in one chunk column, if it has one.
    ///
    /// Integer arithmetic only: the placement decision has to produce the same
    /// bits on every platform (§8.5), and float comparisons are exactly what
    /// that rule exists to avoid.
    fn tree_in_column(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        blocks: TerrainBlocks,
    ) -> Option<PlacedTree> {
        let oak = blocks.oak?;
        // A separate hash stream from the terrain's, so retuning one cannot
        // silently move the other.
        let h = crate::noise::hash(self.seed ^ 0x7EE5, chunk_x, 0, chunk_z);
        if !h.is_multiple_of(oak.density as u64) {
            return None;
        }
        // Where in the column, kept clear of the chunk edge by the canopy
        // radius so a tree never reaches further than the one chunk of slack
        // `trees_near` allows for.
        let size = Chunk::SIZE as i32;
        let margin = oak.canopy_radius;
        let span = (size - margin * 2).max(1) as u64;
        let x = chunk_x * size + margin + ((h >> 8) % span) as i32;
        let z = chunk_z * size + margin + ((h >> 24) % span) as i32;

        let surface = self.surface_height(x, z);
        // Only on the block it grows on, which keeps trees off stone and out
        // of caves -- and only where that surface block is actually there,
        // since a cave can carve the ground out from under it.
        // The real surface material, from the real palette.
        //
        // An earlier version compared against a *synthetic* palette whose
        // `grass` was already `grows_on` -- which made the check vacuous: it
        // could only ever pass. `trees_do_not_grow_on_stone_or_in_caves`
        // caught it, which is exactly what that test is for.
        //
        // The density check is the second half: a cave can carve the ground
        // out from under a surface height, and a tree must not grow on air.
        if self.material_at(surface, surface, blocks) != oak.grows_on
            || self.density_at(x, surface, z, surface) <= 0.0
        {
            return None;
        }

        let (lo, hi) = oak.height;
        let height = lo + ((h >> 40) % (hi - lo + 1).max(1) as u64) as i32;
        Some(PlacedTree {
            x,
            z,
            base: surface + 1,
            height,
        })
    }

    /// Whether one of `trees` occupies `(x, y, z)`, and with what.
    ///
    /// Trunk wins over canopy where they overlap, so the trunk reads as a
    /// continuous column rather than being swallowed by its own leaves.
    pub(crate) fn tree_block_at(
        &self,
        trees: &TreeSet,
        x: i32,
        y: i32,
        z: i32,
        oak: Oak,
    ) -> Option<BlockId> {
        let mut leaves = false;
        for t in trees.iter() {
            let top = t.base + t.height - 1;
            if t.x == x && t.z == z && y >= t.base && y <= top {
                return Some(oak.log);
            }
            // A blob around the top of the trunk: within the canopy radius
            // horizontally, and from just below the top to just above it.
            let dx = (x - t.x).abs();
            let dz = (z - t.z).abs();
            let r = oak.canopy_radius;
            if dx <= r && dz <= r && y >= top - r && y <= top + 1 {
                // Trim the corners so the canopy reads as round rather than a
                // cube, and drop the ring at the very top so it comes to a
                // point.
                let shrink = if y >= top { 1 } else { 0 };
                if dx + dz <= r + 1 - shrink {
                    leaves = true;
                }
            }
        }
        leaves.then_some(oak.leaves)
    }

    /// The block at a world position given an already-known
    /// `surface_height(x, z)`, or `None` for air. What
    /// [`generate`](Self::generate) calls per lattice cell.
    fn block_at_on_surface(
        &self,
        x: i32,
        y: i32,
        z: i32,
        surface: i32,
        blocks: TerrainBlocks,
    ) -> Option<BlockId> {
        (self.density_at(x, y, z, surface) > 0.0).then(|| self.material_at(y, surface, blocks))
    }

    /// The block at a world position, or `None` for air. Exposed for
    /// callers that only need one block (a `World` cell with no edit
    /// overriding it) rather than a whole chunk; [`generate`](Self::generate)
    /// does not call this; see its doc comment for why.
    /// The terrain at a position, **without** consulting structures.
    ///
    /// What a caller uses when it has already resolved the trees for a whole
    /// chunk once (`World::build_chunk`) -- so the nine-hash structure lookup
    /// happens per chunk rather than per voxel. Getting that wrong is not a
    /// correctness bug, it is a 5x frame-time regression, which is how it was
    /// found.
    pub fn terrain_block_at(
        &self,
        x: i32,
        y: i32,
        z: i32,
        blocks: TerrainBlocks,
    ) -> Option<BlockId> {
        self.block_at_on_surface(x, y, z, self.surface_height(x, z), blocks)
    }

    pub fn block_at(&self, x: i32, y: i32, z: i32, blocks: TerrainBlocks) -> Option<BlockId> {
        if let Some(oak) = blocks.oak {
            let size = Chunk::SIZE as i32;
            let trees = self.trees_near(x.div_euclid(size), z.div_euclid(size), blocks);
            // Trees sit *on* the terrain, so they win where they overlap air
            // and lose to nothing -- a trunk is never inside solid ground,
            // because it starts one block above the surface.
            if let Some(block) = self.tree_block_at(&trees, x, y, z, oak) {
                return Some(block);
            }
        }
        self.block_at_on_surface(x, y, z, self.surface_height(x, z), blocks)
    }

    /// Whether `(x, y, z)` is solid **including trees**, given the ids that
    /// say what a tree is made of.
    ///
    /// Separate from [`is_solid`](Self::is_solid) because that one answers a
    /// yes/no question without needing a [`TerrainBlocks`], and most callers
    /// (the density field itself) genuinely do not want trees. Walking and
    /// raycasting do: you must not walk through a trunk you can see.
    pub fn is_solid_with_trees(&self, x: i32, y: i32, z: i32, blocks: TerrainBlocks) -> bool {
        if let Some(oak) = blocks.oak {
            let size = Chunk::SIZE as i32;
            let trees = self.trees_near(x.div_euclid(size), z.div_euclid(size), blocks);
            if !trees.is_empty() && self.tree_block_at(&trees, x, y, z, oak).is_some() {
                return true;
            }
        }
        self.is_solid(x, y, z)
    }

    /// Fill a `16³` lattice of samples, `step` world-blocks apart starting
    /// at `origin` -- `step = 1` for a full-resolution chunk, `step = 8` for
    /// a level-3 LOD node sampling the same fixed lattice across a 128³
    /// volume (§6.2's "every node is sampled on a fixed 16³ lattice").
    /// Nothing this returns depends on any other chunk having been
    /// generated first, in any order (§8.1) -- pinned by the isolation
    /// test, `tests::generation_is_isolated_from_neighbours` below.
    ///
    /// Returns a [`Chunk`], not the `ChunkStorage` §8.1's signature sketch
    /// names: `ChunkStorage::from_ids` is a deliberately crate-private fast
    /// path inside `cubara-voxel` (see its own doc comment), and
    /// `Chunk::from_fn` is the existing public entry point that already
    /// does exactly this -- no new hole needed in `cubara-voxel`'s
    /// encapsulation for it.
    ///
    /// Not yet called from the live streamed scene at a non-unit `step` --
    /// `World::chunk_at` always requests `step = 1` today, and
    /// `Chunk::build_mesh_lod` still downsamples an already-generated
    /// full-resolution chunk for distant LOD. Wiring far nodes to call this
    /// directly at their target `step` (skipping the full-resolution
    /// generation `step` exists to avoid) is block 1.10's region node tree,
    /// not this one -- this block's job is to make `step` correct and
    /// tested, ready for that to call.
    ///
    /// Precomputes `surface_height` once per `(x, z)` column (256 calls, not
    /// one per voxel) before sampling the lattice: the naive per-voxel
    /// version called it up to twice per voxel (once for density, once for
    /// material) -- 8,192 calls per chunk of the *same* 16 values, each one
    /// several octaves of hashing. That redundancy was the actual cause of
    /// a real regression the radius-64 smoke test caught (a debug-build
    /// generation pass that used to finish in ~1.3s no longer finished in
    /// the test's 120s budget) -- see `BENCHMARKS.md`.
    /// **No trees here, deliberately.** `PHASE1_ARCHITECTURE.md` §8.4:
    /// structures run at level 0 only, because "a tree sampled every 8 blocks
    /// is one stray voxel". This is the LOD path (`step > 1`), so growing trees
    /// in it would put single floating leaf blocks on the horizon. The two
    /// per-voxel routes -- `block_at` and `is_solid_with_trees` -- are the ones
    /// that do. They differ by design, and this comment is the design.
    pub fn generate(&self, origin: [i32; 3], step: i32, blocks: TerrainBlocks) -> Chunk {
        let mut surfaces = [[0i32; Chunk::SIZE]; Chunk::SIZE];
        for (lz, row) in surfaces.iter_mut().enumerate() {
            let z = origin[2] + lz as i32 * step;
            for (lx, cell) in row.iter_mut().enumerate() {
                let x = origin[0] + lx as i32 * step;
                *cell = self.surface_height(x, z);
            }
        }
        Chunk::from_fn(|lx, ly, lz| {
            let x = origin[0] + lx as i32 * step;
            let y = origin[1] + ly as i32 * step;
            let z = origin[2] + lz as i32 * step;
            self.block_at_on_surface(x, y, z, surfaces[lz][lx], blocks)
                .unwrap_or(BlockId::AIR)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_blocks() -> TerrainBlocks {
        TerrainBlocks {
            oak: None,
            grass: BlockId(1),
            soil: BlockId(2),
            stone: BlockId(3),
        }
    }

    fn chunks_equal(a: &Chunk, b: &Chunk) -> bool {
        for z in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for x in 0..Chunk::SIZE {
                    if a.get(x, y, z) != b.get(x, y, z) {
                        return false;
                    }
                }
            }
        }
        true
    }

    #[test]
    fn terrain_layers_by_depth() {
        // Block 1.4c's depth rule, carried over verbatim: grass at the
        // surface, soil for the next SOIL_DEPTH blocks, stone beyond that.
        // Tests `material_at` directly (not `block_at`/`generate`) so this
        // is exactly the depth mapping, with no cave noise able to
        // interfere by carving the probed cell to air.
        let gen = WorldGen::new(42);
        let blocks = test_blocks();
        let surface = gen.surface_height(0, 0);

        assert_eq!(
            gen.material_at(surface, surface, blocks),
            blocks.grass,
            "surface"
        );
        for depth in 1..=SOIL_DEPTH {
            assert_eq!(
                gen.material_at(surface - depth, surface, blocks),
                blocks.soil,
                "depth {depth}"
            );
        }
        assert_eq!(
            gen.material_at(surface - SOIL_DEPTH - 1, surface, blocks),
            blocks.stone,
            "below the soil layer"
        );
    }

    #[test]
    fn surface_is_solid_and_the_block_above_is_air() {
        // The density/is_solid boundary should land exactly where
        // surface_height says, not off by one in either direction.
        let gen = WorldGen::new(7);
        let surface = gen.surface_height(3, -5);
        assert!(gen.is_solid(3, surface, -5), "the surface block is solid");
        assert!(
            !gen.is_solid(3, surface + 1, -5),
            "one block above the surface is air"
        );
    }

    #[test]
    fn different_seeds_produce_different_terrain() {
        let a = WorldGen::new(1).surface_height(0, 0);
        let b = WorldGen::new(2).surface_height(0, 0);
        assert_ne!(
            a, b,
            "two different seeds landed on the same height by coincidence"
        );
    }

    /// A terrain palette with trees, for the structure-pass tests.
    fn tree_blocks() -> TerrainBlocks {
        TerrainBlocks {
            oak: Some(Oak {
                log: BlockId(10),
                leaves: BlockId(11),
                grows_on: BlockId(1),
                height: (4, 6),
                canopy_radius: 2,
                density: 4,
            }),
            grass: BlockId(1),
            soil: BlockId(2),
            stone: BlockId(3),
        }
    }

    /// Where this seed actually puts a trunk, found by looking rather than
    /// assumed: a hardcoded coordinate would silently stop testing anything
    /// the moment the terrain noise is retuned.
    fn find_a_trunk(gen: &WorldGen, blocks: TerrainBlocks) -> (i32, i32, i32) {
        let oak = blocks.oak.expect("this fixture has trees");
        for cz in -4..4 {
            for cx in -4..4 {
                let trees = gen.trees_near(cx, cz, blocks);
                for x in cx * 16..(cx + 1) * 16 {
                    for z in cz * 16..(cz + 1) * 16 {
                        let surface = gen.surface_height(x, z);
                        for y in surface..surface + 10 {
                            if gen.tree_block_at(&trees, x, y, z, oak) == Some(oak.log) {
                                return (x, y, z);
                            }
                        }
                    }
                }
            }
        }
        panic!("this seed grows no trees at all -- density is wrong");
    }

    #[test]
    fn trees_grow_and_are_solid() {
        // The walk-through-a-trunk bug. `is_solid` answers from the density
        // field, which knows nothing about trees, so the tree-aware query is
        // the one physics and raycasting have to use.
        let gen = WorldGen::new(0x77EE_5EED);
        let blocks = tree_blocks();
        let (x, y, z) = find_a_trunk(&gen, blocks);

        assert_eq!(
            gen.block_at(x, y, z, blocks),
            Some(blocks.oak.unwrap().log),
            "the trunk is there"
        );
        assert!(
            gen.is_solid_with_trees(x, y, z, blocks),
            "and you cannot walk through it"
        );
        assert!(
            !gen.is_solid(x, y, z),
            "while the raw density field still says air -- which is exactly why              the tree-aware query has to exist"
        );
    }

    #[test]
    fn the_same_seed_grows_the_same_trees() {
        let blocks = tree_blocks();
        let survey = |g: &WorldGen| {
            (-3..3)
                .flat_map(|cx| (-3..3).map(move |cz| (cx, cz)))
                .map(|(cx, cz)| g.trees_near(cx, cz, blocks).count_for_test())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            survey(&WorldGen::new(0x77EE_5EED)),
            survey(&WorldGen::new(0x77EE_5EED)),
            "the same seed grows the same trees"
        );
        assert_ne!(
            survey(&WorldGen::new(0x77EE_5EED)),
            survey(&WorldGen::new(0x77EE_5EEE)),
            "a different seed grows different ones -- otherwise the seed is not              reaching placement at all"
        );
    }

    #[test]
    fn trees_do_not_grow_on_stone_or_in_caves() {
        // `grows_on` is the whole rule. Point it at a block no surface ever
        // is, and nothing may grow anywhere.
        let gen = WorldGen::new(0x77EE_5EED);
        let mut blocks = tree_blocks();
        blocks.oak = Some(Oak {
            grows_on: BlockId(99),
            ..blocks.oak.unwrap()
        });
        for cz in -3..3 {
            for cx in -3..3 {
                assert!(
                    gen.trees_near(cx, cz, blocks).is_empty(),
                    "a tree grew on a surface it does not grow on, at chunk ({cx}, {cz})"
                );
            }
        }
    }

    #[test]
    fn lod_nodes_contain_no_tree_blocks() {
        // §8.4: structures run at level 0 only, because a tree sampled every
        // eight blocks is one stray voxel. `generate` is the LOD path.
        let gen = WorldGen::new(0x77EE_5EED);
        let blocks = tree_blocks();
        let oak = blocks.oak.unwrap();
        let node = gen.generate([0, 0, 0], 8, blocks);
        for z in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for x in 0..Chunk::SIZE {
                    let b = node.get(x, y, z);
                    assert_ne!(b, oak.log, "a log appeared in an LOD node");
                    assert_ne!(b, oak.leaves, "leaves appeared in an LOD node");
                }
            }
        }
    }

    #[test]
    fn generation_is_isolated_from_neighbours() {
        // §8.1's binding contract: generating a chunk reads nothing but the
        // seed and its own coordinate -- no neighbouring chunk's state, no
        // generation order, no shared scratch. Generate one chunk alone,
        // then generate it again after generating a shuffled handful of its
        // neighbours first: if `generate` ever reached for neighbour state,
        // this is exactly what would catch it.
        let gen = WorldGen::new(1234);
        let blocks = test_blocks();
        let origin = [32, 0, -16];

        let alone = gen.generate(origin, 1, blocks);

        // A fixed, deliberately-not-sorted handful of neighbouring chunk
        // origins (16 blocks apart, matching Chunk::SIZE).
        let neighbours = [
            [origin[0] + 16, origin[1], origin[2]],
            [origin[0], origin[1] + 16, origin[2] - 16],
            [origin[0] - 16, origin[1], origin[2] + 16],
            [origin[0], origin[1] - 16, origin[2]],
            [origin[0] + 16, origin[1], origin[2] + 16],
        ];
        for n in neighbours {
            let _ = gen.generate(n, 1, blocks);
        }

        let after_neighbours = gen.generate(origin, 1, blocks);
        assert!(
            chunks_equal(&alone, &after_neighbours),
            "chunk at {origin:?} changed after generating its neighbours -- \
             generation must not depend on what else was generated, or in \
             what order"
        );
    }

    #[test]
    fn step_samples_a_coarser_lattice_at_the_same_origin() {
        // Every local cell (lx, ly, lz) of a `step`-N chunk must match what
        // block_at says at world position origin + local*step directly --
        // generate() must not be generating full-resolution and
        // downsampling (the exact thing §2's "factor of 65" is about).
        let gen = WorldGen::new(99);
        let blocks = test_blocks();
        let origin = [0, 0, 0];
        let step = 4;
        let coarse = gen.generate(origin, step, blocks);

        for lz in 0..Chunk::SIZE {
            for ly in 0..Chunk::SIZE {
                for lx in 0..Chunk::SIZE {
                    let x = origin[0] + lx as i32 * step;
                    let y = origin[1] + ly as i32 * step;
                    let z = origin[2] + lz as i32 * step;
                    let expected = gen.block_at(x, y, z, blocks).unwrap_or(BlockId::AIR);
                    assert_eq!(coarse.get(lx, ly, lz), expected, "cell ({lx},{ly},{lz})");
                }
            }
        }
    }

    #[test]
    fn cross_platform_block_array_is_bit_identical() {
        // §8.5: noise is floating point, thresholded to a BlockId here. The
        // FNV-1a hash below folds every cell of a fixed chunk at a fixed
        // seed into one constant; both macOS and Windows CI run this test,
        // so a cross-platform float divergence in the noise shows up as a
        // CI failure instead of silently drifting into different worlds for
        // different players. If this ever fails from a genuine platform
        // difference, the recorded fallback is fixed-point noise (§8.5) --
        // if it fails because worldgen intentionally changed, recompute and
        // update the constant only after confirming both platforms agree on
        // the new value.
        let gen = WorldGen::new(0xC0FFEE);
        let blocks = test_blocks();
        let chunk = gen.generate([0, 0, 0], 1, blocks);

        let mut hash: u64 = 0xcbf29ce484222325;
        for z in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for x in 0..Chunk::SIZE {
                    for b in chunk.get(x, y, z).0.to_le_bytes() {
                        hash ^= b as u64;
                        hash = hash.wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x680f_9807_8f35_e325);
    }
}
