//! Shared chunk-geometry arena + indirect multi-draw (M3.5 Step 1).
//!
//! Instead of one vertex/index buffer and one draw call per chunk, all resident
//! chunk geometry lives in a **single pooled vertex buffer + index buffer**, with
//! each chunk occupying a sub-range (a [`FreeList`] allocator hands out and reuses
//! slots as the world streams in and out). A per-chunk metadata table keeps the
//! buffer offsets, index count, and world-space AABB.
//!
//! Each frame the CPU frustum-culls against those AABBs, writes one
//! `DrawIndexedIndirect` record per visible chunk into an indirect-args buffer, and
//! issues a **single `multi_draw_indexed_indirect`** — collapsing ~1,350 draw calls
//! into one submit. Backends without `MULTI_DRAW_INDIRECT` fall back to a loop of
//! `draw_indexed` over the *same* shared buffers (still one bind, no per-chunk
//! buffers), so nothing regresses there.
//!
//! This is the foundation the fully GPU-driven path (Step 2, #28) builds on: the
//! shared buffers + metadata are exactly what a compute cull needs; only "who
//! writes the draw list" then moves from CPU to GPU.

use std::collections::HashMap;

use cubara_voxel::{Chunk, ChunkCoord, Vertex};
use cubara_world::{streaming, World};

use crate::culling::{Aabb, Frustum};

/// Bytes per vertex in the shared vertex buffer.
const VERTEX_STRIDE: u64 = std::mem::size_of::<Vertex>() as u64;
/// Bytes per index (indices are `u32`).
const INDEX_STRIDE: u64 = std::mem::size_of::<u32>() as u64;

/// Initial slot counts; the arena grows geometrically when these are exceeded.
const INITIAL_VERTICES: u32 = 1 << 16;
const INITIAL_INDICES: u32 = 1 << 17;
const INITIAL_DRAWS: u32 = 512;

/// One indexed indirect-draw record, matching the GPU layout
/// `multi_draw_indexed_indirect` reads
/// (`index_count, instance_count, first_index, base_vertex, first_instance`).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawIndexedIndirect {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

/// A chunk's sub-allocation within the shared buffers, plus its cull bounds.
struct Slot {
    base_vertex: u32,
    vertex_count: u32,
    first_index: u32,
    index_count: u32,
    aabb: Aabb,
}

/// A first-fit free-list over a 1-D range of fixed-size elements (vertices or
/// indices). Frees are coalesced with their neighbours so streaming churn doesn't
/// fragment the arena into unusable slivers.
struct FreeList {
    /// Free `(offset, len)` blocks, sorted by offset and coalesced.
    free: Vec<(u32, u32)>,
    capacity: u32,
}

impl FreeList {
    fn new(capacity: u32) -> Self {
        Self {
            free: vec![(0, capacity)],
            capacity,
        }
    }

    /// Reserve `n` contiguous elements, returning the start offset, or `None` if no
    /// single free block is large enough (the caller then grows and retries).
    fn alloc(&mut self, n: u32) -> Option<u32> {
        for i in 0..self.free.len() {
            let (off, len) = self.free[i];
            if len >= n {
                if len == n {
                    self.free.remove(i);
                } else {
                    self.free[i] = (off + n, len - n);
                }
                return Some(off);
            }
        }
        None
    }

    /// Return a `[offset, offset + n)` block to the pool, coalescing neighbours.
    fn free_block(&mut self, offset: u32, n: u32) {
        let pos = self.free.partition_point(|&(o, _)| o < offset);
        self.free.insert(pos, (offset, n));
        // Merge with the following block if adjacent.
        if pos + 1 < self.free.len() {
            let (o, l) = self.free[pos];
            let (no, nl) = self.free[pos + 1];
            if o + l == no {
                self.free[pos] = (o, l + nl);
                self.free.remove(pos + 1);
            }
        }
        // Merge with the preceding block if adjacent.
        if pos > 0 {
            let (po, pl) = self.free[pos - 1];
            let (o, l) = self.free[pos];
            if po + pl == o {
                self.free[pos - 1] = (po, pl + l);
                self.free.remove(pos);
            }
        }
    }

    /// Extend the pool to `new_capacity`, adding the new tail as free space.
    fn grow_to(&mut self, new_capacity: u32) {
        debug_assert!(new_capacity > self.capacity);
        let added = new_capacity - self.capacity;
        let old_capacity = self.capacity;
        self.free_block(old_capacity, added);
        self.capacity = new_capacity;
    }
}

/// All resident chunk geometry in shared GPU buffers, drawn with one indirect
/// submit (or a `draw_indexed` fallback where `MULTI_DRAW_INDIRECT` is missing).
pub struct ChunkArena {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    indirect_buffer: wgpu::Buffer,

    vertex_alloc: FreeList,
    index_alloc: FreeList,
    indirect_capacity: u32,

    chunks: HashMap<ChunkCoord, Slot>,
    /// Per-frame scratch: the visible draw list, built in [`Self::prepare`] and
    /// consumed in [`Self::encode`]. Kept across frames to avoid reallocating.
    draws: Vec<DrawIndexedIndirect>,

    /// Whether indexed indirect multi-draw is available (else the fallback is used).
    multi_draw: bool,
}

impl ChunkArena {
    pub fn new(device: &wgpu::Device, multi_draw: bool) -> Self {
        let vertex_buffer = new_geometry_buffer(
            device,
            "chunk-arena-vertices",
            INITIAL_VERTICES as u64 * VERTEX_STRIDE,
            wgpu::BufferUsages::VERTEX,
        );
        let index_buffer = new_geometry_buffer(
            device,
            "chunk-arena-indices",
            INITIAL_INDICES as u64 * INDEX_STRIDE,
            wgpu::BufferUsages::INDEX,
        );
        let indirect_buffer = new_indirect_buffer(device, INITIAL_DRAWS);

        Self {
            vertex_buffer,
            index_buffer,
            indirect_buffer,
            vertex_alloc: FreeList::new(INITIAL_VERTICES),
            index_alloc: FreeList::new(INITIAL_INDICES),
            indirect_capacity: INITIAL_DRAWS,
            chunks: HashMap::new(),
            draws: Vec::new(),
            multi_draw,
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn triangle_count(&self) -> u32 {
        self.chunks.values().map(|s| s.index_count / 3).sum()
    }

    pub fn contains(&self, coord: ChunkCoord) -> bool {
        self.chunks.contains_key(&coord)
    }

    /// Mesh `chunk` (baking in its world offset), upload it into the shared buffers,
    /// and record its metadata. A chunk that produces no geometry is skipped.
    pub fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        coord: ChunkCoord,
        chunk: &Chunk,
    ) {
        let mut mesh = chunk.build_mesh();
        mesh.translate(coord.world_offset());
        if mesh.indices.is_empty() {
            return;
        }
        // Re-inserting a resident coord (e.g. a re-mesh) reuses its slot cleanly.
        if self.chunks.contains_key(&coord) {
            self.remove(coord);
        }

        let mut min = glam::Vec3::splat(f32::MAX);
        let mut max = glam::Vec3::splat(f32::MIN);
        for v in &mesh.vertices {
            let p = glam::Vec3::from(v.position);
            min = min.min(p);
            max = max.max(p);
        }

        let vertex_count = mesh.vertices.len() as u32;
        let index_count = mesh.indices.len() as u32;
        let base_vertex = self.alloc_vertices(device, queue, vertex_count);
        let first_index = self.alloc_indices(device, queue, index_count);

        queue.write_buffer(
            &self.vertex_buffer,
            base_vertex as u64 * VERTEX_STRIDE,
            bytemuck::cast_slice(&mesh.vertices),
        );
        queue.write_buffer(
            &self.index_buffer,
            first_index as u64 * INDEX_STRIDE,
            bytemuck::cast_slice(&mesh.indices),
        );

        self.chunks.insert(
            coord,
            Slot {
                base_vertex,
                vertex_count,
                first_index,
                index_count,
                aabb: Aabb::new(min, max),
            },
        );
        self.ensure_indirect_capacity(device);
    }

    /// Free a chunk's slots (no-op if it had no geometry / wasn't resident).
    pub fn remove(&mut self, coord: ChunkCoord) {
        if let Some(slot) = self.chunks.remove(&coord) {
            self.vertex_alloc
                .free_block(slot.base_vertex, slot.vertex_count);
            self.index_alloc
                .free_block(slot.first_index, slot.index_count);
        }
    }

    /// Frustum-cull the resident chunks and stage the visible draw list into the
    /// indirect buffer. Returns the number of visible chunks. Must be called each
    /// frame before [`Self::encode`].
    pub fn prepare(&mut self, queue: &wgpu::Queue, frustum: &Frustum) -> u32 {
        self.draws.clear();
        for slot in self.chunks.values() {
            if frustum.intersects_aabb(&slot.aabb) {
                self.draws.push(slot.draw_args());
            }
        }
        self.upload_draws(queue);
        self.draws.len() as u32
    }

    /// Stage *all* resident chunks (no culling) — used by the screenshot path, which
    /// frames the whole region. Returns the number of chunks.
    pub fn prepare_all(&mut self, queue: &wgpu::Queue) -> u32 {
        self.draws.clear();
        self.draws
            .extend(self.chunks.values().map(|slot| slot.draw_args()));
        self.upload_draws(queue);
        self.draws.len() as u32
    }

    /// Record the staged draw list into `pass`: one `multi_draw_indexed_indirect`
    /// over the shared buffers, or a `draw_indexed` loop where MDI is unavailable.
    pub fn encode(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        if self.multi_draw {
            pass.multi_draw_indexed_indirect(&self.indirect_buffer, 0, self.draws.len() as u32);
        } else {
            for d in &self.draws {
                pass.draw_indexed(
                    d.first_index..d.first_index + d.index_count,
                    d.base_vertex,
                    0..1,
                );
            }
        }
    }

    /// World-space bounds over all resident chunks (`[min]`, `[max]`), for framing a
    /// camera on the scene. Returns zeros when the arena is empty.
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for slot in self.chunks.values() {
            let lo = [slot.aabb.min.x, slot.aabb.min.y, slot.aabb.min.z];
            let hi = [slot.aabb.max.x, slot.aabb.max.y, slot.aabb.max.z];
            for (m, v) in min.iter_mut().zip(lo) {
                *m = m.min(v);
            }
            for (m, v) in max.iter_mut().zip(hi) {
                *m = m.max(v);
            }
        }
        if self.chunks.is_empty() {
            ([0.0; 3], [0.0; 3])
        } else {
            (min, max)
        }
    }

    fn upload_draws(&self, queue: &wgpu::Queue) {
        if !self.draws.is_empty() {
            queue.write_buffer(&self.indirect_buffer, 0, bytemuck::cast_slice(&self.draws));
        }
    }

    /// Reserve `n` vertices, growing (and copying) the shared vertex buffer if the
    /// current pool can't satisfy the request.
    fn alloc_vertices(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, n: u32) -> u32 {
        loop {
            if let Some(off) = self.vertex_alloc.alloc(n) {
                return off;
            }
            let old_capacity = self.vertex_alloc.capacity;
            let new_capacity = grown_capacity(old_capacity, n);
            self.vertex_buffer = grow_buffer(
                device,
                queue,
                "chunk-arena-vertices",
                &self.vertex_buffer,
                old_capacity as u64 * VERTEX_STRIDE,
                new_capacity as u64 * VERTEX_STRIDE,
                wgpu::BufferUsages::VERTEX,
            );
            self.vertex_alloc.grow_to(new_capacity);
        }
    }

    /// Reserve `n` indices, growing the shared index buffer if needed.
    fn alloc_indices(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, n: u32) -> u32 {
        loop {
            if let Some(off) = self.index_alloc.alloc(n) {
                return off;
            }
            let old_capacity = self.index_alloc.capacity;
            let new_capacity = grown_capacity(old_capacity, n);
            self.index_buffer = grow_buffer(
                device,
                queue,
                "chunk-arena-indices",
                &self.index_buffer,
                old_capacity as u64 * INDEX_STRIDE,
                new_capacity as u64 * INDEX_STRIDE,
                wgpu::BufferUsages::INDEX,
            );
            self.index_alloc.grow_to(new_capacity);
        }
    }

    /// Ensure the indirect buffer can hold one record per resident chunk (the worst
    /// case for a frame where everything is visible). Rewritten every frame, so a
    /// bigger buffer just replaces the old one — no copy needed.
    fn ensure_indirect_capacity(&mut self, device: &wgpu::Device) {
        let needed = self.chunks.len() as u32;
        if needed > self.indirect_capacity {
            let new_capacity = grown_capacity(self.indirect_capacity, needed);
            self.indirect_buffer = new_indirect_buffer(device, new_capacity);
            self.indirect_capacity = new_capacity;
        }
    }
}

impl Slot {
    fn draw_args(&self) -> DrawIndexedIndirect {
        DrawIndexedIndirect {
            index_count: self.index_count,
            instance_count: 1,
            first_index: self.first_index,
            base_vertex: self.base_vertex as i32,
            first_instance: 0,
        }
    }
}

/// Build and upload every non-empty chunk in a square region into a fresh arena —
/// the same streaming path the live renderer uses, exposed so the headless bench and
/// screenshot measure and draw the exact same scene.
pub fn build_region(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    center: ChunkCoord,
    radius: i32,
    y_range: std::ops::RangeInclusive<i32>,
    multi_draw: bool,
) -> ChunkArena {
    let mut arena = ChunkArena::new(device, multi_draw);
    for coord in streaming::desired_chunks(center, radius, y_range) {
        if let Some(chunk) = World::chunk_at(coord) {
            arena.insert(device, queue, coord, &chunk);
        }
    }
    log::info!(
        "region radius {radius}: {} chunks meshed, {} triangles",
        arena.chunk_count(),
        arena.triangle_count(),
    );
    arena
}

/// Next capacity when a pool of `current` elements can't fit `needed` more: at least
/// double, but always enough for the request in one step.
fn grown_capacity(current: u32, needed: u32) -> u32 {
    current.saturating_mul(2).max(current + needed).max(1)
}

fn new_geometry_buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    kind: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: kind | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

fn new_indirect_buffer(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("chunk-arena-indirect"),
        size: capacity as u64 * std::mem::size_of::<DrawIndexedIndirect>() as u64,
        usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Allocate a larger geometry buffer and copy the existing `used_bytes` into it, so
/// every live sub-allocation keeps its offset.
fn grow_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    old: &wgpu::Buffer,
    used_bytes: u64,
    new_size: u64,
    kind: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let new = new_geometry_buffer(device, label, new_size, kind);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("arena-grow"),
    });
    encoder.copy_buffer_to_buffer(old, 0, &new, 0, used_bytes);
    queue.submit(std::iter::once(encoder.finish()));
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_list_reuses_freed_space() {
        let mut fl = FreeList::new(100);
        let a = fl.alloc(30).unwrap();
        let b = fl.alloc(30).unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 30);
        fl.free_block(a, 30);
        // The freed block at the front is the first fit for a small request.
        assert_eq!(fl.alloc(10).unwrap(), 0);
    }

    #[test]
    fn free_list_coalesces_adjacent_blocks() {
        let mut fl = FreeList::new(100);
        let a = fl.alloc(40).unwrap(); // [0, 40)
        let b = fl.alloc(40).unwrap(); // [40, 80)
                                       // Free out of order; the two freed blocks + the tail must merge into one
                                       // block big enough for the whole capacity again.
        fl.free_block(b, 40);
        fl.free_block(a, 40);
        assert_eq!(fl.alloc(100).unwrap(), 0);
    }

    #[test]
    fn free_list_grows_and_offers_new_tail() {
        let mut fl = FreeList::new(50);
        assert!(fl.alloc(50).is_some());
        assert!(fl.alloc(20).is_none());
        fl.grow_to(100);
        assert_eq!(fl.alloc(50).unwrap(), 50);
    }
}
