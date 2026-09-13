//! Shared node-geometry arena for GPU-driven rendering.
//!
//! Instead of one vertex/index buffer (and one draw call) per LOD node, every
//! resident node's geometry lives in a pair of large, pooled GPU buffers — a
//! vertex arena and an index arena — with per-node sub-allocations. A node is
//! one mesh, one arena allocation, one draw, covering `2^level` chunks per
//! axis (§6.1) — level 0 is exactly one chunk. Streaming churn (nodes
//! constantly loading/unloading) is absorbed by a first-fit, coalescing
//! [`SlabAllocator`] over each arena, so freed slots are reused.
//!
//! Per frame, the CPU frustum-culls the resident nodes, writes one
//! [`DrawIndexedIndirect`] entry per visible node into an indirect-args buffer,
//! and issues a single `multi_draw_indexed_indirect` — collapsing many draws
//! into one submit (see issue #27 / `PLAN.md` §10). Backends without
//! `MULTI_DRAW_INDIRECT` (checked via the spike, #26) fall back to a loop of
//! `draw_indexed` over the *same* shared buffers, so there is no second geometry
//! path to maintain.
//!
//! The per-node metadata this builds (AABB + geometry offsets) is exactly what
//! the follow-up compute cull (#28) consumes; only *who writes the draw list*
//! moves from CPU to GPU. No throwaway work.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use cubara_voxel::{build_mesh_bounded, Chunk, Mesh, MeshContext, Vertex};

use crate::culling::{Aabb, Frustum};

/// An opaque, orderable, hashable key for one resident node's arena slot.
///
/// This crate never learns what a "node" or a "level" is (`ARCHITECTURE.md`
/// §1 -- the renderer's inputs are meshes, origins and a camera, nothing that
/// knows what a chunk is); `NodeId` carries no meaning of its own beyond being
/// a fixed, deterministic key, so residency tracking and the deterministic
/// draw order (issue #81) don't depend on request/completion order. Its shape
/// mirrors `cubara_world::node::NodeKey` on purpose, so a caller's conversion
/// from one to the other is a trivial field copy, not an encoding scheme --
/// but the two types share no code and this crate has no dependency on that
/// one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId {
    pub level: u32,
    pub pos: [i32; 3],
}

/// Vertex-arena capacity, in vertices (12 bytes/vertex, the packed format
/// since #43 -- not the 28-byte figure an earlier version of this comment
/// quoted, from before that landed). Re-derived (issue #38's tracking arc,
/// sub-issue #111) against the real post-node-tree radius-64 peak: 1,659,216
/// vertices used (`BENCHMARKS.md`, footnote 29), so 4,000,000 is ~2.4×
/// headroom -- **unchanged from the #89-era value**, because that number was
/// never really "sized for radius 12" in any binding sense, just generously
/// picked early and left to absorb terrain growth since; measured against
/// the real worst case for the first time here, it's still comfortably
/// covered without needing to move.
const VERTEX_CAPACITY: u32 = 4_000_000;
/// Index-arena capacity, in indices (4 bytes/index). Same story as
/// `VERTEX_CAPACITY`: measured radius-64 peak is 2,488,824 indices used, so
/// 6,000,000 is ~2.4× headroom, unchanged from #89.
const INDEX_CAPACITY: u32 = 6_000_000;
/// Max nodes the indirect-args buffer can hold (upper bound on *visible*
/// nodes in one frame). Node-based streaming (§6.1) is what finally lets
/// this shrink: the tuned ring schedule (#109) measured 1,341 of 1,585
/// resident nodes visible at once from a wide-open orbit camera (~85% --
/// nearly everything resident can be visible from the right angle, so this
/// isn't sized as a small fraction of residency). 4,096 is ~2.5× headroom
/// over that -- matching #89's own ~2.6× precedent, just against the new,
/// far smaller peak -- down from 16,384 (a 4× reduction, ~0.23 MiB saved).
///
/// **Raised back to 16,384 for the vertical world.** #109 sized 4,096 against a
/// world three chunk-layers tall; once the streamed band follows the player
/// vertically the resident set is several times that, and 4,096 stops being
/// headroom and becomes a *truncation*. Exceeding it does not warn and does not
/// slow down -- it silently stops drawing nodes, which then measures as a
/// *higher* frame rate on a world with holes in it. A capacity constant whose
/// failure mode is "the benchmark looks better" is the worst kind, and it is why
/// this is sized for the world now streamed rather than the one #109 measured.
const MAX_DRAWS: u32 = 16_384;
/// Max simultaneously *resident* nodes with an origin slot -- unlike
/// `MAX_DRAWS` (the per-frame visible-set cap), this bounds the whole
/// streamed set, and is kept at the same 4× multiple over `MAX_DRAWS` #89
/// originally used (65,536 / 16,384 = 4), so a resident-count regression
/// still trips `ArenaUsage::exhausted`'s "draws" warning well before ever
/// approaching this hard ceiling. Radius 64's tuned schedule measured
/// 1,585-1,613 resident nodes across four different world positions
/// (`BENCHMARKS.md` footnote 27) -- 16,384 leaves ~10× headroom, down from
/// 65,536 (a 4× reduction, ~0.75 MiB saved).
const MAX_NODES: u32 = 16_384;

/// A free-list index allocator over `0..MAX_NODES`, handing out the node
/// index each resident node uses to find its origin in the storage buffer.
/// The index is baked into every vertex of that node's mesh (`Vertex::
/// with_node_index`, [`insert`](ChunkArena::insert)) and read back by
/// `mesh.wgsl` from vertex data, not `@builtin(instance_index)` — see §5.3
/// for why neither instance-indexing mechanism survived every real backend.
/// Simpler than [`SlabAllocator`]: every unit is exactly one index, so
/// there's no coalescing to do.
struct NodeIndexAllocator {
    next: u32,
    free: Vec<u32>,
}

impl NodeIndexAllocator {
    fn new() -> Self {
        Self {
            next: 0,
            free: Vec::new(),
        }
    }

    fn alloc(&mut self) -> Option<u32> {
        if let Some(i) = self.free.pop() {
            return Some(i);
        }
        if self.next < MAX_NODES {
            let i = self.next;
            self.next += 1;
            Some(i)
        } else {
            None
        }
    }

    fn free(&mut self, index: u32) {
        self.free.push(index);
    }
}

/// Capacity headroom for the arena's three fixed-size resources — vertices,
/// indices and per-frame draws. `*_used` is a high-water mark (vertices/indices)
/// or the resident node count (draws: any resident node could be visible in a
/// single frame, so residency is the worst case a frame could ask for). This is
/// the "requested vs available" figure issue #89 asks for, so a full arena is a
/// reported number rather than a silently truncated frame.
#[derive(Clone, Copy, Debug)]
pub struct ArenaUsage {
    pub vertices_used: u32,
    pub vertices_capacity: u32,
    pub indices_used: u32,
    pub indices_capacity: u32,
    pub resident_nodes: u32,
    pub max_draws: u32,
}

impl ArenaUsage {
    /// Names of the resources at or over capacity — empty when the arena has
    /// headroom on all three.
    pub fn exhausted(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.vertices_used >= self.vertices_capacity {
            out.push("vertices");
        }
        if self.indices_used >= self.indices_capacity {
            out.push("indices");
        }
        if self.resident_nodes >= self.max_draws {
            out.push("draws");
        }
        out
    }
}

/// One indirect draw command, matching the GPU's `DrawIndexedIndirect` layout
/// (5 tightly-packed 32-bit words). We define our own `Pod` mirror of
/// `wgpu::util::DrawIndexedIndirectArgs` so a whole visible-set slice can be
/// uploaded with a single `write_buffer`.
#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawIndexedIndirect {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

/// `Bounds` in `cull.wgsl`: a node's box, padded to storage alignment.
#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Bounds {
    lo: [f32; 3],
    _pad0: f32,
    hi: [f32; 3],
    _pad1: f32,
}

/// How one frame's draw list is split between the two passes
/// ([`crate::occlusion`]): the first `first` entries are drawn outright, the
/// `candidates` after them only where the occlusion test lets them through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Draws {
    pub first: u32,
    pub candidates: u32,
}

impl Draws {
    /// Every node that survived the frustum cull.
    pub fn total(self) -> u32 {
        self.first + self.candidates
    }
}

/// What the latest occlusion results said, for a frame some frames back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OcclusionStats {
    /// Nodes in that frame's list, after the frustum cull.
    pub tested: u32,
    /// Of those, the ones the test found visible.
    pub visible: u32,
    /// Of those tested, the ones drawn only if visible (the second pass).
    pub candidates: u32,
    /// Of the candidates, the ones found hidden -- and so not drawn at all.
    pub hidden_candidates: u32,
    /// Triangles that frame drew: the whole first pass, and the candidates
    /// found visible.
    pub triangles_drawn: u64,
}

/// A buffer the occlusion results are copied into, to be read on the CPU.
struct Readback {
    buffer: wgpu::Buffer,
    state: ReadState,
}

enum ReadState {
    Idle,
    /// Copied into by a frame, which may not have been submitted yet.
    Copied(FrameList),
    /// Asked to map: 0 while pending, 1 mapped, 2 failed.
    Mapping(FrameList, Arc<AtomicU8>),
}

/// Which nodes one frame's list held, in order, to put its results against.
struct FrameList {
    frame: u64,
    first: u32,
    /// Each entry's node index and triangle count.
    entries: Vec<(u32, u32)>,
}

/// Copies in flight at once. A result is a frame or two old by the time it can
/// be read; beyond a few, a frame just goes without a copy.
const READBACKS: usize = 3;

/// Where one node's geometry lives inside the shared arenas, plus its world-space
/// bounds for culling. This is the per-node metadata the GPU compute cull (#28)
/// will read straight from a storage buffer.
#[derive(Clone, Copy)]
struct NodeSlot {
    /// First vertex of this node in the vertex arena (used as `base_vertex`).
    base_vertex: u32,
    vertex_len: u32,
    /// First index of this node in the index arena.
    first_index: u32,
    index_count: u32,
    aabb: Aabb,
    /// This node's slot in the node-origins storage buffer. Baked into every
    /// vertex of this node's mesh at insert time ([`Vertex::with_node_index`]),
    /// so `mesh.wgsl` reads it from vertex data rather than an instance index.
    node_index: u32,
    /// Indices per face direction, in [`Face`](cubara_voxel::Face) order
    /// ([`Mesh::group_by_face`](cubara_voxel::Mesh::group_by_face)), or `None`
    /// for a mesh that was not grouped and is always drawn whole.
    face_indices: Option<[u32; 6]>,
}

/// How far behind a face's plane the camera may be and still have the face
/// drawn. The planes are exact; the eye is recovered from the view-projection
/// matrix (`Frustum::eye`), good to about a thousandth of a block.
const FACING_MARGIN: f32 = 0.05;

/// Which face directions of a node bounded by `aabb` can face a camera at
/// `eye`, in [`Face`](cubara_voxel::Face) order.
///
/// A `PosX` face on the plane `x = p` is seen only from `x > p`, from behind
/// it is back-face culled; and every face of the node lies within its box, so
/// if the camera is not past the box's low x, no `PosX` face of it can show.
/// About half of what a node draws points away from any one camera -- and
/// without this each of those vertices is still run through the vertex shader,
/// only for the triangle to be thrown away after.
pub fn faces_facing(aabb: &Aabb, eye: glam::Vec3) -> [bool; 6] {
    let (lo, hi) = (aabb.min, aabb.max);
    [
        eye.x > lo.x - FACING_MARGIN,
        eye.x < hi.x + FACING_MARGIN,
        eye.y > lo.y - FACING_MARGIN,
        eye.y < hi.y + FACING_MARGIN,
        eye.z > lo.z - FACING_MARGIN,
        eye.z < hi.z + FACING_MARGIN,
    ]
}

/// First-fit free-list suballocator over a fixed capacity of fixed-size units
/// (vertices or indices). Free ranges are kept sorted and coalesced so repeated
/// load/unload churn doesn't permanently fragment the arena.
struct SlabAllocator {
    capacity: u32,
    /// Sorted, non-overlapping, non-adjacent `(offset, len)` free ranges.
    free: Vec<(u32, u32)>,
    /// Highest unit ever handed out — a coarse fragmentation/occupancy gauge.
    high_water: u32,
}

impl SlabAllocator {
    fn new(capacity: u32) -> Self {
        Self {
            capacity,
            free: vec![(0, capacity)],
            high_water: 0,
        }
    }

    /// Reserve `n` contiguous units, returning the start offset, or `None` if no
    /// free range is large enough.
    fn alloc(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        for i in 0..self.free.len() {
            let (off, len) = self.free[i];
            if len >= n {
                if len == n {
                    self.free.remove(i);
                } else {
                    self.free[i] = (off + n, len - n);
                }
                self.high_water = self.high_water.max(off + n);
                return Some(off);
            }
        }
        None
    }

    /// Return `[off, off + n)` to the free list, coalescing with adjacent ranges.
    fn free(&mut self, off: u32, n: u32) {
        if n == 0 {
            return;
        }
        debug_assert!(off + n <= self.capacity, "freeing outside the arena");
        // Find the first free range that starts after `off`.
        let idx = self.free.partition_point(|&(o, _)| o < off);

        // Coalesce with the previous range if it ends exactly at `off`.
        if idx > 0 {
            let (poff, plen) = self.free[idx - 1];
            if poff + plen == off {
                let merged_len = plen + n;
                // Also bridge to the next range if now adjacent.
                if idx < self.free.len() && poff + merged_len == self.free[idx].0 {
                    let (_, nlen) = self.free.remove(idx);
                    self.free[idx - 1] = (poff, merged_len + nlen);
                } else {
                    self.free[idx - 1] = (poff, merged_len);
                }
                return;
            }
        }
        // Coalesce with the next range if `off + n` meets its start.
        if idx < self.free.len() && off + n == self.free[idx].0 {
            let (_, nlen) = self.free[idx];
            self.free[idx] = (off, n + nlen);
            return;
        }
        // No neighbour to merge with — insert a standalone range.
        self.free.insert(idx, (off, n));
    }
}

/// Every resident node's geometry in shared vertex/index buffers, drawn with one
/// indirect submit (or a `draw_indexed` loop on backends without MDI).
pub struct ChunkArena {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    /// One `DrawIndexedIndirect` per visible node, rewritten each frame.
    indirect_buffer: wgpu::Buffer,
    /// One world-space origin (plus scale in `.w`) per resident node, indexed
    /// by the `node_index` baked into each vertex -- what `mesh.wgsl` adds
    /// (and multiplies) into a packed vertex's node-local position (see
    /// [`build_node_mesh`]).
    origins_buffer: wgpu::Buffer,
    origins_bind_group: wgpu::BindGroup,

    vertices: SlabAllocator,
    indices: SlabAllocator,
    nodes: NodeIndexAllocator,
    slots: BTreeMap<NodeId, NodeSlot>,

    /// Whether the device supports `multi_draw_indexed_indirect`.
    multi_draw: bool,
    /// Per-frame scratch: the visible draw list built by [`prepare`](Self::prepare).
    visible: Vec<DrawIndexedIndirect>,
    /// What the last [`prepare`](Self::prepare) queued: nodes, and triangles.
    visible_nodes: u32,
    visible_triangles: u64,
    /// Occlusion culling ([`crate::occlusion`]): each listed node's box, the
    /// test's verdict per entry, and the copies being read back.
    bounds_buffer: wgpu::Buffer,
    seen_buffer: wgpu::Buffer,
    readbacks: Vec<Readback>,
    /// Whether to split the list at all. Off, every node is drawn in the first
    /// pass and nothing is tested -- the image must be the same either way.
    occlusion: bool,
    /// The nodes the newest results found visible: next frame's first pass.
    /// The frame each node index was last found visible in; a node counts as
    /// seen when that is the frame of the newest results. By index rather than
    /// by `NodeId`, because this is looked up for every node every frame: a
    /// node that reuses a removed one's index may start out drawn in the first
    /// pass for a frame, which costs nothing but time.
    seen_in: Vec<u64>,
    /// Frame counter, and the frame the newest results came from.
    frame: u64,
    seen_frame: u64,
    stats: OcclusionStats,
    /// This frame's list, waiting for [`encode_readback`](Self::encode_readback).
    frame_list: Vec<(u32, u32)>,
    draws: Draws,
    /// Per-frame scratch: the candidates, before they join the list.
    candidates: Vec<(u32, DrawIndexedIndirect, Bounds)>,
    bounds: Vec<Bounds>,
    /// Per-insert scratch: this node's vertices with `node_index` stamped in.
    /// Reused across inserts so streaming churn doesn't allocate and free a
    /// whole mesh's worth of vertices per node ([`insert`](Self::insert)).
    stamped: Vec<Vertex>,
    /// True while we've already warned about the *current* exhaustion episode,
    /// so a full arena logs once rather than once per rejected node. Cleared
    /// by the next successful insert, so a later episode is reported again
    /// instead of being silently swallowed for the rest of the process.
    warned_full: bool,
}

impl ChunkArena {
    /// Create the arena and its GPU buffers. `multi_draw` selects the fast indirect
    /// path; when false, drawing falls back to a per-chunk `draw_indexed` loop over
    /// the same shared buffers.
    pub fn new(device: &wgpu::Device, multi_draw: bool) -> Self {
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-vertices"),
            size: VERTEX_CAPACITY as u64 * std::mem::size_of::<Vertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-indices"),
            size: INDEX_CAPACITY as u64 * std::mem::size_of::<u32>() as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indirect_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-indirect"),
            size: MAX_DRAWS as u64 * std::mem::size_of::<DrawIndexedIndirect>() as u64,
            // STORAGE: the occlusion test switches candidates' draws on and off.
            usage: wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bounds_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-bounds"),
            size: MAX_DRAWS as u64 * std::mem::size_of::<Bounds>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let seen_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-seen"),
            size: MAX_DRAWS as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readbacks = (0..READBACKS)
            .map(|_| Readback {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("chunk-arena-seen-readback"),
                    size: MAX_DRAWS as u64 * 4,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: ReadState::Idle,
            })
            .collect();
        // vec4 (16 bytes) per node: storage buffers want their elements
        // aligned to 16 bytes anyway, and it leaves a spare float per entry
        // (unused today) rather than fighting alignment padding.
        let origins_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk-arena-node-origins"),
            size: MAX_NODES as u64 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let origins_bgl = crate::render::origins_bind_group_layout(device);
        let origins_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chunk-arena-node-origins-bind-group"),
            layout: &origins_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: origins_buffer.as_entire_binding(),
            }],
        });

        Self {
            vertex_buffer,
            index_buffer,
            indirect_buffer,
            origins_buffer,
            origins_bind_group,
            vertices: SlabAllocator::new(VERTEX_CAPACITY),
            indices: SlabAllocator::new(INDEX_CAPACITY),
            nodes: NodeIndexAllocator::new(),
            slots: BTreeMap::new(),
            multi_draw,
            visible: Vec::new(),
            visible_nodes: 0,
            visible_triangles: 0,
            bounds_buffer,
            seen_buffer,
            readbacks,
            occlusion: true,
            seen_in: vec![0; MAX_NODES as usize],
            frame: 0,
            seen_frame: 0,
            stats: OcclusionStats::default(),
            frame_list: Vec::new(),
            draws: Draws::default(),
            candidates: Vec::new(),
            bounds: Vec::new(),
            stamped: Vec::new(),
            warned_full: false,
        }
    }

    /// The bind group for `@group(1)` in `mesh.wgsl` -- the node-origins
    /// storage buffer this arena's resident chunks are indexed into.
    pub fn origins_bind_group(&self) -> &wgpu::BindGroup {
        &self.origins_bind_group
    }

    /// Mesh `chunk` (placed at `origin`, one lattice cell = `scale` world
    /// units) and upload it — the synchronous path used by the headless
    /// bench/screenshot/golden tests and by [`render_chunks`](crate::headless::render_chunks)'s
    /// explicit-chunk scenes. The live renderer instead meshes off-thread
    /// (`cubara_world::mesh::MeshPool`) and calls [`insert`](Self::insert)
    /// with the result directly. No-op if `id` is already resident or the
    /// chunk produced no geometry.
    pub fn upload_node(
        &mut self,
        queue: &wgpu::Queue,
        id: NodeId,
        origin: [f32; 3],
        scale: f32,
        chunk: &Chunk,
        ctx: &MeshContext,
    ) -> bool {
        match build_mesh_bounded(chunk, ctx, origin, scale) {
            Some((mesh, aabb)) => self.insert(queue, id, origin, scale, &mesh, aabb),
            None => false,
        }
    }

    /// Sub-allocate an already-built node-local `mesh` (with precomputed
    /// world-space `aabb`) into the shared arenas and upload it, plus a
    /// node-origins slot recording `origin`/`scale` for the vertex shader to
    /// add/multiply back (§5.2 -- vertices themselves stay node-local). No-op
    /// if `id` is already resident. Returns whether the geometry was added.
    /// This is the GPU-side step, kept separate from meshing so the latter
    /// can run on a worker thread or in a different crate entirely
    /// (`cubara_world::mesh`, since #110) -- `pub`, not `pub(crate)`, because
    /// "already-meshed data in, uploaded, here" is exactly that boundary.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        id: NodeId,
        origin: [f32; 3],
        scale: f32,
        mesh: &Mesh,
        aabb: Aabb,
    ) -> bool {
        if self.slots.contains_key(&id) {
            return false;
        }
        let vertex_len = mesh.vertices.len() as u32;
        let index_count = mesh.indices.len() as u32;

        // Attempt all three up front and name every result, so a partial
        // failure below can free exactly what *did* succeed rather than
        // guessing -- re-calling `alloc` in the failure branch would hand
        // back a fresh (different) offset, not the one that leaked.
        let base_vertex = self.vertices.alloc(vertex_len);
        let first_index = self.indices.alloc(index_count);
        let node_index = self.nodes.alloc();

        let (Some(base_vertex), Some(first_index), Some(node_index)) =
            (base_vertex, first_index, node_index)
        else {
            if let Some(v) = base_vertex {
                self.vertices.free(v, vertex_len);
            }
            if let Some(i) = first_index {
                self.indices.free(i, index_count);
            }
            if let Some(n) = node_index {
                self.nodes.free(n);
            }
            if !self.warned_full {
                log::warn!(
                    "chunk arena full (v {}/{}, i {}/{}, nodes {}/{}) — skipping chunks; \
                     raise capacity",
                    self.vertices.high_water,
                    VERTEX_CAPACITY,
                    self.indices.high_water,
                    INDEX_CAPACITY,
                    self.nodes.next,
                    MAX_NODES,
                );
                self.warned_full = true;
            }
            return false;
        };

        // `node_index` is only known now (the mesh was built off-thread, before
        // this node had an arena slot), so stamp it into every vertex here
        // rather than baking it into the mesh's own output. Into a reused
        // buffer: streaming churn calls this for every node that arrives, and
        // a fresh `Vec` per call would allocate, copy and free a whole mesh's
        // vertices each time, for a value that is one field wide.
        self.stamped.clear();
        self.stamped
            .extend(mesh.vertices.iter().map(|v| v.with_node_index(node_index)));
        queue.write_buffer(
            &self.vertex_buffer,
            base_vertex as u64 * std::mem::size_of::<Vertex>() as u64,
            bytemuck::cast_slice(&self.stamped),
        );
        queue.write_buffer(
            &self.index_buffer,
            first_index as u64 * std::mem::size_of::<u32>() as u64,
            bytemuck::cast_slice(&mesh.indices),
        );
        // `.w` carries the node's scale (world units per lattice step, §5.2/
        // §5.3): 1.0 at level 0, exactly matching every chunk's implicit
        // scale before nodes existed; `mesh.wgsl` multiplies it into the
        // local position before adding the origin.
        let [ox, oy, oz] = origin;
        queue.write_buffer(
            &self.origins_buffer,
            node_index as u64 * 16,
            bytemuck::bytes_of(&[ox, oy, oz, scale]),
        );

        // The arena took a node, so whatever episode of exhaustion the latch
        // was suppressing is over; arm it again for the next one.
        self.warned_full = false;
        self.slots.insert(
            id,
            NodeSlot {
                base_vertex,
                vertex_len,
                first_index,
                index_count,
                aabb,
                node_index,
                face_indices: mesh.is_grouped_by_face().then_some(mesh.face_indices),
            },
        );
        true
    }

    /// Free a node's slots back to the arenas. No-op if not resident.
    pub fn remove(&mut self, id: NodeId) {
        if let Some(slot) = self.slots.remove(&id) {
            self.vertices.free(slot.base_vertex, slot.vertex_len);
            self.indices.free(slot.first_index, slot.index_count);
            self.nodes.free(slot.node_index);
        }
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.slots.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Every resident node's `(base_vertex, first_index)`, keyed by node — the
    /// arena's actual GPU-slot layout. A normal `pub` introspection method
    /// (alongside `usage`/`bounds`/`len`), not test-only: the issue #83
    /// regression test (two different *insertion* orders of the same node
    /// batch must land every node at the same offsets once both orders are
    /// sorted first) lives in `crates/render/tests/` as an integration test
    /// against `cubara_world::mesh`, which can only see this crate's public
    /// surface -- `#[cfg(test)]` items compiled into *this* crate's own unit
    /// tests aren't visible there.
    pub fn slot_offsets(&self) -> std::collections::BTreeMap<NodeId, (u32, u32)> {
        self.slots
            .iter()
            .map(|(&node, slot)| (node, (slot.base_vertex, slot.first_index)))
            .collect()
    }

    /// A snapshot of how full the arena is against its fixed capacities — the
    /// "requested vs available" numbers a legible exhaustion report needs (see
    /// issue #89). Cheap: everything here is already tracked incrementally, so
    /// this is safe to call after every region load, not just on failure.
    pub fn usage(&self) -> ArenaUsage {
        ArenaUsage {
            vertices_used: self.vertices.high_water,
            vertices_capacity: VERTEX_CAPACITY,
            indices_used: self.indices.high_water,
            indices_capacity: INDEX_CAPACITY,
            resident_nodes: self.slots.len() as u32,
            max_draws: MAX_DRAWS,
        }
    }

    /// World-space bounds over all resident nodes, for framing a camera.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        if self.slots.is_empty() {
            return None;
        }
        let mut min = glam::Vec3::splat(f32::MAX);
        let mut max = glam::Vec3::splat(f32::MIN);
        for slot in self.slots.values() {
            min = min.min(slot.aabb.min);
            max = max.max(slot.aabb.max);
        }
        Some((min.to_array(), max.to_array()))
    }

    /// Turn occlusion culling on or off ([`crate::occlusion`]). On by default.
    /// Off, every node the frustum lets through is drawn in the first pass --
    /// which must give exactly the same image, only slower.
    pub fn set_occlusion(&mut self, on: bool) {
        self.occlusion = on;
    }

    pub(crate) fn occlusion(&self) -> bool {
        self.occlusion
    }

    /// Whether the newest occlusion results found `id` visible.
    #[cfg(test)]
    pub(crate) fn was_seen(&self, id: NodeId) -> bool {
        self.slots
            .get(&id)
            .is_some_and(|slot| self.seen_in[slot.node_index as usize] == self.seen_frame)
    }

    /// Nodes the last [`prepare`](Self::prepare) found in view with a face
    /// that could face the camera.
    pub fn visible_nodes(&self) -> u32 {
        self.visible_nodes
    }

    /// Triangles the last [`prepare`](Self::prepare) queued, before
    /// occlusion culling.
    pub fn visible_triangles(&self) -> u64 {
        self.visible_triangles
    }

    /// What the newest occlusion results that have been read back said.
    pub fn occlusion_stats(&self) -> OcclusionStats {
        self.stats
    }

    /// Frustum-cull the resident nodes, split the survivors between the two
    /// passes, and upload the list. Returns the split, for
    /// [`SceneFrame::draws`](crate::SceneFrame). Call once per frame, before
    /// encoding it.
    ///
    /// Nodes the newest occlusion results saw go first and are drawn outright;
    /// the rest follow as candidates, drawn only where the GPU's test finds
    /// them visible. Also takes in any results that have come back since the
    /// last call, and asks for the ones copied since to be mapped.
    pub fn prepare(&mut self, queue: &wgpu::Queue, frustum: &Frustum) -> Draws {
        puffin::profile_function!();
        self.collect_readbacks();
        self.frame += 1;
        self.visible.clear();
        self.bounds.clear();
        self.candidates.clear();
        self.frame_list.clear();
        // `slots` is a BTreeMap, so this iterates in `NodeId` order every frame,
        // regardless of the order workers finished meshing in. That makes the draw
        // list — and therefore the rendered frame — deterministic (issue #81), and
        // also makes the MAX_DRAWS cap below drop a stable set of nodes rather than
        // whichever ones a hash happened to visit last.
        //
        // Stable is the weaker half of it. `NodeId` compares `level` before `pos`,
        // so the tail this truncates is the *highest levels* — the coarsest nodes,
        // each covering 2^level chunks per axis out at the horizon (§6.1). Running
        // out of draws therefore sheds the most distant, least detailed geometry
        // first, which is the graceful direction to fail in. That property is
        // load-bearing and lives entirely in the field order of a struct with a
        // derived `Ord`, so it is pinned by
        // `node_ids_sort_by_level_first_so_truncation_drops_the_coarsest` rather
        // than by this comment.
        self.visible_nodes = 0;
        self.visible_triangles = 0;
        let eye = frustum.eye();
        // A node's draws: one per run of face directions that can face the
        // camera ([`faces_facing`]), at most three.
        let mut node_draws: Vec<DrawIndexedIndirect> = Vec::with_capacity(3);
        'nodes: for slot in self.slots.values() {
            if !frustum.intersects_aabb(&slot.aabb) {
                continue;
            }
            // A node is drawn whole or not at all: never half its faces
            // because the draw list ran out part-way through it.
            if (self.visible.len() + self.candidates.len()) as u32 + 3 > MAX_DRAWS {
                break 'nodes;
            }
            let draw = |first_index: u32, index_count: u32| DrawIndexedIndirect {
                index_count,
                instance_count: 1,
                first_index,
                base_vertex: slot.base_vertex as i32,
                // Not used to look up the node origin -- that index is
                // baked into vertex data instead (§5.3) -- so this is
                // always the default single-instance draw.
                first_instance: 0,
            };
            node_draws.clear();
            match slot.face_indices {
                None => node_draws.push(draw(slot.first_index, slot.index_count)),
                Some(counts) => {
                    // Adjacent directions that are both drawn share one draw. At
                    // most three runs: of each opposite pair (adjacent in `Face`
                    // order) a camera outside the box sees one.
                    let facing = faces_facing(&slot.aabb, eye);
                    let mut start = slot.first_index;
                    let mut run: Option<(u32, u32)> = None;
                    for (count, facing) in counts.into_iter().zip(facing) {
                        if count > 0 {
                            if facing {
                                run = Some(match run {
                                    Some((first, n)) => (first, n + count),
                                    None => (start, count),
                                });
                            } else if let Some((first, n)) = run.take() {
                                node_draws.push(draw(first, n));
                            }
                        }
                        start += count;
                    }
                    if let Some((first, n)) = run {
                        node_draws.push(draw(first, n));
                    }
                }
            }
            if node_draws.is_empty() {
                continue;
            }
            self.visible_nodes += 1;
            let bounds = Bounds {
                lo: slot.aabb.min.to_array(),
                hi: slot.aabb.max.to_array(),
                ..Default::default()
            };
            let first_pass =
                !self.occlusion || self.seen_in[slot.node_index as usize] == self.seen_frame;
            for &d in &node_draws {
                self.visible_triangles += d.index_count as u64 / 3;
                if first_pass {
                    self.visible.push(d);
                    self.bounds.push(bounds);
                    self.frame_list.push((slot.node_index, d.index_count / 3));
                } else {
                    self.candidates.push((slot.node_index, d, bounds));
                }
            }
        }
        let first = self.visible.len() as u32;
        for &(node, draw, bounds) in &self.candidates {
            self.visible.push(draw);
            self.bounds.push(bounds);
            self.frame_list.push((node, draw.index_count / 3));
        }
        if !self.visible.is_empty() {
            queue.write_buffer(
                &self.indirect_buffer,
                0,
                bytemuck::cast_slice(&self.visible),
            );
            queue.write_buffer(&self.bounds_buffer, 0, bytemuck::cast_slice(&self.bounds));
        }
        self.draws = Draws {
            first,
            candidates: self.visible.len() as u32 - first,
        };
        self.draws
    }

    /// The buffers the occlusion test reads and writes: every listed node's
    /// box, the indirect draw list, and one result per entry.
    pub(crate) fn occlusion_buffers(&self) -> (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer) {
        (
            &self.bounds_buffer,
            &self.indirect_buffer,
            &self.seen_buffer,
        )
    }

    /// Copy this frame's occlusion results out to be read back, if a readback
    /// buffer is free. Encode after the test.
    pub(crate) fn encode_readback(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let count = self.draws.total();
        if count == 0 {
            return;
        }
        let Some(readback) = self
            .readbacks
            .iter_mut()
            .find(|r| matches!(r.state, ReadState::Idle))
        else {
            return;
        };
        encoder.copy_buffer_to_buffer(&self.seen_buffer, 0, &readback.buffer, 0, count as u64 * 4);
        readback.state = ReadState::Copied(FrameList {
            frame: self.frame,
            first: self.draws.first,
            entries: std::mem::take(&mut self.frame_list),
        });
    }

    /// Take in results that have been mapped, and ask for the ones copied by
    /// earlier frames -- submitted by now -- to be mapped.
    fn collect_readbacks(&mut self) {
        for readback in &mut self.readbacks {
            let state = std::mem::replace(&mut readback.state, ReadState::Idle);
            readback.state = match state {
                ReadState::Idle => ReadState::Idle,
                ReadState::Copied(list) => {
                    let status = Arc::new(AtomicU8::new(0));
                    let done = Arc::clone(&status);
                    let bytes = list.entries.len() as u64 * 4;
                    if bytes == 0 {
                        ReadState::Idle
                    } else {
                        readback.buffer.slice(..bytes).map_async(
                            wgpu::MapMode::Read,
                            move |result| {
                                done.store(if result.is_ok() { 1 } else { 2 }, Ordering::Relaxed);
                            },
                        );
                        ReadState::Mapping(list, status)
                    }
                }
                ReadState::Mapping(list, status) => match status.load(Ordering::Relaxed) {
                    0 => ReadState::Mapping(list, status),
                    1 => {
                        let bytes = list.entries.len() as u64 * 4;
                        if list.frame > self.seen_frame {
                            let data = readback.buffer.slice(..bytes).get_mapped_range();
                            let verdicts: &[u32] = bytemuck::cast_slice(&data);
                            let mut stats = OcclusionStats {
                                tested: verdicts.len() as u32,
                                candidates: verdicts.len() as u32 - list.first,
                                ..Default::default()
                            };
                            for (i, (&(node, triangles), &verdict)) in
                                list.entries.iter().zip(verdicts).enumerate()
                            {
                                let first_pass = (i as u32) < list.first;
                                if verdict != 0 {
                                    self.seen_in[node as usize] = list.frame;
                                    stats.visible += 1;
                                }
                                if !first_pass && verdict == 0 {
                                    stats.hidden_candidates += 1;
                                }
                                if first_pass || verdict != 0 {
                                    stats.triangles_drawn += triangles as u64;
                                }
                            }
                            drop(data);
                            self.stats = stats;
                            self.seen_frame = list.frame;
                        }
                        readback.buffer.unmap();
                        ReadState::Idle
                    }
                    _ => ReadState::Idle,
                },
            };
        }
    }

    /// The first pass: the nodes drawn outright.
    pub fn encode_first(&self, pass: &mut wgpu::RenderPass<'_>, draws: Draws) {
        if draws.first == 0 {
            return;
        }
        self.bind(pass);
        if self.multi_draw {
            pass.multi_draw_indexed_indirect(&self.indirect_buffer, 0, draws.first);
        } else {
            for draw in &self.visible[..draws.first as usize] {
                pass.draw_indexed(
                    draw.first_index..draw.first_index + draw.index_count,
                    draw.base_vertex,
                    0..1,
                );
            }
        }
    }

    /// The second pass: the candidates, each drawn with the instance count the
    /// occlusion test wrote -- one if visible, none if not. Without
    /// occlusion there are none.
    pub fn encode_candidates(&self, pass: &mut wgpu::RenderPass<'_>, draws: Draws) {
        if draws.candidates == 0 {
            return;
        }
        self.bind(pass);
        let size = std::mem::size_of::<DrawIndexedIndirect>() as u64;
        let offset = draws.first as u64 * size;
        if self.multi_draw {
            pass.multi_draw_indexed_indirect(&self.indirect_buffer, offset, draws.candidates);
        } else {
            for i in 0..draws.candidates as u64 {
                pass.draw_indexed_indirect(&self.indirect_buffer, offset + i * size);
            }
        }
    }

    fn bind(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    }

    /// Build and upload every node in `meshed` into a fresh arena — the
    /// headless bench/screenshot/golden-test entry point. Unlike the live
    /// renderer (which streams nodes in over several frames via a worker
    /// pool), these callers mesh a whole scene up front and just want it
    /// uploaded: `meshed` is expected to already be filtered to
    /// actually-non-empty nodes (whoever built it, e.g.
    /// `cubara_world::mesh::mesh_region`, already knows which nodes had no
    /// geometry) and sorted (`cubara_world::mesh::sort_batch`'s ordering) --
    /// this function does no filtering or reordering of its own, so it stays
    /// ignorant of what a "node" even is beyond the plain data in
    /// [`MeshedNode`].
    pub fn from_meshed(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        multi_draw: bool,
        meshed: impl IntoIterator<Item = MeshedNode>,
    ) -> Self {
        let mut arena = Self::new(device, multi_draw);
        let mut total_tris = 0u32;
        for node in meshed {
            if arena.insert(
                queue,
                node.id,
                node.origin,
                node.scale,
                &node.mesh,
                node.aabb,
            ) {
                if let Some(slot) = arena.slots.get(&node.id) {
                    total_tris += slot.index_count / 3;
                }
            }
        }
        let usage = arena.usage();
        log::info!(
            "{} nodes meshed, {total_tris} triangles (arena v {}/{}, i {}/{}, d {}/{})",
            usage.resident_nodes,
            usage.vertices_used,
            usage.vertices_capacity,
            usage.indices_used,
            usage.indices_capacity,
            usage.resident_nodes,
            usage.max_draws,
        );
        let exhausted = usage.exhausted();
        if !exhausted.is_empty() {
            log::warn!(
                "region exceeds arena capacity: {} — requested {} resident nodes (worst case \
                 {} draws) against {} vertices/{} indices/{} draws available; excess nodes are \
                 silently dropped from whichever frame's visible set overflows first (arena.rs \
                 MAX_DRAWS/VERTEX_CAPACITY/INDEX_CAPACITY). See issue #89.",
                exhausted.join(", "),
                usage.resident_nodes,
                usage.resident_nodes,
                usage.vertices_capacity,
                usage.indices_capacity,
                usage.max_draws,
            );
        }
        arena
    }
}

/// One already-meshed node, ready for [`ChunkArena::from_meshed`] to upload --
/// everything [`ChunkArena::insert`] needs, bundled so a caller building a
/// whole scene can collect a plain `Vec<MeshedNode>` rather than four parallel
/// ones.
pub struct MeshedNode {
    pub id: NodeId,
    pub origin: [f32; 3],
    pub scale: f32,
    pub mesh: Mesh,
    pub aabb: Aabb,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_is_contiguous_and_bump_style_when_empty() {
        let mut a = SlabAllocator::new(100);
        assert_eq!(a.alloc(10), Some(0));
        assert_eq!(a.alloc(20), Some(10));
        assert_eq!(a.alloc(5), Some(30));
        assert_eq!(a.high_water, 35);
    }

    #[test]
    fn alloc_none_when_too_big() {
        let mut a = SlabAllocator::new(16);
        assert_eq!(a.alloc(20), None);
        // The failed request left the arena untouched.
        assert_eq!(a.alloc(16), Some(0));
    }

    #[test]
    fn freed_slot_is_reused() {
        let mut a = SlabAllocator::new(100);
        let x = a.alloc(10).unwrap();
        let _y = a.alloc(10).unwrap();
        a.free(x, 10);
        // First-fit picks the just-freed hole at the front.
        assert_eq!(a.alloc(10), Some(x));
    }

    #[test]
    fn adjacent_frees_coalesce_into_one_range() {
        let mut a = SlabAllocator::new(30);
        let x = a.alloc(10).unwrap();
        let y = a.alloc(10).unwrap();
        let z = a.alloc(10).unwrap();
        // Free the two ends, then the middle — everything should merge back so a
        // full-capacity allocation succeeds again.
        a.free(x, 10);
        a.free(z, 10);
        a.free(y, 10);
        assert_eq!(a.free.len(), 1);
        assert_eq!(a.alloc(30), Some(0));
    }

    #[test]
    fn coalesce_with_next_only() {
        let mut a = SlabAllocator::new(30);
        let x = a.alloc(10).unwrap();
        let y = a.alloc(10).unwrap();
        let _z = a.alloc(10).unwrap();
        // Free y first (no left neighbour free), then x merges left-to-right.
        a.free(y, 10);
        a.free(x, 10);
        assert_eq!(a.free.len(), 1);
        assert_eq!(a.free[0], (0, 20));
    }

    #[test]
    fn node_ids_sort_by_level_first_so_truncation_drops_the_coarsest() {
        // `prepare` walks `slots` (a BTreeMap keyed by NodeId) in order and
        // stops at MAX_DRAWS. *Which* nodes that drops is decided entirely by
        // NodeId's derived `Ord`, which compares `level` before `pos` -- so
        // the truncated tail is the highest levels, and a higher level is a
        // coarser node covering more world further away. Shedding the horizon
        // is the graceful failure; shedding the ground under the player is the
        // opposite one.
        //
        // This holds *only* because `level` is declared before `pos`. Swapping
        // those two fields still compiles, still derives `Ord`, and still
        // passes every other test in this crate -- while silently inverting
        // which geometry survives a full draw list. Nothing else pins it.
        let near_fine = NodeId {
            level: 0,
            pos: [i32::MAX, i32::MAX, i32::MAX],
        };
        let far_coarse = NodeId {
            level: 1,
            pos: [i32::MIN, i32::MIN, i32::MIN],
        };
        assert!(
            near_fine < far_coarse,
            "a level-0 node must sort before any level-1 node, whatever their positions"
        );
    }

    #[test]
    fn truncating_the_slot_order_keeps_the_finest_levels() {
        // The same walk `prepare` does, against a mixed-level set: iterate in
        // key order, stop at a cap, and check what survived.
        let mut slots = BTreeMap::new();
        for level in 0..4u32 {
            for x in 0..4i32 {
                slots.insert(
                    NodeId {
                        level,
                        pos: [x, 0, 0],
                    },
                    (),
                );
            }
        }
        let kept: Vec<NodeId> = slots.keys().copied().take(6).collect();
        assert!(
            kept.iter().all(|n| n.level <= 1),
            "a truncated draw list must keep the finest levels, got {kept:?}"
        );
    }

    #[test]
    fn a_direction_is_left_out_only_when_no_face_of_it_could_face_the_camera() {
        // Faces on planes anywhere inside the box, cameras all around it: a
        // face the camera is in front of must never have its direction left
        // out, and a direction left out must be one no plane in the box could
        // show -- so the test cannot pass by drawing everything either.
        let aabb = Aabb::new(glam::vec3(16.0, -32.0, 48.0), glam::vec3(32.0, 0.0, 80.0));
        let normals = [
            glam::Vec3::X,
            -glam::Vec3::X,
            glam::Vec3::Y,
            -glam::Vec3::Y,
            glam::Vec3::Z,
            -glam::Vec3::Z,
        ];
        let mut left_out = 0;
        let steps = [
            -40.0, 0.0, 15.9, 16.0, 16.1, 24.0, 31.9, 32.0, 32.1, 60.0, 90.0,
        ];
        for &ex in &steps {
            for &ey in &steps {
                for &ez in &steps {
                    let eye = glam::vec3(ex, ey - 32.0, ez + 32.0);
                    let facing = faces_facing(&aabb, eye);
                    for (k, normal) in normals.iter().enumerate() {
                        // A face on each plane through the box along this axis.
                        let axis = k / 2;
                        let (lo, hi) = (aabb.min[axis], aabb.max[axis]);
                        let seen_on_some_plane = (0..=16).any(|i| {
                            let mut on_plane = aabb.min;
                            on_plane[axis] = lo + (hi - lo) * i as f32 / 16.0;
                            (eye - on_plane).dot(*normal) > 0.0
                        });
                        if seen_on_some_plane {
                            assert!(
                                facing[k],
                                "eye {eye}: direction {k} left out but a face shows"
                            );
                        }
                        if !facing[k] {
                            left_out += 1;
                        }
                    }
                }
            }
        }
        assert!(left_out > 0, "nothing was ever left out");
    }

    #[test]
    fn draw_indexed_indirect_is_tightly_packed_20_bytes() {
        assert_eq!(std::mem::size_of::<DrawIndexedIndirect>(), 20);
    }
}
