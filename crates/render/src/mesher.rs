//! Background chunk meshing.
//!
//! Worldgen + greedy meshing is the CPU-heavy part of streaming a chunk in, and
//! doing it on the main thread means every chunk-boundary crossing stalls the frame
//! (a visible hitch). [`MeshPool`] moves that work onto a pool of worker threads:
//! the renderer *requests* a coord, the workers generate + mesh it, and finished
//! [`BuiltChunk`]s are drained each frame and uploaded to the GPU on the main thread
//! (the only step that must stay there). See issue #41 / `PLAN.md` §4.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cubara_voxel::{ChunkCoord, Mesh};
use cubara_world::World;

use crate::arena::build_chunk_mesh;
use crate::culling::Aabb;

/// A finished meshing job: the coord and its geometry, or `None` if the chunk was
/// empty (still reported so the renderer marks it resident and stops re-requesting).
pub struct BuiltChunk {
    pub coord: ChunkCoord,
    pub geometry: Option<(Mesh, Aabb)>,
}

/// Generate + mesh the chunk at `coord`, exactly as the synchronous path would.
fn mesh_coord(coord: ChunkCoord) -> Option<(Mesh, Aabb)> {
    World::chunk_at(coord).and_then(|chunk| build_chunk_mesh(coord, &chunk))
}

/// A pool of worker threads that mesh chunks off the main thread.
///
/// Tracks which coords are in flight so a coord is never requested twice, and so a
/// coord that gets unloaded again before its mesh is ready can be [`cancel`]led —
/// the worker still finishes it, but [`poll`](Self::poll) drops the stale result.
pub struct MeshPool {
    job_tx: Sender<ChunkCoord>,
    result_rx: Receiver<BuiltChunk>,
    in_flight: HashSet<ChunkCoord>,
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
        let (job_tx, job_rx) = std::sync::mpsc::channel::<ChunkCoord>();
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
                        let coord = {
                            let rx = jobs.lock().expect("mesher job lock");
                            match rx.recv() {
                                Ok(coord) => coord,
                                // All senders dropped (pool dropped) — exit.
                                Err(_) => break,
                            }
                        };
                        let built = BuiltChunk {
                            coord,
                            geometry: mesh_coord(coord),
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

    /// Queue `coord` for meshing, unless it is already in flight.
    pub fn request(&mut self, coord: ChunkCoord) {
        if self.in_flight.insert(coord) {
            // Send can only fail if all workers died; nothing useful to do if so.
            let _ = self.job_tx.send(coord);
        }
    }

    /// Forget an in-flight coord: the worker still finishes it, but its result will
    /// be dropped by [`poll`](Self::poll) instead of uploaded.
    pub fn cancel(&mut self, coord: ChunkCoord) {
        self.in_flight.remove(&coord);
    }

    /// The coords currently being meshed (so the renderer doesn't re-request them).
    pub fn in_flight(&self) -> &HashSet<ChunkCoord> {
        &self.in_flight
    }

    /// Take all finished results that are still wanted, clearing them from the
    /// in-flight set. Non-blocking.
    pub fn poll(&mut self) -> Vec<BuiltChunk> {
        let mut done = Vec::new();
        while let Ok(built) = self.result_rx.try_recv() {
            // `remove` is false if the coord was cancelled meanwhile → drop it.
            if self.in_flight.remove(&built.coord) {
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
        // for every requested coord (including empty chunks, reported as None).
        let coords = streaming::desired_chunks(ChunkCoord::new(0, 0, 0), 1, 0..=2);
        let mut pool = MeshPool::with_workers(3);
        for &c in &coords {
            pool.request(c);
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
        for &c in &coords {
            let expect = mesh_coord(c).map(|(m, _)| m.triangle_count());
            assert_eq!(got.get(&c).copied().flatten(), expect, "mismatch at {c:?}");
        }
    }

    #[test]
    fn cancelled_coords_are_dropped_by_poll() {
        let mut pool = MeshPool::with_workers(1);
        let c = ChunkCoord::new(0, 0, 0);
        pool.request(c);
        pool.cancel(c);
        // Give the worker time to finish and enqueue its (now unwanted) result.
        while !pool.in_flight().is_empty() {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(pool.poll().is_empty(), "cancelled result must not surface");
    }
}
