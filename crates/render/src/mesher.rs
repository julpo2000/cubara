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

use cubara_voxel::{ChunkCoord, Mesh};
use cubara_world::World;

use crate::arena::build_chunk_mesh;
use crate::culling::Aabb;

/// A finished meshing job: the coord, the LOD `level` it was meshed at, and its
/// geometry — or `None` if the chunk was empty (still reported so the renderer marks
/// it resident and stops re-requesting).
pub struct BuiltChunk {
    pub coord: ChunkCoord,
    pub level: u32,
    pub geometry: Option<(Mesh, Aabb)>,
}

/// Generate + mesh the chunk at `coord` at LOD `level`, as the synchronous path would.
fn mesh_coord(coord: ChunkCoord, level: u32) -> Option<(Mesh, Aabb)> {
    World::chunk_at(coord).and_then(|chunk| build_chunk_mesh(coord, &chunk, level))
}

/// A pool of worker threads that mesh chunks off the main thread.
///
/// Tracks the LOD level each in-flight coord was last requested at, so a coord is
/// never requested twice at the same level, and a result whose level no longer
/// matches (the chunk was unloaded, or its LOD changed as the camera moved) is
/// dropped by [`poll`](Self::poll) instead of uploaded.
pub struct MeshPool {
    job_tx: Sender<(ChunkCoord, u32)>,
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
        let (job_tx, job_rx) = std::sync::mpsc::channel::<(ChunkCoord, u32)>();
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
                        let (coord, level) = {
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
                            geometry: mesh_coord(coord, level),
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

    /// Queue `coord` for meshing at LOD `level`, unless that exact (coord, level) is
    /// already in flight. Requesting a coord already in flight at a *different* level
    /// supersedes it — the stale result is dropped on arrival.
    pub fn request(&mut self, coord: ChunkCoord, level: u32) {
        if self.in_flight.get(&coord) != Some(&level) {
            self.in_flight.insert(coord, level);
            // Send can only fail if all workers died; nothing useful to do if so.
            let _ = self.job_tx.send((coord, level));
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
    use cubara_world::streaming;
    use std::collections::HashMap;

    #[test]
    fn pool_results_match_synchronous_meshing() {
        // Meshing on workers must produce exactly what the synchronous path does,
        // for every requested coord (including empty chunks, reported as None), at
        // the requested LOD level.
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 1, 0..=2);
        let mut pool = MeshPool::with_workers(3);
        for (i, &c) in coords.iter().enumerate() {
            pool.request(c, (i % 3) as u32); // a mix of levels 0, 1, 2
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
            let expect = mesh_coord(c, (i % 3) as u32).map(|(m, _)| m.triangle_count());
            assert_eq!(got.get(&c).copied().flatten(), expect, "mismatch at {c:?}");
        }
    }

    #[test]
    fn cancelled_coords_are_dropped_by_poll() {
        let mut pool = MeshPool::with_workers(1);
        let c = ChunkCoord::new(0, 0, 0);
        pool.request(c, 0);
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
        let mut pool = MeshPool::with_workers(1);
        let c = ChunkCoord::new(0, 0, 0);
        pool.request(c, 0);
        pool.request(c, 2);
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
}
