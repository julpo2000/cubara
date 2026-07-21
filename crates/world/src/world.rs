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

use cubara_voxel::{Chunk, ChunkCoord};

/// Deterministic terrain source, overlaid with player [edits](World::set_block).
///
/// Cloning is how an edit publishes a new snapshot to readers; the overlay is the
/// only owned state, so a clone costs one map copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    /// World block coord → solid?, overriding terrain.
    ///
    /// `BTreeMap`, not `HashMap`: iteration order is part of the world's
    /// observable state once this is saved or hashed, and `ARCHITECTURE.md` Rule 1
    /// forbids results that depend on unordered iteration.
    edits: BTreeMap<[i32; 3], bool>,
}

/// Deterministic rolling-hills height (in blocks) for a world column.
fn terrain_height(x: i32, z: i32) -> i32 {
    let fx = x as f32;
    let fz = z as f32;
    let h = 22.0
        + 7.0 * (fx * 0.045).sin() * (fz * 0.045).cos()
        + 4.0 * (fx * 0.11 + 1.7).sin()
        + 4.0 * (fz * 0.09 + 0.5).cos();
    h.round() as i32
}

impl World {
    /// An unedited world — pure terrain.
    pub fn new() -> Self {
        Self::default()
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
            None => y <= terrain_height(x, z),
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
    /// `None` if it ends up with no solid blocks.
    pub fn chunk_at(&self, coord: ChunkCoord) -> Option<Chunk> {
        let size = Chunk::SIZE as i32;
        let chunk = Chunk::from_solid_fn(|lx, ly, lz| {
            let wx = coord.x * size + lx as i32;
            let wy = coord.y * size + ly as i32;
            let wz = coord.z * size + lz as i32;
            self.is_solid_at(wx, wy, wz)
        });
        (!chunk.is_empty()).then_some(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming;

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
        let world = World::new();
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 2, 0..=2);
        let mut chunks = 0usize;
        let mut tris = 0usize;
        for coord in coords {
            if let Some(chunk) = world.chunk_at(coord) {
                chunks += 1;
                tris += chunk.build_mesh().triangle_count();
            }
        }
        assert_eq!((chunks, tris), (52, 14716));
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
        assert!(world.chunk_at(cc).is_none(), "air chunk starts empty");
        world.set_block(0, 130, 0, true);
        assert!(
            world.chunk_at(cc).is_some(),
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
