//! A deterministic digest of world state (`ARCHITECTURE.md` Rule 1, issue #90).
//!
//! `ARCHITECTURE.md` names the intended enforcement directly: a replay test
//! that runs a fixed input script twice -- once single-threaded, once with
//! several worker threads -- and compares a hash of the resulting state.
//! [`WorldHash`] is that hash. It covers, in a fixed order, the tick number,
//! the RNG state, the player's state, and every chunk in an explicitly given
//! region's *contents* (not which chunks happen to be resident -- that's a
//! streaming/rendering concern, not part of world state).
//!
//! **Algorithm: FNV-1a**, written in-crate rather than reached for from
//! `std::hash::Hasher` -- `DefaultHasher`'s algorithm is explicitly not
//! stability-guaranteed across compiler versions, which would make a pinned
//! constant (below, and eventually a save file's own integrity check)
//! meaningless. Same reasoning as [`crate::WorldRng`]'s hand-written PCG32.
//! The constants match the 64-bit FNV-1a standard, and the encoding matches
//! the precedent already in `cubara_world::worldgen`'s own cross-platform
//! block-array hash test: `z`-outer/`y`-mid/`x`-inner voxel order, each
//! value's little-endian bytes fed in one at a time.

use cubara_voxel::{BlockId, Chunk, ChunkCoord, ItemState};
use cubara_world::{TerrainBlocks, World};

use crate::inventory::Inventory;
use crate::Sim;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// A running FNV-1a digest. `write_*` methods feed it in the fixed order
/// this module's own functions use; nothing here reads a `HashMap` or any
/// other unordered structure (Rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WorldHash(u64);

impl WorldHash {
    fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }

    fn write_u8(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }

    fn write_bool(&mut self, v: bool) {
        self.write_u8(v as u8);
    }

    fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_u16(&mut self, v: u16) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_f32(&mut self, v: f32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Fold an independently-computed digest into this one. What lets
    /// [`hash_region`] combine each chunk's own digest into a running total
    /// in a fixed, *position*-determined order -- called once per chunk, in
    /// ascending-`ChunkCoord` order, never in whatever order a worker
    /// thread happened to finish (the merge-order bug class issue #83
    /// fixed elsewhere; this is what keeps it from recurring here).
    fn write_hash(&mut self, other: WorldHash) {
        self.write_u64(other.0);
    }

    /// The raw 64-bit digest.
    pub fn value(&self) -> u64 {
        self.0
    }

    fn write_sim(&mut self, sim: &Sim) {
        self.write_u64(sim.tick);
        self.write_u64(sim.rng.state);
        self.write_u64(sim.rng.inc);
        let p = &sim.player;
        self.write_f32(p.pos.x);
        self.write_f32(p.pos.y);
        self.write_f32(p.pos.z);
        self.write_f32(p.velocity.x);
        self.write_f32(p.velocity.y);
        self.write_f32(p.velocity.z);
        self.write_bool(p.on_ground);
        self.write_bool(p.free_fly);
        self.write_f32(p.yaw);
        self.write_f32(p.pitch);
        self.write_inventory(&p.inventory);
    }

    /// Every slot in index order, present-or-not first so an empty slot is
    /// distinct from a slot holding zero of something (which cannot exist --
    /// `ItemStack` rejects a zero count -- but the encoding should not depend
    /// on that staying true).
    ///
    /// `ItemState` is written as a discriminant byte plus its payload rather
    /// than only the payload: without the byte, a stateless item and a tool at
    /// zero remaining would hash identically, and those are very different
    /// worlds.
    fn write_inventory(&mut self, inv: &Inventory) {
        self.write_u8(inv.selected_slot());
        for slot in inv.slots() {
            self.write_bool(slot.is_some());
            let Some(stack) = slot else { continue };
            self.write_u16(stack.item().0);
            self.write_u8(stack.count());
            match stack.state() {
                ItemState::None => self.write_u8(0),
                ItemState::Durability { remaining } => {
                    self.write_u8(1);
                    self.write_u16(remaining);
                }
            }
        }
    }

    /// One chunk's contents: its coordinate, then every voxel's [`BlockId`]
    /// in `z`-outer/`y`-mid/`x`-inner order (matching
    /// `cubara_world::worldgen`'s own cross-platform hash precedent). A
    /// chunk that generated to nothing (fully air) is written as absent --
    /// distinct from a present-but-different chunk at the same coordinate.
    fn write_chunk(&mut self, coord: ChunkCoord, chunk: Option<&Chunk>) {
        self.write_i32(coord.x);
        self.write_i32(coord.y);
        self.write_i32(coord.z);
        self.write_bool(chunk.is_some());
        let Some(chunk) = chunk else { return };
        for z in 0..Chunk::SIZE {
            for y in 0..Chunk::SIZE {
                for x in 0..Chunk::SIZE {
                    let BlockId(id) = chunk.get(x, y, z);
                    self.write_u16(id);
                }
            }
        }
    }

    /// The combined hash of `sim`'s own state and every chunk in `region`'s
    /// contents (generated fresh from `world` + `blocks`, not read from any
    /// residency/streaming state -- there is none here, deliberately: which
    /// chunks a renderer happens to have resident is not part of world
    /// state). `region` need not be pre-sorted; the fixed ascending-
    /// `ChunkCoord` order this method hashes in is this function's own
    /// guarantee, not a caller contract.
    ///
    /// `thread_count` workers split `region` into contiguous slices and
    /// each compute their chunks' digests independently, but every chunk's
    /// digest is folded into the total via [`Self::write_hash`] in the same
    /// ascending-`ChunkCoord` order regardless of how the work was split --
    /// so the result is the same whether `thread_count` is 1 or many,
    /// exactly what `world_hash_is_independent_of_worker_count` and the
    /// replay fixture's own single- vs multi-threaded test assert.
    pub fn compute(
        sim: &Sim,
        world: &World,
        region: &[ChunkCoord],
        blocks: TerrainBlocks,
        thread_count: usize,
    ) -> WorldHash {
        let mut total = WorldHash::new();
        total.write_sim(sim);
        total.write_hash(hash_region(world, region, blocks, thread_count));
        total
    }
}

/// See [`WorldHash::compute`]'s doc comment -- this is its chunk-hashing
/// half, split out because the fixture's own multi-threading test calls it
/// directly (it doesn't need `sim` at all, just `world` + a region).
pub fn hash_region(
    world: &World,
    region: &[ChunkCoord],
    blocks: TerrainBlocks,
    thread_count: usize,
) -> WorldHash {
    let mut region = region.to_vec();
    region.sort(); // the ascending-ChunkCoord order the whole hash promises

    let thread_count = thread_count.max(1);
    let slice_len = region.len().div_ceil(thread_count).max(1);

    // One fresh digest *per chunk*, not per slice: a chunk's own digest
    // must come out identical no matter which worker computed it or how
    // many chunks that worker was given, so `thread_count` only changes
    // how the work is distributed, never the values being folded together
    // below. (A per-*slice* digest -- one FNV-1a run continued across every
    // chunk a worker happens to own -- would NOT have this property: FNV-1a
    // isn't associative across arbitrarily-sized sub-ranges, so 1 slice of
    // N chunks and N slices of 1 chunk each would legitimately hash
    // differently. This was caught by `world_hash_is_independent_of_worker_count`
    // failing against an earlier, per-slice version of this function.)
    let per_chunk: Vec<WorldHash> = std::thread::scope(|scope| {
        region
            .chunks(slice_len)
            .map(|slice| {
                scope.spawn(move || {
                    slice
                        .iter()
                        .map(|&coord| {
                            let mut h = WorldHash::new();
                            h.write_chunk(coord, world.chunk_at(coord, blocks).as_ref());
                            h
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().expect("chunk-hash worker thread panicked"))
            .collect()
    });

    // Slices are contiguous, already-sorted ranges of `region`, and results
    // are collected by joining each spawned handle in spawn order (not
    // completion order) -- so `per_chunk` is in ascending-`ChunkCoord`
    // order regardless of `thread_count` or actual thread scheduling. This
    // fold is the one place a #83-style bug (folding in completion order
    // instead) could sneak back in.
    let mut total = WorldHash::new();
    for h in per_chunk {
        total.write_hash(h);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Player;

    fn blocks() -> TerrainBlocks {
        TerrainBlocks {
            grass: BlockId(1),
            soil: BlockId(2),
            stone: BlockId(3),
        }
    }

    fn region() -> Vec<ChunkCoord> {
        (-1..=1)
            .flat_map(|x| {
                (0..=1).flat_map(move |y| (-1..=1).map(move |z| ChunkCoord::new(x, y, z)))
            })
            .collect()
    }

    #[test]
    fn identical_sims_and_worlds_hash_equal() {
        let world = World::with_seed(1);
        let sim = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let a = WorldHash::compute(&sim, &world, &region(), blocks(), 1);
        let b = WorldHash::compute(&sim, &world, &region(), blocks(), 1);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_tick_count_changes_the_hash() {
        let world = World::with_seed(1);
        let mut a = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let b = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        a.tick += 1;
        assert_ne!(
            WorldHash::compute(&a, &world, &region(), blocks(), 1),
            WorldHash::compute(&b, &world, &region(), blocks(), 1),
        );
    }

    #[test]
    fn an_edit_changes_the_hash() {
        let sim = Sim::new(1, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let mut world = World::with_seed(1);
        // (0, 0, 0) is deep underground -- already solid stone -- so
        // *placing* there (`true`) would be a no-op on content; break it
        // instead, which unconditionally turns solid stone into air.
        assert!(
            world.is_solid_at(0, 0, 0),
            "test assumes this cell starts solid"
        );
        let before = WorldHash::compute(&sim, &world, &region(), blocks(), 1);
        world.set_block(0, 0, 0, false);
        let after = WorldHash::compute(&sim, &world, &region(), blocks(), 1);
        assert_ne!(before, after);
    }

    #[test]
    fn region_order_in_the_caller_does_not_matter() {
        let world = World::with_seed(2);
        let sim = Sim::new(2, Player::new(glam::Vec3::ZERO, 0.0, 0.0));
        let forward = region();
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            WorldHash::compute(&sim, &world, &forward, blocks(), 1),
            WorldHash::compute(&sim, &world, &reversed, blocks(), 1),
        );
    }

    #[test]
    fn world_hash_is_independent_of_worker_count() {
        let world = World::with_seed(3);
        let big_region: Vec<ChunkCoord> = (-3..=3)
            .flat_map(|x| {
                (0..=1).flat_map(move |y| (-3..=3).map(move |z| ChunkCoord::new(x, y, z)))
            })
            .collect();
        let one = hash_region(&world, &big_region, blocks(), 1);
        let many = hash_region(&world, &big_region, blocks(), 8);
        assert_eq!(
            one, many,
            "chunk-hash result must not depend on worker count"
        );
    }
}
