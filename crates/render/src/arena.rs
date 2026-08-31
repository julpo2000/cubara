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
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
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

    /// CPU frustum-cull the resident nodes and upload the visible set's indirect
    /// draw args. Returns the number of visible nodes (the draw count). Call once
    /// per frame, before beginning the render pass; then [`encode`](Self::encode).
    pub fn prepare(&mut self, queue: &wgpu::Queue, frustum: &Frustum) -> u32 {
        puffin::profile_function!();
        self.visible.clear();
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
        for slot in self.slots.values() {
            if self.visible.len() as u32 >= MAX_DRAWS {
                break;
            }
            if frustum.intersects_aabb(&slot.aabb) {
                self.visible.push(DrawIndexedIndirect {
                    index_count: slot.index_count,
                    instance_count: 1,
                    first_index: slot.first_index,
                    base_vertex: slot.base_vertex as i32,
                    // Not used to look up the node origin -- that index is
                    // baked into vertex data instead (§5.3) -- so this is
                    // always the default single-instance draw.
                    first_instance: 0,
                });
            }
        }
        if !self.visible.is_empty() {
            queue.write_buffer(
                &self.indirect_buffer,
                0,
                bytemuck::cast_slice(&self.visible),
            );
        }
        self.visible.len() as u32
    }

    /// Bind the shared buffers and issue the draws for the `count` visible chunks
    /// prepared this frame — one `multi_draw_indexed_indirect` on MDI backends, or a
    /// `draw_indexed` loop over the same buffers otherwise.
    pub fn encode(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        if count == 0 {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        if self.multi_draw {
            pass.multi_draw_indexed_indirect(&self.indirect_buffer, 0, count);
        } else {
            for draw in &self.visible[..count as usize] {
                pass.draw_indexed(
                    draw.first_index..draw.first_index + draw.index_count,
                    draw.base_vertex,
                    0..1,
                );
            }
        }
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
    fn draw_indexed_indirect_is_tightly_packed_20_bytes() {
        assert_eq!(std::mem::size_of::<DrawIndexedIndirect>(), 20);
    }
}
