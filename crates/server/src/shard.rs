//! A set of chunks, and everything in them, detached from the server that held
//! it.
//!
//! Block 2.16, designed in `docs/PHASE2_MULTIPLAYER.md` §7 and enforced by
//! `ARCHITECTURE.md` Rule 8.
//!
//! # What a shard is
//!
//! Deliberately **an explicit set of chunk coordinates**, not a fixed cube. The
//! save format's 32×32×32 region is the wrong unit — 32,768 chunks is far more
//! than anything would keep active — and choosing a new constant here would be
//! inventing a number the measurements have not asked for yet. §8 leaves how
//! shards are sized open on purpose; this leaves it open too.
//!
//! # Why this can work at all
//!
//! §7.2's claim, and the reason this project can do something most cannot:
//!
//! > A shard's output is a **pure function of** `(seed, the region's state at
//! > tick N, the inputs applied between N and N+k)`.
//!
//! Every one of those is a value that can be serialised, and Rule 1 makes the
//! function deterministic. So a shard ticked somewhere else reaches exactly the
//! state it would have reached at home — not approximately, bit-for-bit — and
//! any other machine can replay the slice and compare.
//!
//! That is what makes [`Shard`] worth comparing with `==` rather than by hash.
//! A hash says two things differ; the value says *how*, which is what someone
//! debugging a handoff at three in the morning actually needs.
//!
//! # What this block does not do
//!
//! It does not move a shard between *machines* — that is 2.17, and it needs
//! this to work first. It does not decide who is allowed to run one, which §7.3
//! says is deployment policy rather than architecture. And it does not audit a
//! returned shard; §7.2's replay check is the mechanism, and building it before
//! there is a peer to distrust would be building a policy nobody has set.

use std::collections::BTreeSet;

use cubara_sim::{DroppedItem, EntityKey};
use cubara_voxel::{Chunk, ChunkCoord};
use cubara_world::Furnace;

use crate::Server;

/// One shard's whole simulable state.
///
/// Compared with `==` in tests, which is the strongest statement available:
/// two shards are equal when every block, every furnace and every dropped item
/// agrees.
#[derive(Debug, Clone, PartialEq)]
pub struct Shard {
    /// The chunks this shard is responsible for, in coordinate order.
    pub chunks: Vec<ChunkCoord>,
    /// The tick the state was taken at. Carried so a receiver can say how far
    /// behind it is, and so a handoff that arrives late is recognisable as
    /// late rather than as wrong.
    pub tick: u64,
    /// Every chunk's blocks, materialised. Terrain included: a receiver
    /// generates the same terrain from the same seed, but a shard that carried
    /// only the *edits* would be trusting the receiver to have the same
    /// worldgen version, and `WORLDGEN_VERSION` exists because that is not
    /// always true.
    pub blocks: Vec<(ChunkCoord, Chunk)>,
    /// Block entities inside the shard, in position order.
    pub block_entities: Vec<([i32; 3], Furnace)>,
    /// Dropped items inside the shard, in `EntityKey` order.
    pub entities: Vec<(EntityKey, DroppedItem)>,
}

impl Shard {
    /// Whether `pos` is inside this shard.
    pub fn contains_block(&self, pos: [i32; 3]) -> bool {
        self.chunks
            .contains(&ChunkCoord::from_block(pos[0], pos[1], pos[2]))
    }
}

impl Server {
    /// Keep `chunks` simulating regardless of where any player is (block 2.16).
    ///
    /// Until this existed, which chunks ticked was decided entirely by
    /// `update_simulation_radius` centred on a player — so a `Server` with no
    /// players simulated **nothing**, and a shard host is exactly that. Rule 8's
    /// point made concrete: a shard owns a region because it was given it, not
    /// because someone is standing in it.
    pub fn assign(&mut self, chunks: impl IntoIterator<Item = ChunkCoord>) {
        self.assigned = chunks.into_iter().collect();
        self.assigned_changed = true;
    }

    /// The chunks this server keeps simulating on its own account.
    pub fn assigned(&self) -> &BTreeSet<ChunkCoord> {
        &self.assigned
    }

    /// Take a copy of everything inside `chunks`.
    ///
    /// A copy, not a move: what the holder does with its own copy afterwards is
    /// the coordinator's business, and a function that silently emptied a
    /// server's world would be a hard thing to call twice.
    pub fn extract_shard(&self, chunks: &[ChunkCoord]) -> Shard {
        let mut chunks: Vec<ChunkCoord> = chunks.to_vec();
        chunks.sort();
        chunks.dedup();
        let inside: BTreeSet<ChunkCoord> = chunks.iter().copied().collect();
        let terrain = self.terrain();

        let blocks = chunks
            .iter()
            .map(|&c| (c, self.world.edited_chunk_at(c, terrain)))
            .collect();

        let mut block_entities: Vec<([i32; 3], Furnace)> = self
            .world
            .block_entities()
            .filter(|(pos, _)| inside.contains(&ChunkCoord::from_block(pos[0], pos[1], pos[2])))
            .map(|(pos, f)| (*pos, *f))
            .collect();
        block_entities.sort_by_key(|(pos, _)| *pos);

        let entities = self
            .sim
            .entities
            .sorted()
            .into_iter()
            .filter(|(_, d)| {
                let p = d.pos;
                inside.contains(&ChunkCoord::from_block(
                    p.x.floor_block(),
                    p.y.floor_block(),
                    p.z.floor_block(),
                ))
            })
            .collect();

        Shard {
            chunks,
            tick: self.sim.tick,
            blocks,
            block_entities,
            entities,
        }
    }

    /// Put a shard's state into this server, replacing whatever was there.
    ///
    /// Replacing, not merging: a handoff carries the whole of the shard's
    /// state, so anything this server still held for those chunks is by
    /// definition out of date. Merging would mean two authorities for one
    /// block, which is the bug sharding exists to avoid.
    pub fn install_shard(&mut self, shard: &Shard) {
        let terrain = self.terrain();
        let inside: BTreeSet<ChunkCoord> = shard.chunks.iter().copied().collect();
        let world = std::sync::Arc::make_mut(&mut self.world);

        for (coord, chunk) in &shard.blocks {
            world.load_chunk_edits(*coord, chunk, terrain);
        }

        // Block entities that were in these chunks and are not in the shard
        // have been broken while it was away.
        let stale: Vec<[i32; 3]> = world
            .block_entities()
            .map(|(pos, _)| *pos)
            .filter(|pos| inside.contains(&ChunkCoord::from_block(pos[0], pos[1], pos[2])))
            .filter(|pos| !shard.block_entities.iter().any(|(p, _)| p == pos))
            .collect();
        for pos in stale {
            world.remove_block_entity(pos);
        }
        for (pos, furnace) in &shard.block_entities {
            world.put_furnace(*pos, *furnace);
        }

        // Entities are keyed, so the same item coming back keeps its identity
        // and the hash does not move (§10.2 rule 3).
        let gone: Vec<EntityKey> = self
            .sim
            .entities
            .sorted()
            .into_iter()
            .filter(|(_, d)| {
                inside.contains(&ChunkCoord::from_block(
                    d.pos.x.floor_block(),
                    d.pos.y.floor_block(),
                    d.pos.z.floor_block(),
                ))
            })
            .map(|(key, _)| key)
            .filter(|key| !shard.entities.iter().any(|(k, _)| k == key))
            .collect();
        for key in gone {
            self.sim.entities.despawn(key);
        }
        for (key, item) in &shard.entities {
            self.sim.entities.restore_item(*key, *item);
        }
    }
}
