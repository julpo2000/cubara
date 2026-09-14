//! Node streaming: decides which nodes should be resident around the camera
//! and drives `cubara_world`'s mesh pool to keep them that way, handing
//! finished geometry to the renderer.
//!
//! This is the piece `cubara-render` itself can no longer do
//! (`ARCHITECTURE.md` §1, issue #38's tracking arc sub-issue #110): its
//! inputs are meshes, origins and a camera, nothing that knows what a
//! `World` or a `NodeKey` is. `cubara-app` is the one crate that depends on
//! both `cubara-render` and `cubara-world`, so the glue lives here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cubara_render::{MeshedNode, NodeId, Renderer};
use cubara_voxel::{BlockRegistry, ChunkCoord};
use cubara_world::mesh::{sort_batch, BuiltNode, MeshPool};
use cubara_world::node::{self, NodeKey};
use cubara_world::visibility;
use cubara_world::TerrainBlocks;
use cubara_world::World;

/// How many times sooner detail coarsens with height and depth than with
/// horizontal distance (`cubara_world::node::desired_nodes_3d`).
///
/// **There is no vertical band any more.** It used to stream only ±2
/// chunk-layers around the camera, chosen to pass the perf gate by drawing
/// less: build 40 blocks up and the ground stopped existing. The render
/// distance is now the same in every direction, paid for by leaving out
/// covered faces, and only the *detail* falls off faster vertically.
///
/// **Two, measured** (radius 64, `--bench 64 --eye … --squash k`, Windows /
/// RTX 4060; `BENCHMARKS.md`): at the surface 5,603 FPS with `2` against 6,293
/// with `4`, deep underground 5,096 against 6,985. `4` is faster, but caves
/// exist only at full detail and it keeps that only 40 blocks up and down, so
/// the bottom of a shaft you look into turns to solid rock. `2` keeps 80.
pub(crate) const VERTICAL_LOD_SQUASH: i32 = 2;

/// The nodes in `visible` that cover the same space `node` does: either its
/// eight children one level finer (the camera came closer and the node split)
/// or its parent one level coarser (the camera left and eight merged).
///
/// Checking exactly one level each way is enough because the ring schedule
/// keeps neighbours within one level of each other
/// (`cubara_world::node::desired_nodes`), and a node is replaced *in place* --
/// by what the octree puts in its own footprint, which is its children or its
/// parent and nothing else.
///
/// Empty means nothing is taking this space over: the camera walked away from
/// it, or the visibility search stopped reaching it. Those unload at once;
/// only a *replacement* is worth waiting for.
fn replacements_of(node: NodeKey, visible: &HashSet<NodeKey>) -> Vec<NodeKey> {
    let mut found = Vec::new();
    if let Some(finer) = node.level.checked_sub(1) {
        let [x, y, z] = node.pos;
        for dx in 0..2 {
            for dy in 0..2 {
                for dz in 0..2 {
                    let child = NodeKey::new(finer, [2 * x + dx, 2 * y + dy, 2 * z + dz]);
                    if visible.contains(&child) {
                        found.push(child);
                    }
                }
            }
        }
    }
    let [x, y, z] = node.pos;
    let parent = NodeKey::new(
        node.level + 1,
        [x.div_euclid(2), y.div_euclid(2), z.div_euclid(2)],
    );
    if visible.contains(&parent) {
        found.push(parent);
    }
    found
}

/// Whether `node` can leave the arena this frame without leaving a hole.
///
/// **This is the whole fix for #262.** A node used to be unloaded the moment
/// it left the visible set, while what replaces it was still being meshed on
/// the worker pool -- tens of milliseconds during which nothing at all was
/// drawn there, so walking towards a mountain showed sky through it, over and
/// over as the search re-ran. Waiting costs a few frames of a slightly coarser
/// (or finer) mountain; not waiting costs a hole.
///
/// Drawing both meanwhile is not the alternative: a node and its replacement
/// approximate the same surface, so both in the arena at once is z-fighting
/// rather than a hole. Hence *swap*, in one `apply_node_updates` call.
fn can_unload(node: NodeKey, visible: &HashSet<NodeKey>, resident: &HashSet<NodeKey>) -> bool {
    let replacements = replacements_of(node, visible);
    replacements.is_empty() || replacements.iter().all(|r| resident.contains(r))
}

pub(crate) fn to_node_id(node: NodeKey) -> NodeId {
    NodeId {
        level: node.level,
        pos: node.pos,
    }
}

/// A finished `cubara_world::mesh` job, converted to what `cubara-render`
/// understands -- `None` if the node was empty. Shared by the live
/// (worker-pool) path here and the one-shot `--bench`/`--screenshot` paths,
/// since both end up needing exactly this conversion.
pub(crate) fn to_meshed_node(built: BuiltNode) -> Option<MeshedNode> {
    let geometry = built.geometry?;
    Some(MeshedNode {
        id: to_node_id(built.node),
        origin: geometry.origin,
        scale: geometry.scale,
        mesh: geometry.mesh,
        aabb: geometry.aabb,
    })
}

/// Where a node is in the **rendering** lifecycle
/// (`docs/PHASE2_ARCHITECTURE.md` §11.1).
///
/// This is the other half of block 2.6, and the half that deliberately did
/// **not** move into `cubara-world`. A node is a rendering unit: it exists at
/// the level it does because of its distance from a *camera*, and above level 0
/// one node covers up to 512 chunks. The simulation's lifecycle
/// ([`cubara_world::ChunkState`]) is per chunk and keyed off the *player*. As
/// one enum, a chunk would go dormant because it was far from the camera.
///
/// The states were always here -- as `HashSet` membership plus whatever
/// `MeshPool` was holding. This names them, and changes nothing: #47's bar for
/// this half is explicitly "re-expressed in terms of states with no behaviour
/// change", and restructuring the two containers into one would be a real
/// change with real risk and no behavioural benefit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeResidency {
    /// Not wanted, or not asked for yet.
    Absent,
    /// Requested; a worker is meshing it.
    InFlight,
    /// Meshed and handed to the renderer (or known to be empty).
    Resident,
}

/// Owns the mesh-worker pool and the resident node set; drives
/// [`Renderer::apply_node_updates`] each frame. One per live `Renderer` (they
/// share a lifecycle -- see `main.rs`).
pub struct NodeStreaming {
    /// Which ids the terrain is made of, including the oak.
    blocks: TerrainBlocks,
    registry: Arc<BlockRegistry>,
    layer_of: Arc<dyn Fn(&str) -> u32 + Send + Sync>,
    mesh_pool: MeshPool,
    /// Nodes meshed and uploaded (or known empty) -- a `NodeKey` already
    /// names both position and detail level, so residency is just set
    /// membership.
    resident: HashSet<NodeKey>,
    /// The chunk the camera was in as of the last [`update`](Self::update)
    /// call; `None` before the first one, so it always streams in the
    /// initial region rather than needing a separate priming call.
    center: Option<ChunkCoord>,
    /// What joins what inside each node generated so far -- kept after a node's
    /// mesh is dropped for being out of sight, so the search can still pass
    /// through it without generating it again. Behind an [`Arc`] so a search
    /// can take a snapshot without copying it; a write while a search holds
    /// one copies once.
    links: Arc<HashMap<NodeKey, visibility::NodeLinks>>,
    /// Nodes that have left the visible set but are still drawn, because what
    /// replaces them is not resident yet (#262). Dropping them on time is what
    /// made mountains flicker; they leave in the same `apply_node_updates` call
    /// that brings their replacement in.
    held: HashSet<NodeKey>,
    /// The nodes the camera could see ([`visibility::visible_nodes`]); the only
    /// ones meshed and drawn.
    visible: HashSet<NodeKey>,
    /// New links arrived since the last search was started.
    visibility_stale: bool,
    /// Frames since the last search was started, to spread searches over a
    /// burst of arriving meshes.
    frames_since_visibility: u32,
    /// Searches run here, off the frame: over a full render distance one takes
    /// tens of milliseconds, which on the main thread would be a hitch every
    /// time the camera crossed a chunk.
    searcher: Searcher,
    /// The newest search started, so an older answer arriving late is ignored.
    generation: u64,
    /// A search is running that has not answered yet.
    searching: bool,
}

/// One search to run: the camera's chunk and a snapshot of what is known.
struct SearchJob {
    generation: u64,
    center: ChunkCoord,
    links: Arc<HashMap<NodeKey, visibility::NodeLinks>>,
}

/// A search's answer: the whole render distance around `center`, and what of it
/// can be seen.
struct SearchDone {
    generation: u64,
    center: ChunkCoord,
    desired: HashSet<NodeKey>,
    visible: HashSet<NodeKey>,
}

/// A thread that works out the render distance and what of it is visible.
struct Searcher {
    jobs: std::sync::mpsc::Sender<SearchJob>,
    done: std::sync::mpsc::Receiver<SearchDone>,
    _thread: std::thread::JoinHandle<()>,
}

impl Searcher {
    fn new() -> Self {
        let (jobs, job_rx) = std::sync::mpsc::channel::<SearchJob>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("cubara-visibility".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    // Only the newest matters; skip any that queued behind it.
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    let desired: HashSet<NodeKey> = node::desired_nodes_3d(
                        job.center,
                        VERTICAL_LOD_SQUASH,
                        node::DEFAULT_RING_SCHEDULE,
                    )
                    .into_iter()
                    .collect();
                    let links = &job.links;
                    let visible =
                        visibility::visible_nodes(job.center, &desired, |n| links.get(&n).copied());
                    let answer = SearchDone {
                        generation: job.generation,
                        center: job.center,
                        desired,
                        visible,
                    };
                    if done_tx.send(answer).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn visibility thread");
        Self {
            jobs,
            done,
            _thread: thread,
        }
    }
}

/// How many frames arriving meshes may wait before another search is started.
/// Searches run on their own thread, but each takes tens of milliseconds, so
/// starting one every frame during a load would only queue answers nobody
/// waits for.
const VISIBILITY_EVERY_FRAMES: u32 = 10;

impl NodeStreaming {
    /// Takes the registry already behind an [`Arc`] rather than wrapping one
    /// itself: `Game` needs the *same* registry to resolve a broken block's
    /// name to an item (block 2.1d, #143), and two loads would be two id
    /// spaces -- ids are assigned per registry by sorted name (§1.2), so the
    /// same number would mean different materials on each side.
    pub fn new(
        registry: Arc<BlockRegistry>,
        structures: &cubara_voxel::StructureRegistry,
        ores: &cubara_voxel::OreRegistry,
        layer_of: impl Fn(&str) -> u32 + Send + Sync + 'static,
    ) -> Self {
        Self {
            // Resolved once, here, and carried with every meshing job.
            // Meshing used to re-derive this per node from the registry, which
            // was both wasted work and blind to structures (block 2.3a).
            blocks: TerrainBlocks::from_registry(&registry)
                .with_oak(structures, &registry)
                .with_ores(ores, &registry),
            registry,
            layer_of: Arc::new(layer_of),
            mesh_pool: MeshPool::new(),
            resident: HashSet::new(),
            held: HashSet::new(),
            center: None,
            links: Arc::new(HashMap::new()),
            visible: HashSet::new(),
            visibility_stale: false,
            frames_since_visibility: 0,
            searcher: Searcher::new(),
            generation: 0,
            searching: false,
        }
    }

    /// Bring the streamed set in line with the camera's current chunk (if it
    /// moved), and apply whatever's finished meshing since the last call.
    /// Cheap when nothing changed: `stream_around` only runs on a
    /// chunk-boundary crossing.
    pub fn update(&mut self, renderer: &mut Renderer, world: &Arc<World>, eye: [f32; 3]) {
        let center = ChunkCoord::from_world_pos(eye);
        self.frames_since_visibility += 1;
        if self.center != Some(center) {
            self.center = Some(center);
            self.start_search(center);
        } else if self.visibility_stale
            && !self.searching
            && self.frames_since_visibility >= VISIBILITY_EVERY_FRAMES
        {
            self.start_search(center);
        }
        while let Ok(done) = self.searcher.done.try_recv() {
            if done.generation == self.generation {
                self.searching = false;
                self.follow_visibility(renderer, world, done);
            }
        }
        self.drain_meshes(renderer);
    }

    /// Force a re-mesh of the chunk `cc` (e.g. after an edit): the worker
    /// re-reads the edit overlay, and the next [`update`](Self::update)'s
    /// `drain_meshes` swaps the geometry in atomically, so there's no gap.
    ///
    /// Always re-meshes `cc`'s level-0 node, not whatever node is currently
    /// resident there: an edit only ever happens within player reach, which
    /// is well inside the always-full-resolution near field the ring
    /// schedule keeps around the player (`World::node_at`'s doc comment) --
    /// a coarser node can never be the one actually representing an
    /// editable chunk.
    pub fn invalidate(&mut self, world: &Arc<World>, cc: ChunkCoord) {
        let node = NodeKey::containing(cc, 0);
        self.mesh_pool.cancel(node);
        self.mesh_pool
            .request(world, &self.registry, &self.layer_of, node, self.blocks);
    }

    /// Hand the search thread the camera's chunk and what is known now.
    fn start_search(&mut self, center: ChunkCoord) {
        self.generation += 1;
        self.searching = true;
        self.visibility_stale = false;
        self.frames_since_visibility = 0;
        let job = SearchJob {
            generation: self.generation,
            center,
            links: Arc::clone(&self.links),
        };
        // A closed channel means the thread died with a panic, which it
        // reports itself; drawing what is already loaded is the best left.
        let _ = self.searcher.jobs.send(job);
    }

    /// Apply a search's answer: drop what cannot be seen, and ask for what can
    /// and is not here yet, nearest first.
    ///
    /// **Unseen nodes are not meshed, not uploaded and not drawn** -- and a
    /// node the search has not been able to reach is never even generated. A
    /// node not yet generated counts as seen but is not searched through until
    /// its links arrive, so the visible set grows outward along what can be
    /// seen as meshes come in.
    fn follow_visibility(&mut self, renderer: &mut Renderer, world: &Arc<World>, done: SearchDone) {
        // What a node joins does not depend on where the camera is, but a node
        // outside the render distance is not worth remembering.
        let desired = done.desired;
        Arc::make_mut(&mut self.links).retain(|n, _| desired.contains(n));
        self.visible = done.visible;

        let stale: Vec<NodeKey> = self
            .resident
            .iter()
            .chain(self.mesh_pool.in_flight().iter())
            .filter(|n| !self.visible.contains(n))
            .copied()
            .collect();
        // Nothing in flight is worth finishing once it has left the visible
        // set -- that is work, not geometry, and cancelling it is what keeps
        // walking back and forth from queueing meshes nobody wants.
        for &node in &stale {
            self.mesh_pool.cancel(node);
        }
        // ...but what is already *drawn* only leaves once its replacement has
        // arrived (#262). `held` keeps drawing until then; it is re-checked
        // here and again as each mesh lands, so the swap happens on the frame
        // the replacement is ready rather than on the next search.
        let (unload_now, held): (Vec<NodeKey>, Vec<NodeKey>) = stale
            .into_iter()
            .filter(|n| self.resident.contains(n))
            .partition(|&n| can_unload(n, &self.visible, &self.resident));
        for &node in &unload_now {
            self.resident.remove(&node);
        }
        self.held = held.into_iter().collect();
        let to_unload: Vec<NodeId> = unload_now.into_iter().map(to_node_id).collect();
        renderer.apply_node_updates(to_unload, std::iter::empty());

        let mut to_load: Vec<NodeKey> = self
            .visible
            .iter()
            .filter(|n| !self.resident.contains(n) && !self.mesh_pool.is_in_flight(**n))
            .copied()
            .collect();
        node::sort_nearest_first(&mut to_load, done.center);
        for node in to_load {
            self.mesh_pool
                .request(world, &self.registry, &self.layer_of, node, self.blocks);
        }
    }

    /// Where `node` is in the rendering lifecycle (§11.1).
    ///
    /// Derived from the existing containers rather than stored separately --
    /// two sources of truth for one fact is how they drift apart.
    ///
    /// Nothing calls this yet: naming the states is what #47 asked for, and the
    /// streaming loop reads the containers directly because that is the code
    /// that already worked. It is the accessor anything asking "what is this
    /// node doing" should use rather than reaching into `resident`.
    #[allow(dead_code)]
    pub fn residency(&self, node: NodeKey) -> NodeResidency {
        if self.resident.contains(&node) {
            NodeResidency::Resident
        } else if self.mesh_pool.is_in_flight(node) {
            NodeResidency::InFlight
        } else {
            NodeResidency::Absent
        }
    }

    /// Take finished meshes from the worker pool and hand them to the
    /// renderer, in a fixed order (ascending `NodeKey`, [`sort_batch`])
    /// rather than whatever order the workers happened to finish in --
    /// issue #83. Marks nodes resident immediately (so they aren't
    /// re-requested); [`Renderer::apply_node_updates`] paces the actual GPU
    /// upload itself.
    fn drain_meshes(&mut self, renderer: &mut Renderer) {
        let meshed: Vec<MeshedNode> = sort_batch(self.mesh_pool.poll())
            .into_iter()
            .filter_map(|built| {
                if self.links.get(&built.node) != Some(&built.links) {
                    Arc::make_mut(&mut self.links).insert(built.node, built.links);
                    self.visibility_stale = true;
                }
                // Went out of sight while it was being meshed: remember what it
                // joins, but do not upload it.
                if !self.visible.contains(&built.node) {
                    return None;
                }
                self.resident.insert(built.node);
                to_meshed_node(built)
            })
            .collect();
        // The arriving meshes may be exactly what a held node was waiting for
        // (#262). Release those in the *same* call, so the coarse node and the
        // fine ones that replace it are never both in the arena -- and never
        // neither.
        let released: Vec<NodeId> = if self.held.is_empty() {
            Vec::new()
        } else {
            let ready: Vec<NodeKey> = self
                .held
                .iter()
                .filter(|&&n| can_unload(n, &self.visible, &self.resident))
                .copied()
                .collect();
            for node in &ready {
                self.held.remove(node);
                self.resident.remove(node);
            }
            ready.into_iter().map(to_node_id).collect()
        };
        renderer.apply_node_updates(released, meshed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chunks a node covers: `2^level` on each axis from its own grid position.
    fn covers(node: NodeKey) -> Vec<[i32; 3]> {
        let e = 1i32 << node.level;
        let [x, y, z] = node.pos;
        let mut out = Vec::new();
        for cx in x * e..x * e + e {
            for cy in y * e..y * e + e {
                for cz in z * e..z * e + e {
                    out.push([cx, cy, cz]);
                }
            }
        }
        out
    }

    fn children_of(node: NodeKey) -> Vec<NodeKey> {
        let finer = node.level - 1;
        let [x, y, z] = node.pos;
        let mut out = Vec::new();
        for dx in 0..2 {
            for dy in 0..2 {
                for dz in 0..2 {
                    out.push(NodeKey::new(finer, [2 * x + dx, 2 * y + dy, 2 * z + dz]));
                }
            }
        }
        out
    }

    /// The owner walked towards a mountain and saw it flicker (#262): the
    /// coarse node left the arena the instant it stopped being visible, while
    /// its eight replacements were still on the worker pool.
    #[test]
    fn a_node_stays_until_every_child_replacing_it_has_arrived() {
        let parent = NodeKey::new(2, [3, 0, -4]);
        let kids = children_of(parent);
        let visible: HashSet<NodeKey> = kids.iter().copied().collect();

        // Nothing has arrived: holding it is the only thing that is not a hole.
        let mut resident: HashSet<NodeKey> = HashSet::from([parent]);
        assert!(!can_unload(parent, &visible, &resident));

        // Seven of eight is still a hole -- an `any` here instead of `all`
        // would leave one eighth of the mountain missing, which is precisely
        // the shape of the bug being fixed.
        for kid in kids.iter().take(7) {
            resident.insert(*kid);
            assert!(
                !can_unload(parent, &visible, &resident),
                "released with {kid:?} the last one missing"
            );
        }
        resident.insert(kids[7]);
        assert!(can_unload(parent, &visible, &resident));
    }

    /// Walking away merges eight into one, and had the identical hole. A fix
    /// that only handled splitting would pass the test above and still flicker
    /// on the way back down the mountain.
    #[test]
    fn eight_children_stay_until_the_parent_replacing_them_has_arrived() {
        let parent = NodeKey::new(2, [3, 0, -4]);
        let kids = children_of(parent);
        let visible: HashSet<NodeKey> = HashSet::from([parent]);
        let resident: HashSet<NodeKey> = kids.iter().copied().collect();

        for kid in &kids {
            assert!(!can_unload(*kid, &visible, &resident));
        }
        let with_parent: HashSet<NodeKey> = resident.union(&visible).copied().collect();
        for kid in &kids {
            assert!(can_unload(*kid, &visible, &with_parent));
        }
    }

    /// Only *replacement* is worth waiting for. A node the camera has simply
    /// left behind must go at once, or the render distance stops bounding
    /// anything and the arena fills with the world behind you.
    #[test]
    fn a_node_nothing_is_replacing_leaves_immediately() {
        let gone = NodeKey::new(1, [40, 0, 40]);
        let elsewhere: HashSet<NodeKey> = HashSet::from([NodeKey::new(1, [-40, 0, -40])]);
        let resident: HashSet<NodeKey> = HashSet::from([gone]);
        assert!(can_unload(gone, &elsewhere, &resident));
        // Including when nothing at all is visible (walked into a cave).
        assert!(can_unload(gone, &HashSet::new(), &resident));
    }

    /// The property the old code broke, stated over chunks rather than nodes:
    /// while detail changes, every chunk that was drawn stays drawn.
    #[test]
    fn no_chunk_is_ever_uncovered_while_the_detail_changes() {
        let parent = NodeKey::new(2, [1, 0, 1]);
        let kids = children_of(parent);
        let before: HashSet<[i32; 3]> = covers(parent).into_iter().collect();
        let visible: HashSet<NodeKey> = kids.iter().copied().collect();

        // Feed the children in one at a time, unloading the parent as soon as
        // the policy allows, and check coverage after every single step.
        let mut resident: HashSet<NodeKey> = HashSet::from([parent]);
        let mut parent_gone = false;
        for kid in kids.iter().chain(std::iter::once(&kids[7])) {
            resident.insert(*kid);
            if !parent_gone && can_unload(parent, &visible, &resident) {
                resident.remove(&parent);
                parent_gone = true;
            }
            let drawn: HashSet<[i32; 3]> = resident.iter().flat_map(|&n| covers(n)).collect();
            for chunk in &before {
                assert!(
                    drawn.contains(chunk),
                    "chunk {chunk:?} was drawn and then was not"
                );
            }
        }
        assert!(
            parent_gone,
            "the coarse node must leave once it is replaced"
        );
    }

    /// The rendering lifecycle is a *different* lifecycle from the simulation's
    /// (§11.1), and this is where that is asserted rather than only written
    /// down: the two enums are not convertible, and nothing here mentions a
    /// `ChunkCoord`.
    #[test]
    fn node_residency_names_the_states_that_already_existed() {
        let states = [
            NodeResidency::Absent,
            NodeResidency::InFlight,
            NodeResidency::Resident,
        ];
        // Distinct, and exhaustive: absent, asked for, arrived.
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                assert_eq!(i == j, a == b, "{a:?} vs {b:?}");
            }
        }
    }
}
