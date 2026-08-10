//! Background chunk meshing.
//!
//! Worldgen + greedy meshing is the CPU-heavy part of streaming a chunk in, and
//! doing it on the main thread means every chunk-boundary crossing stalls the frame
//! (a visible hitch). [`MeshPool`] moves that work onto a pool of worker threads:
//! the renderer *requests* a coord, the workers generate + mesh it, and finished
//! [`BuiltChunk`]s are drained each frame and uploaded to the GPU on the main thread
//! (the only step that must stay there). See issue #41 / `PLAN.md` §4.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cubara_voxel::{ChunkCoord, Mesh, MeshContext};
use cubara_world::World;

use crate::arena::build_chunk_mesh;
use crate::culling::Aabb;
use crate::materials::MeshAssets;

/// A finished meshing job: the coord, the LOD `level` it was meshed at, and its
/// geometry — or `None` if the chunk was empty (still reported so the renderer marks
/// it resident and stops re-requesting).
pub struct BuiltChunk {
    pub coord: ChunkCoord,
    pub level: u32,
    pub geometry: Option<(Mesh, Aabb)>,
}

/// Put a batch of finished mesh jobs into a fixed order — ascending
/// `ChunkCoord` — before anything uploads them.
///
/// Worker threads finish jobs in whatever order the OS happens to schedule
/// them, which is not the same order every run, and [`ChunkArena`](crate::arena::ChunkArena)'s
/// suballocator is first-fit: whichever job is applied first claims the
/// earliest free slot. Left unsorted, the arena's slab layout — which coord
/// ends up at which GPU offset — depended on thread timing instead of world
/// state (issue #83). Sorting first makes it depend on world state alone,
/// which is `ARCHITECTURE.md` Rule 1's requirement that a parallel step's
/// results are merged in a fixed order.
pub fn sort_batch(mut batch: Vec<BuiltChunk>) -> Vec<BuiltChunk> {
    batch.sort_by_key(|b| b.coord);
    batch
}

/// Generate + mesh the chunk at `coord` at LOD `level`, as the synchronous path would.
fn mesh_coord(
    world: &World,
    assets: &MeshAssets,
    coord: ChunkCoord,
    level: u32,
) -> Option<(Mesh, Aabb)> {
    let layer_of = |name: &str| assets.layers.layer_of(name);
    let ctx = MeshContext {
        registry: &assets.registry,
        layer_of: &layer_of,
    };
    // TODO(#48): every solid voxel is this one id until real terrain
    // materials (block 1.5) -- see `World::chunk_at`'s doc comment for why
    // it's resolved by name here rather than a hardcoded `BlockId`.
    let solid = assets
        .registry
        .id_of("cubara:stone")
        .expect("assets/blocks must define cubara:stone");
    world
        .chunk_at(coord, solid)
        .and_then(|chunk| build_chunk_mesh(coord, &chunk, &ctx, level))
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
type Job = (Arc<World>, Arc<MeshAssets>, ChunkCoord, u32);

/// A pool of worker threads that mesh chunks off the main thread.
///
/// Tracks the LOD level each in-flight coord was last requested at, so a coord is
/// never requested twice at the same level, and a result whose level no longer
/// matches (the chunk was unloaded, or its LOD changed as the camera moved) is
/// dropped by [`poll`](Self::poll) instead of uploaded.
pub struct MeshPool {
    job_tx: Sender<Job>,
    result_rx: Receiver<BuiltChunk>,
    in_flight: HashMap<ChunkCoord, u32>,
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
        let (result_tx, result_rx) = std::sync::mpsc::channel::<BuiltChunk>();
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
                        let (world, assets, coord, level) = {
                            let rx = jobs.lock().expect("mesher job lock");
                            match rx.recv() {
                                Ok(job) => job,
                                // All senders dropped (pool dropped) — exit.
                                Err(_) => break,
                            }
                        };
                        let built = BuiltChunk {
                            coord,
                            level,
                            geometry: mesh_coord(&world, &assets, coord, level),
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
            in_flight: HashMap::new(),
            _workers,
        }
    }

    /// Queue `coord` for meshing at LOD `level` against the `world` snapshot and
    /// `assets`, unless that exact (coord, level) is already in flight.
    /// Requesting a coord already in flight at a *different* level supersedes it
    /// — the stale result is dropped on arrival.
    ///
    /// The caller passes the world it wants meshed, so a job can never observe an
    /// edit that lands after it was queued.
    pub fn request(
        &mut self,
        world: &Arc<World>,
        assets: &Arc<MeshAssets>,
        coord: ChunkCoord,
        level: u32,
    ) {
        if self.in_flight.get(&coord) != Some(&level) {
            self.in_flight.insert(coord, level);
            // Send can only fail if all workers died; nothing useful to do if so.
            let _ = self
                .job_tx
                .send((Arc::clone(world), Arc::clone(assets), coord, level));
        }
    }

    /// Forget an in-flight coord: the worker still finishes it, but its result will
    /// be dropped by [`poll`](Self::poll) instead of uploaded.
    pub fn cancel(&mut self, coord: ChunkCoord) {
        self.in_flight.remove(&coord);
    }

    /// Whether `coord` is currently being meshed at exactly `level`.
    pub fn is_in_flight(&self, coord: ChunkCoord, level: u32) -> bool {
        self.in_flight.get(&coord) == Some(&level)
    }

    /// The coords currently being meshed (so the renderer can unload ones that fell
    /// out of range before their mesh was ready).
    pub fn in_flight(&self) -> &HashMap<ChunkCoord, u32> {
        &self.in_flight
    }

    /// Take all finished results that still match what's wanted (same coord *and*
    /// level), clearing them from the in-flight set. Non-blocking.
    pub fn poll(&mut self) -> Vec<BuiltChunk> {
        let mut done = Vec::new();
        while let Ok(built) = self.result_rx.try_recv() {
            // Keep only if this coord is still wanted at exactly this level.
            if self.in_flight.get(&built.coord) == Some(&built.level) {
                self.in_flight.remove(&built.coord);
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
    use cubara_voxel::{Faces, Material, Shape};
    use cubara_world::streaming;
    use std::collections::HashMap;

    /// A registry with a single solid material -- enough for meshing tests,
    /// which only care about solid-vs-air (registry mechanics are tested in
    /// `cubara_voxel::registry`) -- paired with an empty texture-layer map,
    /// since these tests don't care about texturing either and shouldn't need
    /// a GPU device just to build a real texture array.
    fn test_assets() -> Arc<MeshAssets> {
        let registry = cubara_voxel::BlockRegistry::from_materials(vec![(
            std::path::PathBuf::from("test-fixture.ron"),
            Material {
                name: "cubara:stone".to_string(),
                solid: true,
                faces: Faces::All("stone".to_string()),
                shapes: vec![Shape::Full],
            },
        )])
        .expect("fixture registry is valid");
        Arc::new(MeshAssets {
            registry,
            layers: crate::materials::TextureLayers::empty(),
        })
    }

    #[test]
    fn pool_results_match_synchronous_meshing() {
        // Meshing on workers must produce exactly what the synchronous path does,
        // for every requested coord (including empty chunks, reported as None), at
        // the requested LOD level.
        let world = Arc::new(World::new());
        let assets = test_assets();
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 1, 0..=2);
        let mut pool = MeshPool::with_workers(3);
        for (i, &c) in coords.iter().enumerate() {
            pool.request(&world, &assets, c, (i % 3) as u32); // a mix of levels 0, 1, 2
        }

        let mut got: HashMap<ChunkCoord, Option<usize>> = HashMap::new();
        while !pool.in_flight().is_empty() {
            for built in pool.poll() {
                got.insert(built.coord, built.geometry.map(|(m, _)| m.triangle_count()));
            }
            std::thread::yield_now();
        }

        assert_eq!(
            got.len(),
            coords.len(),
            "every requested coord returns once"
        );
        for (i, &c) in coords.iter().enumerate() {
            let expect =
                mesh_coord(&world, &assets, c, (i % 3) as u32).map(|(m, _)| m.triangle_count());
            assert_eq!(got.get(&c).copied().flatten(), expect, "mismatch at {c:?}");
        }
    }

    #[test]
    fn cancelled_coords_are_dropped_by_poll() {
        let world = Arc::new(World::new());
        let assets = test_assets();
        let mut pool = MeshPool::with_workers(1);
        let c = ChunkCoord::new(0, 0, 0);
        pool.request(&world, &assets, c, 0);
        pool.cancel(c);
        // Give the worker time to finish and enqueue its (now unwanted) result.
        while !pool.in_flight().is_empty() {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(pool.poll().is_empty(), "cancelled result must not surface");
    }

    #[test]
    fn superseded_level_result_is_dropped() {
        // Re-requesting a coord at a new level before draining supersedes the old
        // one: only the current level's mesh should ever surface.
        let world = Arc::new(World::new());
        let assets = test_assets();
        let mut pool = MeshPool::with_workers(1);
        let c = ChunkCoord::new(0, 0, 0);
        pool.request(&world, &assets, c, 0);
        pool.request(&world, &assets, c, 2);
        let mut levels = Vec::new();
        while !pool.in_flight().is_empty() {
            for built in pool.poll() {
                levels.push(built.level);
            }
            std::thread::yield_now();
        }
        assert!(
            levels.contains(&2),
            "the current (level 2) mesh must surface"
        );
        assert!(
            !levels.contains(&0),
            "the superseded (level 0) mesh must not"
        );
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
        // of finished mesh jobs is *applied* in decides which coord gets which
        // slab offset. Worker completion order is not the same every run. A unit
        // test over the allocator alone would not catch this -- the allocator is
        // already deterministic given its input order, the bug is the pipeline
        // feeding it that order -- so this drives the real pipeline pieces
        // (BuiltChunk, build_chunk_mesh, ChunkArena::insert) through several
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
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 3, 0..=2);

        // Several stand-ins for different worker-scheduling outcomes: request
        // order, fully reversed, and a couple of arbitrary shuffles.
        let mut reversed = coords.clone();
        reversed.reverse();
        let mut shuffled_a = coords.clone();
        shuffled_a.sort_by_key(|c| (c.x * 7 + c.z * 13 + c.y * 31).rem_euclid(97));
        let mut shuffled_b = coords.clone();
        shuffled_b.sort_by_key(|c| -(c.x * 5 + c.z * 11 + c.y * 17));
        let orderings = [coords.clone(), reversed, shuffled_a, shuffled_b];

        let mut layouts = Vec::new();
        for order in &orderings {
            let batch: Vec<BuiltChunk> = order
                .iter()
                .map(|&coord| BuiltChunk {
                    coord,
                    level: 0,
                    geometry: mesh_coord(&world, &assets, coord, 0),
                })
                .collect();

            let mut arena = ChunkArena::new(&device, false);
            for built in sort_batch(batch) {
                if let Some((mesh, aabb)) = built.geometry {
                    arena.insert(&queue, built.coord, &mesh, aabb);
                }
            }
            layouts.push(arena.slot_offsets());
        }

        let reference = &layouts[0];
        assert!(
            reference.len() > 1,
            "the test region must produce more than one chunk of geometry to be meaningful"
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
