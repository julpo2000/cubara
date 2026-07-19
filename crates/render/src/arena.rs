//! Shared chunk-geometry arena for GPU-driven rendering.
//!
//! Instead of one vertex/index buffer (and one draw call) per chunk, every
//! resident chunk's geometry lives in a pair of large, pooled GPU buffers — a
//! vertex arena and an index arena — with per-chunk sub-allocations. Streaming
//! churn (chunks constantly loading/unloading) is absorbed by a first-fit,
//! coalescing [`SlabAllocator`] over each arena, so freed slots are reused.
//!
//! Per frame, the CPU frustum-culls the resident chunks, writes one
//! [`DrawIndexedIndirect`] entry per visible chunk into an indirect-args buffer,
//! and issues a single `multi_draw_indexed_indirect` — collapsing ~1,350 draws
//! into one submit (see issue #27 / `PLAN.md` §10). Backends without
//! `MULTI_DRAW_INDIRECT` (checked via the spike, #26) fall back to a loop of
//! `draw_indexed` over the *same* shared buffers, so there is no second geometry
//! path to maintain.
//!
//! The per-chunk metadata this builds (AABB + geometry offsets) is exactly what
//! the follow-up compute cull (#28) consumes; only *who writes the draw list*
//! moves from CPU to GPU. No throwaway work.

use std::collections::HashMap;

use cubara_voxel::{Chunk, ChunkCoord, Mesh, Vertex};
use cubara_world::{streaming, World};

use crate::culling::{Aabb, Frustum};

/// Mesh a chunk into world space at LOD `level` and compute its bounds — the
/// CPU-heavy part of getting a chunk on screen, split out so it can run on a worker
/// thread (see [`crate::mesher`]). Returns `None` for a chunk that produces no
/// geometry. `level` 0 is full resolution; higher is coarser (see
/// [`build_mesh_lod`](cubara_voxel::Chunk::build_mesh_lod)).
pub(crate) fn build_chunk_mesh(
    coord: ChunkCoord,
    chunk: &Chunk,
    level: u32,
) -> Option<(Mesh, Aabb)> {
    let mut mesh = chunk.build_mesh_lod(level);
    mesh.translate(coord.world_offset());
    if mesh.indices.is_empty() {
        return None;
    }
    let mut min = glam::Vec3::splat(f32::MAX);
    let mut max = glam::Vec3::splat(f32::MIN);
    for v in &mesh.vertices {
        let p = glam::Vec3::from(v.position);
        min = min.min(p);
        max = max.max(p);
    }
    Some((mesh, Aabb::new(min, max)))
}

/// Vertex-arena capacity, in vertices (~112 MiB at 28 bytes/vertex). The heaviest
/// current scene — the radius-12 bench — peaks well under this, leaving ample
/// headroom for streaming fragmentation and the denser terrain still to come.
const VERTEX_CAPACITY: u32 = 4_000_000;
/// Index-arena capacity, in indices (~24 MiB at 4 bytes/index). Same ~9× headroom
/// over the radius-12 bench peak of ~653k indices.
const INDEX_CAPACITY: u32 = 6_000_000;
/// Max chunks the indirect-args buffer can hold (upper bound on visible chunks).
/// 16k covers a square radius of ~52 chunks across the 3-high vertical band.
const MAX_DRAWS: u32 = 16_384;

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

/// Where one chunk's geometry lives inside the shared arenas, plus its world-space
/// bounds for culling. This is the per-chunk metadata the GPU compute cull (#28)
/// will read straight from a storage buffer.
#[derive(Clone, Copy)]
struct ChunkSlot {
    /// First vertex of this chunk in the vertex arena (used as `base_vertex`).
    base_vertex: u32,
    vertex_len: u32,
    /// First index of this chunk in the index arena.
    first_index: u32,
    index_count: u32,
    aabb: Aabb,
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

/// Every resident chunk's geometry in shared vertex/index buffers, drawn with one
/// indirect submit (or a `draw_indexed` loop on backends without MDI).
pub struct ChunkArena {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    /// One `DrawIndexedIndirect` per visible chunk, rewritten each frame.
    indirect_buffer: wgpu::Buffer,

    vertices: SlabAllocator,
    indices: SlabAllocator,
    slots: HashMap<ChunkCoord, ChunkSlot>,

    /// Whether the device supports `multi_draw_indexed_indirect`.
    multi_draw: bool,
    /// Per-frame scratch: the visible draw list built by [`prepare`](Self::prepare).
    visible: Vec<DrawIndexedIndirect>,
    /// True once we've warned about a full arena, so we log it only once.
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

        Self {
            vertex_buffer,
            index_buffer,
            indirect_buffer,
            vertices: SlabAllocator::new(VERTEX_CAPACITY),
            indices: SlabAllocator::new(INDEX_CAPACITY),
            slots: HashMap::new(),
            multi_draw,
            visible: Vec::new(),
            warned_full: false,
        }
    }

    /// Mesh `chunk` at LOD `level` and upload it — the synchronous path used by the
    /// headless bench/screenshot. The live renderer instead meshes on a worker thread
    /// (via [`build_chunk_mesh`]) and calls [`insert`](Self::insert) with the result.
    /// No-op if the chunk is already resident or produced no geometry.
    pub fn upload_chunk(
        &mut self,
        queue: &wgpu::Queue,
        coord: ChunkCoord,
        chunk: &Chunk,
        level: u32,
    ) -> bool {
        match build_chunk_mesh(coord, chunk, level) {
            Some((mesh, aabb)) => self.insert(queue, coord, &mesh, aabb),
            None => false,
        }
    }

    /// Sub-allocate an already-built world-space `mesh` (with precomputed `aabb`)
    /// into the shared arenas and upload it. No-op if `coord` is already resident.
    /// Returns whether the geometry was added. This is the GPU-side step, kept off
    /// the meshing so the latter can run on worker threads.
    pub(crate) fn insert(
        &mut self,
        queue: &wgpu::Queue,
        coord: ChunkCoord,
        mesh: &Mesh,
        aabb: Aabb,
    ) -> bool {
        if self.slots.contains_key(&coord) {
            return false;
        }
        let vertex_len = mesh.vertices.len() as u32;
        let index_count = mesh.indices.len() as u32;
        let (Some(base_vertex), Some(first_index)) = (
            self.vertices.alloc(vertex_len),
            self.indices.alloc(index_count),
        ) else {
            // Roll back a partial allocation so the arenas stay consistent.
            if let Some(v) = self.vertices.alloc(vertex_len) {
                self.vertices.free(v, vertex_len);
            }
            if !self.warned_full {
                log::warn!(
                    "chunk arena full (v {}/{}, i {}/{}) — skipping chunks; raise capacity",
                    self.vertices.high_water,
                    VERTEX_CAPACITY,
                    self.indices.high_water,
                    INDEX_CAPACITY
                );
                self.warned_full = true;
            }
            return false;
        };

        queue.write_buffer(
            &self.vertex_buffer,
            base_vertex as u64 * std::mem::size_of::<Vertex>() as u64,
            bytemuck::cast_slice(&mesh.vertices),
        );
        queue.write_buffer(
            &self.index_buffer,
            first_index as u64 * std::mem::size_of::<u32>() as u64,
            bytemuck::cast_slice(&mesh.indices),
        );

        self.slots.insert(
            coord,
            ChunkSlot {
                base_vertex,
                vertex_len,
                first_index,
                index_count,
                aabb,
            },
        );
        true
    }

    /// Free a chunk's slots back to the arenas. No-op if not resident.
    pub fn remove(&mut self, coord: ChunkCoord) {
        if let Some(slot) = self.slots.remove(&coord) {
            self.vertices.free(slot.base_vertex, slot.vertex_len);
            self.indices.free(slot.first_index, slot.index_count);
        }
    }

    pub fn contains(&self, coord: ChunkCoord) -> bool {
        self.slots.contains_key(&coord)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// World-space bounds over all resident chunks, for framing a camera.
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

    /// CPU frustum-cull the resident chunks and upload the visible set's indirect
    /// draw args. Returns the number of visible chunks (the draw count). Call once
    /// per frame, before beginning the render pass; then [`encode`](Self::encode).
    pub fn prepare(&mut self, queue: &wgpu::Queue, frustum: &Frustum) -> u32 {
        puffin::profile_function!();
        self.visible.clear();
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

    /// Build and upload every non-empty chunk in a square region, each at its
    /// distance-based LOD ([`streaming::lod_for`]) — the same scene and detail
    /// falloff the live renderer streams, exposed so the headless bench/screenshot
    /// build and draw it too.
    pub fn from_region(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        multi_draw: bool,
        center: ChunkCoord,
        radius: i32,
        y_range: std::ops::RangeInclusive<i32>,
    ) -> Self {
        let mut arena = Self::new(device, multi_draw);
        let mut total_tris = 0u32;
        for coord in streaming::desired_chunks(center, radius, y_range) {
            if let Some(chunk) = World::chunk_at(coord) {
                let level = streaming::lod_for(coord, center);
                if arena.upload_chunk(queue, coord, &chunk, level) {
                    if let Some(slot) = arena.slots.get(&coord) {
                        total_tris += slot.index_count / 3;
                    }
                }
            }
        }
        log::info!(
            "region radius {radius}: {} chunks meshed (distance LOD), {total_tris} triangles \
             (arena v {}/{}, i {}/{})",
            arena.slots.len(),
            arena.vertices.high_water,
            VERTEX_CAPACITY,
            arena.indices.high_water,
            INDEX_CAPACITY,
        );
        arena
    }
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
    fn draw_indexed_indirect_is_tightly_packed_20_bytes() {
        assert_eq!(std::mem::size_of::<DrawIndexedIndirect>(), 20);
    }
}
