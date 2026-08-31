//! Node meshing — synchronous and backgrounded.
//!
//! Worldgen + greedy meshing is the CPU-heavy part of streaming a node in, and
//! doing it on the main thread means every streaming update stalls the frame (a
//! visible hitch). [`MeshPool`] moves that work onto a pool of worker threads:
//! the caller *requests* a node, the workers generate + mesh it, and finished
//! [`BuiltNode`]s are drained each frame and handed to a renderer (the only step
//! that needs the GPU, and therefore the only step that lives outside this
//! crate). [`mesh_region`] is the synchronous equivalent for callers that build
//! a whole scene in one shot and don't need a worker pool (headless bench,
//! screenshot, golden-image tests).
//!
//! This lives in `cubara-world`, not `cubara-render`, because it is pure CPU
//! work on chunk/node data (`docs/PHASE1_ARCHITECTURE.md` §1) — the renderer's
//! inputs are meshes, origins and a camera, nothing that knows what a `World`
//! or a `NodeKey` is. See issue #38's tracking arc, sub-issue #110.

use std::collections::HashSet;
use std::ops::RangeInclusive;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cubara_voxel::{build_mesh_bounded, Aabb, BlockRegistry, ChunkCoord, Mesh, MeshContext};

use crate::node::{desired_nodes, NodeKey, RingSchedule};
use crate::{TerrainBlocks, World};

/// A meshed node's geometry, in world space and ready to hand to a renderer:
/// the triangle mesh (still node-local, §5.2 -- placing it is a GPU-side
/// per-node origin add), its world-space bounds for frustum culling, and the
/// origin/scale pair a vertex shader needs to do that placement (§5.2/§5.3).
pub struct NodeGeometry {
    pub mesh: Mesh,
    pub aabb: Aabb,
    pub origin: [f32; 3],
    pub scale: f32,
}

/// A finished meshing job: the node and its geometry — or `None` if it was empty
/// (still reported so the caller marks it resident and stops re-requesting).
pub struct BuiltNode {
    pub node: NodeKey,
    pub geometry: Option<NodeGeometry>,
}

/// Put a batch of finished mesh jobs into a fixed order — ascending
/// `NodeKey` — before anything uploads them.
///
/// Worker threads finish jobs in whatever order the OS happens to schedule
/// them, which is not the same order every run, and a GPU arena's suballocator
/// is typically first-fit: whichever job is applied first claims the earliest
/// free slot. Left unsorted, the arena's slab layout — which node ends up at
/// which GPU offset — would depend on thread timing instead of world state
/// (issue #83). Sorting first makes it depend on world state alone, which is
/// `ARCHITECTURE.md` Rule 1's requirement that a parallel step's results are
/// merged in a fixed order. `NodeKey`'s `Ord` is total and fixed (level then
/// pos) for exactly this reason.
pub fn sort_batch(mut batch: Vec<BuiltNode>) -> Vec<BuiltNode> {
    batch.sort_by_key(|b| b.node);
    batch
}

/// Generate + mesh `node`'s content against `world`, resolving textures via
/// `registry`/`layer_of` — the synchronous building block both the worker pool
/// ([`MeshPool`]) and the whole-region helper ([`mesh_region`]) share.
pub fn mesh_node(
    world: &World,
    registry: &BlockRegistry,
    layer_of: &dyn Fn(&str) -> u32,
    node: NodeKey,
    blocks: TerrainBlocks,
) -> Option<NodeGeometry> {
    let ctx = MeshContext { registry, layer_of };
    let chunk = world.node_at(node, blocks)?;
    let world_origin = node.world_origin();
    let origin = [
        world_origin[0] as f32,
        world_origin[1] as f32,
        world_origin[2] as f32,
    ];
    let scale = node.extent_chunks() as f32;
    let (mesh, aabb) = build_mesh_bounded(&chunk, &ctx, origin, scale)?;
    Some(NodeGeometry {
        mesh,
        aabb,
        origin,
        scale,
    })
}

/// Mesh every node [`desired_nodes`] wants for `schedule` around `center`,
/// synchronously, in ascending [`NodeKey`] order (matching [`sort_batch`]'s
/// ordering guarantee) — for callers that build a whole scene in one shot and
/// don't need a worker pool: the headless bench, screenshot, and golden-image
/// test paths.
pub fn mesh_region(
    world: &World,
    registry: &BlockRegistry,
    layer_of: &dyn Fn(&str) -> u32,
    center: ChunkCoord,
    y_range: RangeInclusive<i32>,
    schedule: &RingSchedule,
    blocks: TerrainBlocks,
) -> Vec<BuiltNode> {
    let mut nodes = desired_nodes(center, y_range, schedule);
    nodes.sort();
    nodes
        .into_iter()
        .map(|node| BuiltNode {
            node,
            geometry: mesh_node(world, registry, layer_of, node, blocks),
        })
        .collect()
}

/// One meshing job: what to mesh, the world snapshot to mesh it from, and the
/// registry + texture-layer resolver to resolve solidity and texturing
/// against.
///
/// The snapshot travels *with* the job rather than the workers reaching for
/// shared state (`ARCHITECTURE.md` Rule 2). An edit publishes a new [`Arc`] via
/// [`MeshPool::request`], so a job always meshes a consistent view and readers
/// never block a writer. `layer_of` is a callback rather than a concrete type
/// (mirroring `MeshContext`'s own convention) so this crate never has to name
/// whatever GPU-side type actually owns the texture array — that stays the
/// caller's business.
type Job = (
    Arc<World>,
    Arc<BlockRegistry>,
    Arc<dyn Fn(&str) -> u32 + Send + Sync>,
    NodeKey,
    // Resolved once by the caller and carried with the job, rather than each
    // worker re-deriving it from the registry per node -- which is what this
    // used to do, and which also had no way to know about structures.
    TerrainBlocks,
);

/// A pool of worker threads that mesh nodes off the main thread.
///
/// Tracks which nodes are currently in flight, so a node is never requested
/// twice while its job is outstanding, and a result for a node no longer
/// wanted (unloaded while it was being meshed) is dropped by
/// [`poll`](Self::poll) instead of surfaced. A `NodeKey` already names both a
/// node's position *and* its detail level, so a change in desired detail is a
/// different key entirely, not a re-request of the same one.
pub struct MeshPool {
    job_tx: Sender<Job>,
    result_rx: Receiver<BuiltNode>,
    in_flight: HashSet<NodeKey>,
    _workers: Vec<JoinHandle<()>>,
}

impl MeshPool {
    /// Spawn a pool sized to leave the main thread a core to itself.
    pub fn new() -> Self {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(1)
            .max(1);
        Self::with_workers(workers)
    }

    fn with_workers(workers: usize) -> Self {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (result_tx, result_rx) = std::sync::mpsc::channel::<BuiltNode>();
        // One receiver shared by all workers: each grabs the next job under the lock,
        // then releases it and meshes in parallel with the others.
        let job_rx = Arc::new(Mutex::new(job_rx));

        let _workers = (0..workers)
            .map(|_| {
                let jobs = Arc::clone(&job_rx);
                let results = result_tx.clone();
                std::thread::Builder::new()
                    .name("cubara-mesher".into())
                    .spawn(move || loop {
                        let (world, registry, layer_of, node, blocks) = {
                            let rx = jobs.lock().expect("mesher job lock");
                            match rx.recv() {
                                Ok(job) => job,
                                // All senders dropped (pool dropped) — exit.
                                Err(_) => break,
                            }
                        };
                        let built = BuiltNode {
                            node,
                            geometry: mesh_node(&world, &registry, &*layer_of, node, blocks),
                        };
                        if results.send(built).is_err() {
                            break; // caller gone
                        }
                    })
                    .expect("spawn mesher thread")
            })
            .collect();

        Self {
            job_tx,
            result_rx,
            in_flight: HashSet::new(),
            _workers,
        }
    }

    /// Queue `node` for meshing against the `world` snapshot, `registry` and
    /// `layer_of`, unless it's already in flight.
    ///
    /// The caller passes the world it wants meshed, so a job can never observe
    /// an edit that lands after it was queued.
    pub fn request(
        &mut self,
        world: &Arc<World>,
        registry: &Arc<BlockRegistry>,
        layer_of: &Arc<dyn Fn(&str) -> u32 + Send + Sync>,
        node: NodeKey,
        blocks: TerrainBlocks,
    ) {
        if self.in_flight.insert(node) {
            // Send can only fail if all workers died; nothing useful to do if so.
            let _ = self.job_tx.send((
                Arc::clone(world),
                Arc::clone(registry),
                Arc::clone(layer_of),
                node,
                blocks,
            ));
        }
    }

    /// Forget an in-flight node: the worker still finishes it, but its result
    /// will be dropped by [`poll`](Self::poll) instead of surfaced.
    pub fn cancel(&mut self, node: NodeKey) {
        self.in_flight.remove(&node);
    }

    /// Whether `node` is currently being meshed.
    pub fn is_in_flight(&self, node: NodeKey) -> bool {
        self.in_flight.contains(&node)
    }

    /// The nodes currently being meshed (so the caller can unload ones that
    /// fell out of range before their mesh was ready).
    pub fn in_flight(&self) -> &HashSet<NodeKey> {
        &self.in_flight
    }

    /// Take all finished results that are still wanted, clearing them from the
    /// in-flight set. Non-blocking.
    pub fn poll(&mut self) -> Vec<BuiltNode> {
        let mut done = Vec::new();
        while let Ok(built) = self.result_rx.try_recv() {
            if self.in_flight.remove(&built.node) {
                done.push(built);
            }
        }
        done
    }
}

impl Default for MeshPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_voxel::{DropRule, Faces, Material, Shape};
    use std::collections::HashMap;

    /// A registry with the three real material *names* -- `mesh_node` resolves
    /// `TerrainBlocks::from_registry` by name (block 1.4c), so a fixture missing
    /// any of them panics inside the worker thread and the test hangs waiting
    /// for a result that never arrives, rather than failing loudly. These tests
    /// only care about solid-vs-air and worker-pool mechanics (registry
    /// mechanics are tested in `cubara_voxel::registry`), so all three are plain
    /// `All` materials, and there's no real texture layer mapping since these
    /// tests don't care about texturing either.
    fn test_registry() -> Arc<BlockRegistry> {
        let material = |name: &str| {
            (
                std::path::PathBuf::from("test-fixture.ron"),
                Material {
                    name: name.to_string(),
                    solid: true,
                    faces: Faces::All(name.to_string()),
                    shapes: vec![Shape::Full],
                    drops: DropRule::SameName,
                    requires_tier: 0,
                    hardness: Some(1),
                },
            )
        };
        Arc::new(
            BlockRegistry::from_materials(vec![
                material("cubara:grass"),
                material("cubara:soil"),
                material("cubara:stone"),
            ])
            .expect("fixture registry is valid"),
        )
    }

    fn zero_layer() -> Arc<dyn Fn(&str) -> u32 + Send + Sync> {
        Arc::new(|_: &str| 0)
    }

    #[test]
    fn pool_results_match_synchronous_meshing() {
        // Meshing on workers must produce exactly what the synchronous path does,
        // for every requested node (including empty ones, reported as None),
        // across a mix of levels.
        let world = Arc::new(World::new());
        let registry = test_registry();
        let layer_of = zero_layer();
        let nodes = [
            NodeKey::new(0, [0, 0, 0]),
            NodeKey::new(0, [1, 0, 0]),
            NodeKey::new(0, [0, 1, 0]),
            NodeKey::new(1, [0, 0, 0]),
            NodeKey::new(2, [-1, 0, 0]),
            NodeKey::new(3, [0, 5, 0]), // high enough to plausibly be empty
        ];
        let mut pool = MeshPool::with_workers(3);
        for &n in &nodes {
            pool.request(
                &world,
                &registry,
                &layer_of,
                n,
                TerrainBlocks::from_registry(&registry),
            );
        }

        let mut got: HashMap<NodeKey, Option<usize>> = HashMap::new();
        while !pool.in_flight().is_empty() {
            for built in pool.poll() {
                got.insert(built.node, built.geometry.map(|g| g.mesh.triangle_count()));
            }
            std::thread::yield_now();
        }

        assert_eq!(got.len(), nodes.len(), "every requested node returns once");
        for &n in &nodes {
            let expect = mesh_node(
                &world,
                &registry,
                &*layer_of,
                n,
                TerrainBlocks::from_registry(&registry),
            )
            .map(|g| g.mesh.triangle_count());
            assert_eq!(got.get(&n).copied().flatten(), expect, "mismatch at {n:?}");
        }
    }

    #[test]
    fn cancelled_nodes_are_dropped_by_poll() {
        let world = Arc::new(World::new());
        let registry = test_registry();
        let layer_of = zero_layer();
        let mut pool = MeshPool::with_workers(1);
        let n = NodeKey::new(0, [0, 0, 0]);
        pool.request(
            &world,
            &registry,
            &layer_of,
            n,
            TerrainBlocks::from_registry(&registry),
        );
        pool.cancel(n);
        // Give the worker time to finish and enqueue its (now unwanted) result.
        while !pool.in_flight().is_empty() {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(pool.poll().is_empty(), "cancelled result must not surface");
    }

    #[test]
    fn requesting_an_in_flight_node_again_surfaces_exactly_once() {
        // A `NodeKey` already names both position and detail level, so there is
        // no "different level supersedes" case -- a different level is simply a
        // different key. Re-requesting the same key while it's in flight must
        // stay a no-op.
        let world = Arc::new(World::new());
        let registry = test_registry();
        let layer_of = zero_layer();
        let mut pool = MeshPool::with_workers(1);
        let n = NodeKey::new(0, [0, 0, 0]);
        pool.request(
            &world,
            &registry,
            &layer_of,
            n,
            TerrainBlocks::from_registry(&registry),
        );
        pool.request(
            &world,
            &registry,
            &layer_of,
            n,
            TerrainBlocks::from_registry(&registry),
        );
        let mut results = Vec::new();
        while !pool.in_flight().is_empty() {
            results.extend(pool.poll());
            std::thread::yield_now();
        }
        assert_eq!(results.len(), 1, "must surface exactly once");
    }

    #[test]
    fn mesh_region_returns_nodes_in_ascending_order() {
        let world = World::new();
        let registry = test_registry();
        let layer_of = zero_layer();
        let schedule = [(0u32, 2i32)];
        let built = mesh_region(
            &world,
            &registry,
            &*layer_of,
            ChunkCoord::new(0, 0, 0),
            0..=1,
            &schedule,
            TerrainBlocks::from_registry(&registry),
        );
        assert_eq!(built.len(), 5 * 5 * 2, "(2*2+1)^2 columns * 2 y layers");
        let mut sorted = built.iter().map(|b| b.node).collect::<Vec<_>>();
        sorted.sort();
        assert_eq!(
            built.iter().map(|b| b.node).collect::<Vec<_>>(),
            sorted,
            "already in ascending NodeKey order"
        );
    }
}
