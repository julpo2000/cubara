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
use cubara_voxel::{BlockRegistry, ChunkCoord, FaceLinks};
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
    /// Every node the octree wants around the camera: the whole render
    /// distance, seen or not.
    desired: HashSet<NodeKey>,
    /// What joins what inside each node generated so far -- kept after a node's
    /// mesh is dropped for being out of sight, so the search can still pass
    /// through it without generating it again.
    links: HashMap<NodeKey, FaceLinks>,
    /// The nodes the camera could see ([`visibility::visible_nodes`]); the only
    /// ones meshed and drawn.
    visible: HashSet<NodeKey>,
    /// New links arrived since the visible set was worked out.
    visibility_stale: bool,
    /// Frames since the visible set was last worked out, to spread the search
    /// over a burst of arriving meshes instead of running it every frame.
    frames_since_visibility: u32,
}

/// How many frames arriving meshes may wait before the visible set is worked
/// out again. The search is a few milliseconds over a full render distance;
/// every frame during a load would be most of a frame budget, and ten frames
/// is still far quicker than a person notices a node appear.
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
            center: None,
            desired: HashSet::new(),
            links: HashMap::new(),
            visible: HashSet::new(),
            visibility_stale: false,
            frames_since_visibility: 0,
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
            self.stream_around(renderer, world, center);
        } else if self.visibility_stale && self.frames_since_visibility >= VISIBILITY_EVERY_FRAMES {
            self.follow_visibility(renderer, world, center);
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

    /// Drop nodes that fell outside the ring schedule, and *request* each
    /// desired node that isn't already resident or in flight, nearest first.
    /// Meshing happens on the worker pool; results are applied in
    /// [`drain_meshes`](Self::drain_meshes), so this never meshes on the
    /// caller's thread.
    ///
    /// A boundary crossing typically unloads some nodes and loads others at
    /// a *different* level for the same area (a coarse node splitting into
    /// finer ones, or the reverse) -- unlike a same-key LOD change, this
    /// isn't a same-key swap, so the outgoing node's geometry disappears
    /// immediately rather than staying drawn until the replacement is
    /// ready. A momentary gap at a ring boundary is an accepted, known
    /// limitation (see issue #107) -- the same category as the
    /// LOD-boundary cracks skirts (#108) fix, not a correctness bug.
    fn stream_around(&mut self, renderer: &mut Renderer, world: &Arc<World>, center: ChunkCoord) {
        self.desired =
            node::desired_nodes_3d(center, VERTICAL_LOD_SQUASH, node::DEFAULT_RING_SCHEDULE)
                .into_iter()
                .collect();
        // What a node joins does not depend on where the camera is, but a node
        // outside the render distance is not worth remembering.
        let desired = &self.desired;
        self.links.retain(|n, _| desired.contains(n));
        self.center = Some(center);
        self.follow_visibility(renderer, world, center);
    }

    /// Work out what the camera could see, drop what it cannot, and ask for
    /// what it can and is not here yet, nearest first.
    ///
    /// **Unseen nodes are not meshed, not uploaded and not drawn** -- and a
    /// node the search has not been able to reach is never even generated. A
    /// node not yet generated counts as seen but is not searched through until
    /// its links arrive, so the visible set grows outward along what can be
    /// seen as meshes come in.
    fn follow_visibility(
        &mut self,
        renderer: &mut Renderer,
        world: &Arc<World>,
        center: ChunkCoord,
    ) {
        let links = &self.links;
        self.visible = visibility::visible_nodes(center, &self.desired, |n| links.get(&n).copied());
        self.visibility_stale = false;
        self.frames_since_visibility = 0;

        let stale: Vec<NodeKey> = self
            .resident
            .iter()
            .chain(self.mesh_pool.in_flight().iter())
            .filter(|n| !self.visible.contains(n))
            .copied()
            .collect();
        for &node in &stale {
            self.resident.remove(&node);
            self.mesh_pool.cancel(node);
        }
        let to_unload: Vec<NodeId> = stale.into_iter().map(to_node_id).collect();
        renderer.apply_node_updates(to_unload, std::iter::empty());

        let mut to_load: Vec<NodeKey> = self
            .visible
            .iter()
            .filter(|n| !self.resident.contains(n) && !self.mesh_pool.is_in_flight(**n))
            .copied()
            .collect();
        node::sort_nearest_first(&mut to_load, center);
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
                if self.links.insert(built.node, built.links) != Some(built.links) {
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
        renderer.apply_node_updates(std::iter::empty(), meshed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
