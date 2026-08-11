//! Background node meshing.
//!
//! Worldgen + greedy meshing is the CPU-heavy part of streaming a node in, and
//! doing it on the main thread means every streaming update stalls the frame
//! (a visible hitch). [`MeshPool`] moves that work onto a pool of worker threads:
//! the renderer *requests* a node, the workers generate + mesh it, and finished
//! [`BuiltNode`]s are drained each frame and uploaded to the GPU on the main thread
//! (the only step that must stay there). See issue #41 / `PLAN.md` §4.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cubara_voxel::{Mesh, MeshContext};
use cubara_world::node::NodeKey;
use cubara_world::{TerrainBlocks, World};

use crate::arena::build_node_mesh;
use crate::culling::Aabb;
use crate::materials::MeshAssets;

/// A finished meshing job: the node and its geometry — or `None` if it was empty
/// (still reported so the renderer marks it resident and stops re-requesting).
pub struct BuiltNode {
    pub node: NodeKey,
    pub geometry: Option<(Mesh, Aabb)>,
}

/// Put a batch of finished mesh jobs into a fixed order — ascending
/// `NodeKey` — before anything uploads them.
///
/// Worker threads finish jobs in whatever order the OS happens to schedule
/// them, which is not the same order every run, and [`ChunkArena`](crate::arena::ChunkArena)'s
/// suballocator is first-fit: whichever job is applied first claims the
/// earliest free slot. Left unsorted, the arena's slab layout — which node
/// ends up at which GPU offset — depended on thread timing instead of world
/// state (issue #83). Sorting first makes it depend on world state alone,
/// which is `ARCHITECTURE.md` Rule 1's requirement that a parallel step's
/// results are merged in a fixed order. `NodeKey`'s `Ord` is total and fixed
/// (level then pos) for exactly this reason.
pub fn sort_batch(mut batch: Vec<BuiltNode>) -> Vec<BuiltNode> {
    batch.sort_by_key(|b| b.node);
    batch
}

/// Generate + mesh `node`'s content, as the synchronous path would.
fn mesh_node(world: &World, assets: &MeshAssets, node: NodeKey) -> Option<(Mesh, Aabb)> {
    let layer_of = |name: &str| assets.layers.layer_of(name);
    let ctx = MeshContext {
        registry: &assets.registry,
        layer_of: &layer_of,
    };
    // TODO(#48): a simple fixed depth rule (block 1.4c) until real layered
    // terrain (block 1.5) -- see `TerrainBlocks`.
    let blocks = TerrainBlocks::from_registry(&assets.registry);
    world
        .node_at(node, blocks)
        .and_then(|chunk| build_node_mesh(node, &chunk, &ctx))
}

/// One meshing job: what to mesh, the world snapshot to mesh it from, and the
/// mesh assets (registry + texture layers) to resolve solidity and texturing
/// against.
///
/// The snapshot travels *with* the job rather than the workers reaching for shared
/// state (`ARCHITECTURE.md` Rule 2). An edit publishes a new [`Arc`] via
/// [`MeshPool::request`], so a job always meshes a consistent view and readers
/// never block a writer. The mesh assets are loaded once at startup and never
/// change, so sharing one `Arc` across every job is enough (no snapshot needed).
type Job = (Arc<World>, Arc<MeshAssets>, NodeKey);

/// A pool of worker threads that mesh nodes off the main thread.
///
/// Tracks which nodes are currently in flight, so a node is never requested
/// twice while its job is outstanding, and a result for a node no longer
/// wanted (unloaded while it was being meshed) is dropped by
/// [`poll`](Self::poll) instead of uploaded. Unlike the old per-chunk pool,
/// there is no separate "level" to supersede: a `NodeKey` already names both
/// a node's position *and* its detail level, so a change in desired detail is
/// a different key entirely, not a re-request of the same one.
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
                        let (world, assets, node) = {
                            let rx = jobs.lock().expect("mesher job lock");
                            match rx.recv() {
                                Ok(job) => job,
                                // All senders dropped (pool dropped) — exit.
                                Err(_) => break,
                            }
                        };
                        let built = BuiltNode {
                            node,
                            geometry: mesh_node(&world, &assets, node),
                        };
                        if results.send(built).is_err() {
                            break; // renderer gone
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

    /// Queue `node` for meshing against the `world` snapshot and `assets`,
    /// unless it's already in flight.
    ///
    /// The caller passes the world it wants meshed, so a job can never observe an
    /// edit that lands after it was queued.
    pub fn request(&mut self, world: &Arc<World>, assets: &Arc<MeshAssets>, node: NodeKey) {
        if self.in_flight.insert(node) {
            // Send can only fail if all workers died; nothing useful to do if so.
            let _ = self
                .job_tx
                .send((Arc::clone(world), Arc::clone(assets), node));
        }
    }

    /// Forget an in-flight node: the worker still finishes it, but its result will
    /// be dropped by [`poll`](Self::poll) instead of uploaded.
    pub fn cancel(&mut self, node: NodeKey) {
        self.in_flight.remove(&node);
    }

    /// Whether `node` is currently being meshed.
    pub fn is_in_flight(&self, node: NodeKey) -> bool {
        self.in_flight.contains(&node)
    }

    /// The nodes currently being meshed (so the renderer can unload ones that fell
    /// out of range before their mesh was ready).
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
    use crate::arena::ChunkArena;
    use cubara_voxel::{ChunkCoord, Faces, Material, Shape};
    use std::collections::HashMap;

    /// A registry with the three real material *names* -- `mesh_node` resolves
    /// `TerrainBlocks::from_registry` by name (block 1.4c), so a fixture missing
    /// any of them panics inside the worker thread and the test hangs waiting
    /// for a result that never arrives, rather than failing loudly. These tests
    /// only care about solid-vs-air and worker-pool mechanics (registry mechanics
    /// are tested in `cubara_voxel::registry`), so all three are plain `All`
    /// materials -- paired with an empty texture-layer map, since these tests
    /// don't care about texturing either and shouldn't need a GPU device just to
    /// build a real texture array.
    fn test_assets() -> Arc<MeshAssets> {
        let material = |name: &str| {
            (
                std::path::PathBuf::from("test-fixture.ron"),
                Material {
                    name: name.to_string(),
                    solid: true,
                    faces: Faces::All(name.to_string()),
                    shapes: vec![Shape::Full],
                },
            )
        };
        let registry = cubara_voxel::BlockRegistry::from_materials(vec![
            material("cubara:grass"),
            material("cubara:soil"),
            material("cubara:stone"),
        ])
        .expect("fixture registry is valid");
        Arc::new(MeshAssets {
            registry,
            layers: crate::materials::TextureLayers::empty(),
        })
    }

    #[test]
    fn pool_results_match_synchronous_meshing() {
        // Meshing on workers must produce exactly what the synchronous path does,
        // for every requested node (including empty ones, reported as None),
        // across a mix of levels.
        let world = Arc::new(World::new());
        let assets = test_assets();
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
            pool.request(&world, &assets, n);
        }

        let mut got: HashMap<NodeKey, Option<usize>> = HashMap::new();
        while !pool.in_flight().is_empty() {
            for built in pool.poll() {
                got.insert(built.node, built.geometry.map(|(m, _)| m.triangle_count()));
            }
            std::thread::yield_now();
        }

        assert_eq!(got.len(), nodes.len(), "every requested node returns once");
        for &n in &nodes {
            let expect = mesh_node(&world, &assets, n).map(|(m, _)| m.triangle_count());
            assert_eq!(got.get(&n).copied().flatten(), expect, "mismatch at {n:?}");
        }
    }

    #[test]
    fn cancelled_nodes_are_dropped_by_poll() {
        let world = Arc::new(World::new());
        let assets = test_assets();
        let mut pool = MeshPool::with_workers(1);
        let n = NodeKey::new(0, [0, 0, 0]);
        pool.request(&world, &assets, n);
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
        // Unlike the old per-chunk pool, a `NodeKey` already names both position
        // and detail level, so there is no "different level supersedes" case --
        // a different level is simply a different key. Re-requesting the same
        // key while it's in flight must stay a no-op.
        let world = Arc::new(World::new());
        let assets = test_assets();
        let mut pool = MeshPool::with_workers(1);
        let n = NodeKey::new(0, [0, 0, 0]);
        pool.request(&world, &assets, n);
        pool.request(&world, &assets, n);
        let mut results = Vec::new();
        while !pool.in_flight().is_empty() {
            results.extend(pool.poll());
            std::thread::yield_now();
        }
        assert_eq!(results.len(), 1, "must surface exactly once");
    }

    /// A headless device, or `None` on a CI runner with no GPU adapter — the same
    /// convention `crate::headless::render` uses, so this test skips loudly
    /// instead of failing where there is nothing to test against.
    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cubara-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()
    }

    #[test]
    fn sorted_batch_gives_the_same_arena_layout_regardless_of_arrival_order() {
        // Issue #83: ChunkArena::insert is first-fit, so whichever order a batch
        // of finished mesh jobs is *applied* in decides which node gets which
        // slab offset. Worker completion order is not the same every run. A unit
        // test over the allocator alone would not catch this -- the allocator is
        // already deterministic given its input order, the bug is the pipeline
        // feeding it that order -- so this drives the real pipeline pieces
        // (BuiltNode, build_node_mesh, ChunkArena::insert) through several
        // different arrival orders of the *same* batch and asserts sort_batch
        // makes all of them land on the identical layout.
        let Some((device, queue)) = test_device() else {
            eprintln!(
                "SKIP sorted_batch_gives_the_same_arena_layout_regardless_of_arrival_order: \
                 no GPU adapter"
            );
            return;
        };

        let world = World::new();
        let assets = test_assets();
        let schedule = [(0u32, 3i32)];
        let nodes = cubara_world::node::desired_nodes(ChunkCoord::new(0, 0, 0), 0..=2, &schedule);

        // Several stand-ins for different worker-scheduling outcomes: request
        // order, fully reversed, and a couple of arbitrary shuffles.
        let mut reversed = nodes.clone();
        reversed.reverse();
        let mut shuffled_a = nodes.clone();
        shuffled_a.sort_by_key(|n| (n.pos[0] * 7 + n.pos[2] * 13 + n.pos[1] * 31).rem_euclid(97));
        let mut shuffled_b = nodes.clone();
        shuffled_b.sort_by_key(|n| -(n.pos[0] * 5 + n.pos[2] * 11 + n.pos[1] * 17));
        let orderings = [nodes.clone(), reversed, shuffled_a, shuffled_b];

        let mut layouts = Vec::new();
        for order in &orderings {
            let batch: Vec<BuiltNode> = order
                .iter()
                .map(|&node| BuiltNode {
                    node,
                    geometry: mesh_node(&world, &assets, node),
                })
                .collect();

            let mut arena = ChunkArena::new(&device, false);
            for built in sort_batch(batch) {
                if let Some((mesh, aabb)) = built.geometry {
                    arena.insert(&queue, built.node, &mesh, aabb);
                }
            }
            layouts.push(arena.slot_offsets());
        }

        let reference = &layouts[0];
        assert!(
            reference.len() > 1,
            "the test region must produce more than one node of geometry to be meaningful"
        );
        for (i, layout) in layouts.iter().enumerate().skip(1) {
            assert_eq!(
                layout, reference,
                "arrival order {i} produced a different arena layout than sorted order 0 -- \
                 sort_batch did not make the layout arrival-order-independent"
            );
        }
    }
}
