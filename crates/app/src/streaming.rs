//! Node streaming: decides which nodes should be resident around the camera
//! and drives `cubara_world`'s mesh pool to keep them that way, handing
//! finished geometry to the renderer.
//!
//! This is the piece `cubara-render` itself can no longer do
//! (`ARCHITECTURE.md` §1, issue #38's tracking arc sub-issue #110): its
//! inputs are meshes, origins and a camera, nothing that knows what a
//! `World` or a `NodeKey` is. `cubara-app` is the one crate that depends on
//! both `cubara-render` and `cubara-world`, so the glue lives here.

use std::collections::HashSet;
use std::sync::Arc;

use cubara_render::{MeshedNode, NodeId, Renderer};
use cubara_voxel::{BlockRegistry, ChunkCoord};
use cubara_world::mesh::{sort_batch, BuiltNode, MeshPool};
use cubara_world::node::{self, NodeKey};
use cubara_world::World;

/// Vertical chunk band to stream -- the terrain sits comfortably inside it.
/// How far out (and at what LOD) is [`node::DEFAULT_RING_SCHEDULE`]'s job.
const STREAM_Y_MIN: i32 = 0;
const STREAM_Y_MAX: i32 = 2;

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

/// Owns the mesh-worker pool and the resident node set; drives
/// [`Renderer::apply_node_updates`] each frame. One per live `Renderer` (they
/// share a lifecycle -- see `main.rs`).
pub struct NodeStreaming {
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
}

impl NodeStreaming {
    pub fn new(
        registry: BlockRegistry,
        layer_of: impl Fn(&str) -> u32 + Send + Sync + 'static,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            layer_of: Arc::new(layer_of),
            mesh_pool: MeshPool::new(),
            resident: HashSet::new(),
            center: None,
        }
    }

    /// Bring the streamed set in line with the camera's current chunk (if it
    /// moved), and apply whatever's finished meshing since the last call.
    /// Cheap when nothing changed: `stream_around` only runs on a
    /// chunk-boundary crossing.
    pub fn update(&mut self, renderer: &mut Renderer, world: &Arc<World>, eye: [f32; 3]) {
        let center = ChunkCoord::from_world_pos(eye);
        if self.center != Some(center) {
            self.stream_around(renderer, world, center);
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
            .request(world, &self.registry, &self.layer_of, node);
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
        let y_range = STREAM_Y_MIN..=STREAM_Y_MAX;
        let desired_set: HashSet<NodeKey> =
            node::desired_nodes(center, y_range.clone(), node::DEFAULT_RING_SCHEDULE)
                .into_iter()
                .collect();

        // Unload anything no longer desired — uploaded or still in flight.
        let stale: Vec<NodeKey> = self
            .resident
            .iter()
            .chain(self.mesh_pool.in_flight().iter())
            .filter(|n| !desired_set.contains(n))
            .copied()
            .collect();
        for &node in &stale {
            self.resident.remove(&node);
            self.mesh_pool.cancel(node);
        }
        let to_unload: Vec<NodeId> = stale.into_iter().map(to_node_id).collect();
        renderer.apply_node_updates(to_unload, std::iter::empty());

        // Request whatever's desired but not yet resident, nearest first --
        // reuses `plan_node_updates`'s tested nearest-first ordering rather
        // than a second distance sort here.
        let updates =
            node::plan_node_updates(&self.resident, center, y_range, node::DEFAULT_RING_SCHEDULE);
        for node in updates.to_load {
            if self.mesh_pool.is_in_flight(node) {
                continue;
            }
            self.mesh_pool
                .request(world, &self.registry, &self.layer_of, node);
        }
        self.center = Some(center);
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
                self.resident.insert(built.node);
                to_meshed_node(built)
            })
            .collect();
        renderer.apply_node_updates(std::iter::empty(), meshed);
    }
}
