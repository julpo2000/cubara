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

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cubara_voxel::{
    build_mesh_bounded_occluded, Aabb, BlockRegistry, ChunkCoord, Mesh, MeshContext,
};

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
    let surfaces = std::cell::RefCell::new(HashMap::new());
    let covered = |gx, gy, gz| border_covered(world, node, &surfaces, [gx, gy, gz]);
    let (mesh, aabb) = build_mesh_bounded_occluded(&chunk, &ctx, origin, scale, covered)?;
    Some(NodeGeometry {
        mesh,
        aabb,
        origin,
        scale,
    })
}

/// Every chunk whose mesh can change when the block at `pos` changes: its own,
/// and -- since a chunk leaves out border faces its neighbour covers -- the
/// neighbour across each border `pos` sits on.
///
/// Without the neighbours, digging out a block at the edge of a chunk leaves
/// the next chunk still hiding the face that now looks into the hole.
pub fn chunks_affected_by_edit(pos: [i32; 3]) -> Vec<ChunkCoord> {
    let size = cubara_voxel::Chunk::SIZE as i32;
    let own = ChunkCoord::new(
        pos[0].div_euclid(size),
        pos[1].div_euclid(size),
        pos[2].div_euclid(size),
    );
    let mut out = vec![own];
    for axis in 0..3 {
        let local = pos[axis].rem_euclid(size);
        let step = match local {
            0 => -1,
            l if l == size - 1 => 1,
            _ => continue,
        };
        let mut c = [own.x, own.y, own.z];
        c[axis] += step;
        out.push(ChunkCoord::new(c[0], c[1], c[2]));
    }
    out
}

/// Whether a border face of `node` against its outside cell `g` -- grid
/// coordinates with exactly one axis at `-1` or `16` -- can be left out,
/// because whatever is drawn next to it covers it completely.
///
/// **The neighbour may be drawn at a different level of detail**, and which one
/// depends on where the camera is, which a node's mesh cannot know: it is built
/// once and kept while the camera moves. So a face is dropped only when the
/// outside is solid for *every* neighbour this node could have. Rings of
/// detail are nested one level apart, so that is three:
///
/// - **the same level** -- one cell, sampled where that node samples it;
/// - **one coarser** -- the double-size cell that contains it;
/// - **one finer** -- the four half-size cells that touch the face.
///
/// If any of those is air, some neighbour draws an opening there, and this
/// face is what a player looking through it would see -- so it stays. Drop it
/// and the result is a hole into the inside of the terrain.
///
/// **At the edge of the render distance the walls go too**: the outside is not
/// drawn, but it is solid ground. What used to be a grey cliff where the drawn
/// world stopped is now the terrain simply ending -- which is what it is.
fn border_covered(
    world: &World,
    node: NodeKey,
    surfaces: &std::cell::RefCell<HashMap<(i32, i32), i32>>,
    g: [i32; 3],
) -> bool {
    let level = node.level;
    let step = node.extent_chunks();
    let origin = node.world_origin();
    let p = [
        origin[0] + g[0] * step,
        origin[1] + g[1] * step,
        origin[2] + g[2] * step,
    ];
    // Surface heights are the expensive part of a sample, and one node's
    // border touches few columns, so each is computed once per node.
    let solid = |q: [i32; 3], at: u32| {
        let surface = *surfaces
            .borrow_mut()
            .entry((q[0], q[2]))
            .or_insert_with(|| world.surface_height(q[0], q[2]));
        world.certainly_solid_at(q[0], q[1], q[2], at, surface)
    };

    if !solid(p, level) {
        return false;
    }
    let coarse = 2 * step;
    let q = [
        p[0].div_euclid(coarse) * coarse,
        p[1].div_euclid(coarse) * coarse,
        p[2].div_euclid(coarse) * coarse,
    ];
    if !solid(q, level + 1) {
        return false;
    }
    if level == 0 {
        return true;
    }
    // The finer layer that touches this face: the near half of the outside
    // cell along the face's axis, and both halves across it.
    let half = step / 2;
    let axis = (0..3)
        .find(|&k| g[k] == -1 || g[k] == 16)
        .expect("a border cell");
    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut f = p;
    if g[axis] == -1 {
        f[axis] += half;
    }
    for da in [0, half] {
        for db in [0, half] {
            let mut r = f;
            r[a] += da;
            r[b] += db;
            if !solid(r, level - 1) {
                return false;
            }
        }
    }
    true
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
    mesh_nodes(
        world,
        registry,
        layer_of,
        desired_nodes(center, y_range, schedule),
        blocks,
    )
}

/// Mesh exactly `nodes`, synchronously, in ascending [`NodeKey`] order -- what
/// [`mesh_region`] does for its band, for a caller that chose the nodes some
/// other way (a 3D selection).
pub fn mesh_nodes(
    world: &World,
    registry: &BlockRegistry,
    layer_of: &dyn Fn(&str) -> u32,
    mut nodes: Vec<NodeKey>,
    blocks: TerrainBlocks,
) -> Vec<BuiltNode> {
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
    use cubara_voxel::{DropRule, Faces, Interact, Material, Shape};
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
                    interact: Interact::None,
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

    /// Whether `node`'s cell containing world point `p` is solid, as the node
    /// itself draws it. `chunks` caches generated nodes across calls.
    fn drawn_solid(
        world: &World,
        blocks: TerrainBlocks,
        chunks: &mut HashMap<NodeKey, Option<cubara_voxel::Chunk>>,
        node: NodeKey,
        p: [i32; 3],
    ) -> bool {
        let chunk = chunks
            .entry(node)
            .or_insert_with(|| world.node_at(node, blocks));
        let (o, s) = (node.world_origin(), node.extent_chunks());
        let i = |k: usize| ((p[k] - o[k]) / s) as usize;
        chunk
            .as_ref()
            .is_some_and(|c| c.get(i(0), i(1), i(2)) != cubara_voxel::BlockId::AIR)
    }

    /// Whether `node`'s mesh has a face on the plane `axis = at`, pointing
    /// `sign` along it, covering the unit square with min corner `p`.
    fn has_face(mesh: &Mesh, node: NodeKey, axis: usize, at: i32, sign: i8, p: [i32; 3]) -> bool {
        let (o, s) = (node.world_origin(), node.extent_chunks());
        let face = cubara_voxel::Face::from_axis_sign(axis, sign);
        let world = |v: cubara_voxel::Vertex| {
            [
                o[0] + v.x() as i32 * s,
                o[1] + v.y() as i32 * s,
                o[2] + v.z() as i32 * s,
            ]
        };
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        mesh.vertices.chunks(4).any(|q| {
            if q[0].face() != face || world(q[0])[axis] != at {
                return false;
            }
            let corners: Vec<[i32; 3]> = q.iter().map(|&v| world(v)).collect();
            let lo = |k: usize| corners.iter().map(|c| c[k]).min().unwrap();
            let hi = |k: usize| corners.iter().map(|c| c[k]).max().unwrap();
            (lo(a)..hi(a)).contains(&p[a]) && (lo(b)..hi(b)).contains(&p[b])
        })
    }

    /// **No holes where two nodes meet** -- the condition border covering must
    /// never break, at every level pairing the ring schedule produces.
    ///
    /// Walk the shared plane one block at a time. Wherever one side draws
    /// solid and the other air, a player in that air can look at the solid
    /// side, so the solid side must have a face there. If neither has one, the
    /// player sees through the terrain.
    #[test]
    fn where_two_nodes_meet_every_visible_solid_cell_has_a_face() {
        let registry = test_registry();
        let layer_of = |_: &str| 0;
        let blocks = TerrainBlocks::from_registry(&registry);
        let world = World::new();
        let mut meshes: HashMap<NodeKey, Option<Mesh>> = HashMap::new();
        let mut chunks = HashMap::new();
        let mut face_at = |n: NodeKey, axis: usize, at: i32, sign: i8, p: [i32; 3]| {
            meshes
                .entry(n)
                .or_insert_with(|| {
                    mesh_node(&world, &registry, &layer_of, n, blocks).map(|g| g.mesh)
                })
                .as_ref()
                .is_some_and(|m| has_face(m, n, axis, at, sign, p))
        };

        let mut checked = 0usize;
        let mut mismatched = 0usize;
        // Pairs (level of A, level of B), B beyond A along +axis. Level 0 has
        // caves and the coarser ones do not, so 0-1 is where the two sides
        // disagree the most.
        for (la, lb) in [
            (0u32, 0u32),
            (0, 1),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, 1),
            (2, 3),
            (3, 2),
        ] {
            let big = 1i32 << la.max(lb);
            for axis in [0usize, 2] {
                for col in 0..6i32 {
                    for layer in [-1i32, 0, 1] {
                        // A plane both levels' node grids share.
                        let mut plane_chunk = [0i32; 3];
                        plane_chunk[axis] = (col * 7 + 3) * big;
                        let other = if axis == 0 { 2 } else { 0 };
                        plane_chunk[other] = (col * 5 - 9) * big;
                        plane_chunk[1] = layer * big;
                        let a_node = NodeKey::containing(
                            ChunkCoord::new(
                                plane_chunk[0] - (axis == 0) as i32,
                                plane_chunk[1],
                                plane_chunk[2] - (axis == 2) as i32,
                            ),
                            la,
                        );
                        let plane = plane_chunk[axis] * cubara_voxel::Chunk::SIZE as i32;
                        let (ao, asz) = (a_node.world_origin(), 16 * a_node.extent_chunks());
                        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
                        for da in 0..asz {
                            for db in 0..asz {
                                let mut p = [0i32; 3];
                                p[a] = ao[a] + da;
                                p[b] = ao[b] + db;
                                // A's cell is just below the plane, B's at it.
                                p[axis] = plane - 1;
                                let a_solid = drawn_solid(&world, blocks, &mut chunks, a_node, p);
                                let mut pb = p;
                                pb[axis] = plane;
                                let b_node = NodeKey::containing(
                                    ChunkCoord::from_world_pos([
                                        pb[0] as f32,
                                        pb[1] as f32,
                                        pb[2] as f32,
                                    ]),
                                    lb,
                                );
                                let b_solid = drawn_solid(&world, blocks, &mut chunks, b_node, pb);
                                checked += 1;
                                if a_solid == b_solid {
                                    continue;
                                }
                                mismatched += 1;
                                let ok = if a_solid {
                                    face_at(a_node, axis, plane, 1, p)
                                } else {
                                    face_at(b_node, axis, plane, -1, pb)
                                };
                                assert!(
                                    ok,
                                    "a hole: levels {la}|{lb}, plane {axis}={plane}, block {p:?}, solid on the {} side has no face",
                                    if a_solid { "near" } else { "far" }
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(
            mismatched > 100,
            "only {mismatched} of {checked} cells differ: the test sees nothing"
        );
    }

    #[test]
    fn an_edit_dirties_its_chunk_and_the_neighbours_it_borders() {
        let c = ChunkCoord::new;
        assert_eq!(
            chunks_affected_by_edit([5, 5, 5]),
            vec![c(0, 0, 0)],
            "interior"
        );
        assert_eq!(
            chunks_affected_by_edit([0, 5, 15]),
            vec![c(0, 0, 0), c(-1, 0, 0), c(0, 0, 1)],
            "on two borders"
        );
        // Negative coordinates: block -1 is local 15 of chunk -1.
        assert_eq!(
            chunks_affected_by_edit([-1, -16, 7]),
            vec![c(-1, -1, 0), c(0, -1, 0), c(-1, -2, 0)]
        );
    }

    /// A block dug out right at a node's border opens a hole the neighbour
    /// must show its side of -- the edit is the only thing that says so.
    #[test]
    fn digging_at_a_border_shows_the_neighbours_side() {
        let registry = test_registry();
        let layer_of = |_: &str| 0;
        let blocks = TerrainBlocks::from_registry(&registry);
        let mut world = World::new();
        // Deep enough to be solid rock on both sides of the x = 0 plane.
        let (near, far) = ([-1, -40, 5], [0, -40, 5]);
        assert!(
            world.is_solid_at(near[0], near[1], near[2], blocks),
            "not rock"
        );
        assert!(
            world.is_solid_at(far[0], far[1], far[2], blocks),
            "not rock"
        );
        let far_node = NodeKey::containing(ChunkCoord::from_world_pos([0.0, -40.0, 5.0]), 0);
        let before = mesh_node(&world, &registry, &layer_of, far_node, blocks);
        assert!(
            !before
                .as_ref()
                .is_some_and(|g| has_face(&g.mesh, far_node, 0, 0, -1, far)),
            "a face against solid rock: covering is not on, so this proves nothing"
        );

        world.set_block(near[0], near[1], near[2], cubara_voxel::BlockId::AIR);
        let after = mesh_node(&world, &registry, &layer_of, far_node, blocks).expect("rock");
        assert!(
            has_face(&after.mesh, far_node, 0, 0, -1, far),
            "dug a hole beside the border and the neighbour shows no side there"
        );
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
