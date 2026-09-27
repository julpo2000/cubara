//! The far terrain's GPU half (`docs/PROPOSAL_FAR_VIEW.md` §3.2): where the
//! patches' heights live on the GPU, which of them are drawn this frame, and
//! the parameters `far.wgsl` reads.
//!
//! Which patches exist and what heights they hold is decided elsewhere
//! (`cubara_world::far`), and handed over as plain numbers -- this crate does
//! not depend on the world crate (Rule 3), so a patch arrives as a
//! [`FarPatch`] and leaves as a [`FarSlot`].

use crate::aggregate::{MaskingTable, Materials, TABLE_SIZE};
use crate::culling::{Aabb, Frustum};
use crate::render::far_bind_group_layout;
use glam::Vec3;

/// Quads along one side of a patch. Must match `far.wgsl`'s `QUADS` and
/// `cubara_world::far::PATCH_QUADS`; the app asserts the second.
pub const FAR_QUADS: usize = 32;

/// Heights along one side of a patch.
pub const FAR_VERTS: usize = FAR_QUADS + 1;

/// The vertex grid one patch is drawn with: its own vertices plus a ring
/// around them that `far.wgsl` drops into the skirt.
const GRID: u32 = FAR_VERTS as u32 + 2;

/// One patch, as the renderer needs it: where it is, how coarse, and its
/// heights row by row (`z` outer, `x` inner), [`FAR_VERTS`] a side.
#[derive(Clone, Copy, Debug)]
pub struct FarPatch<'a> {
    /// The lowest `(x, z)` corner, in blocks.
    pub origin: [f32; 2],
    /// The width of one quad, in blocks.
    pub quad: f32,
    pub heights: &'a [f32],
    /// How far the skirt drops below the edge, in blocks: deep enough to
    /// cover the gap to a coarser neighbour.
    pub skirt: f32,
}

/// Where a patch was put, to remove it by later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FarSlot(u32);

/// What `far.wgsl` reads besides the heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarParams {
    /// The box the voxel rings draw themselves, `(min, max)` in blocks.
    /// Nothing of the far terrain is drawn inside it. `None`: no hole.
    pub hole: Option<([f32; 3], [f32; 3])>,
    /// What the ground is made of: the mean colours of its faces
    /// ([`crate::materials::mean_color`]) and the soil's depth, for the
    /// block-aggregate shading (`crate::aggregate`).
    pub materials: Materials,
}

/// What the ground is made of, from the block textures: each face's texture
/// averaged ([`crate::materials::mean_color`]), falling back to plain
/// colours where a texture is missing, with the world's `soil_depth`
/// (`cubara_world::SOIL_DEPTH`, which this crate cannot name).
pub fn ground_materials(soil_depth: f32) -> Materials {
    let mean = |name: &str, fallback: Vec3| {
        crate::materials::mean_color(name).map_or(fallback, Vec3::from)
    };
    let p = PLACEHOLDER_MATERIALS;
    Materials {
        top: mean("grass_top", p.top),
        side: mean("grass_side", p.side),
        soil: mean("soil", p.soil),
        stone: mean("stone", p.stone),
        soil_depth,
    }
}

/// Plain grass-and-dirt colours, for before the real ones are set.
const PLACEHOLDER_MATERIALS: Materials = Materials {
    top: Vec3::new(0.2, 0.4, 0.15),
    side: Vec3::new(0.3, 0.3, 0.15),
    soil: Vec3::new(0.3, 0.2, 0.1),
    stone: Vec3::new(0.4, 0.4, 0.4),
    soil_depth: 3.0,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PatchGpu {
    origin: [f32; 2],
    quad: f32,
    skirt: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParamsGpu {
    hole_min: [f32; 4],
    hole_max: [f32; 4],
    top_color: [f32; 4],
    /// `w`: the soil's depth, in blocks.
    side_color: [f32; 4],
    soil_color: [f32; 4],
    stone_color: [f32; 4],
}

impl ParamsGpu {
    fn new(p: FarParams) -> Self {
        // No hole: an empty box, min above max, which nothing is inside.
        let (min, max) = p.hole.unwrap_or(([f32::MAX; 3], [f32::MIN; 3]));
        let m = p.materials;
        Self {
            hole_min: [min[0], min[1], min[2], 0.0],
            hole_max: [max[0], max[1], max[2], 0.0],
            top_color: m.top.extend(1.0).to_array(),
            side_color: m.side.extend(m.soil_depth).to_array(),
            soil_color: m.soil.extend(1.0).to_array(),
            stone_color: m.stone.extend(1.0).to_array(),
        }
    }
}

/// The committed [`MaskingTable`] as the 3D texture `far.wgsl` samples: its
/// first two axes as width and height, its last two stacked as depth
/// (`axis 2 + 16 * axis 3`). That is exactly the table's own byte order, so
/// the bytes go up as they are. Linear filtering interpolates the first three
/// axes; the shader blends along the fourth itself, never across a stack's
/// edge.
fn masking_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let side = TABLE_SIZE as u32;
    let size = wgpu::Extent3d {
        width: side,
        height: side,
        depth_or_array_layers: side * side,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("far-masking-table"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        MaskingTable::committed().bytes(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(side),
            rows_per_image: Some(side),
        },
        size,
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("far-masking-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    (texture.create_view(&Default::default()), sampler)
}

/// The far terrain on the GPU: a fixed number of patch slots, and the list of
/// them drawn this frame.
pub struct FarTerrain {
    heights: wgpu::Buffer,
    patches: wgpu::Buffer,
    draw_slots: wgpu::Buffer,
    params: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    bind_group: wgpu::BindGroup,
    capacity: u32,
    /// Slots not holding a patch, handed out last-freed first.
    free: Vec<u32>,
    /// The voxels' hole, from the last [`set_params`](Self::set_params).
    hole: Option<Aabb>,
    /// Each occupied slot's box, for culling; `None` where the slot is free.
    boxes: Vec<Option<Aabb>>,
    /// How many slots [`prepare`](Self::prepare) found in view.
    drawn: u32,
}

impl FarTerrain {
    /// Room for `capacity` patches.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, capacity: u32) -> Self {
        let storage = |label, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size.max(16),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let per_patch = (FAR_VERTS * FAR_VERTS * std::mem::size_of::<f32>()) as u64;
        let heights = storage("far-heights", per_patch * capacity as u64);
        let patches = storage(
            "far-patches",
            std::mem::size_of::<PatchGpu>() as u64 * capacity as u64,
        );
        let draw_slots = storage("far-draw-slots", 4 * capacity as u64);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("far-params"),
            size: std::mem::size_of::<ParamsGpu>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &params,
            0,
            bytemuck::bytes_of(&ParamsGpu::new(FarParams {
                hole: None,
                materials: PLACEHOLDER_MATERIALS,
            })),
        );
        let (masking, masking_sampler) = masking_texture(device, queue);

        let grid = grid_indices();
        let indices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("far-grid-indices"),
            // A multiple of 4, as a buffer write must be; the padding is
            // never indexed.
            size: (std::mem::size_of_val(grid.as_slice()) as u64).next_multiple_of(4),
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = bytemuck::cast_slice::<u16, u8>(&grid).to_vec();
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        queue.write_buffer(&indices, 0, &bytes);

        let layout = far_bind_group_layout(device);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("far-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: heights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: patches.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: draw_slots.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&masking),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&masking_sampler),
                },
            ],
        });
        Self {
            heights,
            patches,
            draw_slots,
            params,
            indices,
            index_count: grid.len() as u32,
            bind_group,
            capacity,
            free: (0..capacity).rev().collect(),
            boxes: vec![None; capacity as usize],
            drawn: 0,
            hole: None,
        }
    }

    /// Upload a patch. `None` when every slot is taken -- the caller decides
    /// what gives way, since it is the one that knows which patches matter.
    pub fn insert(&mut self, queue: &wgpu::Queue, patch: FarPatch<'_>) -> Option<FarSlot> {
        assert_eq!(
            patch.heights.len(),
            FAR_VERTS * FAR_VERTS,
            "a patch's heights"
        );
        let slot = self.free.pop()?;
        let per_patch = (FAR_VERTS * FAR_VERTS * std::mem::size_of::<f32>()) as u64;
        queue.write_buffer(
            &self.heights,
            per_patch * slot as u64,
            bytemuck::cast_slice(patch.heights),
        );
        queue.write_buffer(
            &self.patches,
            std::mem::size_of::<PatchGpu>() as u64 * slot as u64,
            bytemuck::bytes_of(&PatchGpu {
                origin: patch.origin,
                quad: patch.quad,
                skirt: patch.skirt,
            }),
        );
        self.boxes[slot as usize] = Some(patch_box(&patch));
        Some(FarSlot(slot))
    }

    /// Free a slot. The patch in it stops being drawn from the next
    /// [`prepare`](Self::prepare).
    pub fn remove(&mut self, slot: FarSlot) {
        if self.boxes[slot.0 as usize].take().is_some() {
            self.free.push(slot.0);
        }
    }

    pub fn set_params(&mut self, queue: &wgpu::Queue, params: FarParams) {
        self.hole = params.hole.map(|(min, max)| Aabb {
            min: Vec3::from(min),
            max: Vec3::from(max),
        });
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&ParamsGpu::new(params)));
    }

    /// Decide what is drawn this frame: every occupied slot whose box the
    /// frustum can see, nearest to `eye` first -- so near ground fills the
    /// depth buffer before the distant ground it hides is drawn. Returns how
    /// many.
    /// Patches wholly inside the voxels' hole are not drawn at all.
    pub fn prepare(&mut self, queue: &wgpu::Queue, frustum: &Frustum, eye: Vec3) -> u32 {
        let visible = visible_slots(&self.boxes, frustum);
        let order = draw_order(&self.boxes, visible, self.hole.as_ref(), eye);
        if !order.is_empty() {
            queue.write_buffer(&self.draw_slots, 0, bytemuck::cast_slice(&order));
        }
        self.drawn = order.len() as u32;
        self.drawn
    }

    /// Patches held.
    pub fn len(&self) -> usize {
        self.boxes.iter().filter(|b| b.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Patches drawn this frame, from the last [`prepare`](Self::prepare).
    pub fn drawn(&self) -> u32 {
        self.drawn
    }

    /// Triangles drawn this frame, skirts included.
    pub fn drawn_triangles(&self) -> u64 {
        self.drawn as u64 * (self.index_count / 3) as u64
    }

    /// Draw this frame's patches. The pipeline and bind group 0 (the camera)
    /// are the caller's to set.
    pub(crate) fn encode(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.drawn == 0 {
            return;
        }
        pass.set_bind_group(1, &self.bind_group, &[]);
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..self.drawn);
    }
}

fn min_max(heights: &[f32]) -> (f32, f32) {
    heights
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
            (lo.min(h), hi.max(h))
        })
}

/// A patch's box: its square of ground, from the bottom of its skirt to its
/// highest vertex.
fn patch_box(patch: &FarPatch<'_>) -> Aabb {
    let size = patch.quad * FAR_QUADS as f32;
    let (lo, hi) = min_max(patch.heights);
    let lo = lo - patch.skirt;
    Aabb {
        min: Vec3::new(patch.origin[0], lo, patch.origin[1]),
        max: Vec3::new(patch.origin[0] + size, hi, patch.origin[1] + size),
    }
}

/// The occupied slots the frustum can see, in slot order.
fn visible_slots(boxes: &[Option<Aabb>], frustum: &Frustum) -> Vec<u32> {
    boxes
        .iter()
        .enumerate()
        .filter_map(|(slot, b)| {
            b.as_ref()
                .filter(|b| frustum.intersects_aabb(b))
                .map(|_| slot as u32)
        })
        .collect()
}

/// The visible slots to draw, nearest to `eye` first -- so near ground fills
/// the depth buffer before the distant ground it hides -- leaving out any
/// wholly inside the voxels' hole.
fn draw_order(
    boxes: &[Option<Aabb>],
    visible: Vec<u32>,
    hole: Option<&Aabb>,
    eye: Vec3,
) -> Vec<u32> {
    let bx = |slot: &u32| {
        boxes[*slot as usize]
            .as_ref()
            .expect("a visible slot is occupied")
    };
    let mut order: Vec<u32> = visible
        .into_iter()
        .filter(|slot| !hole.is_some_and(|h| contains(h, bx(slot))))
        .collect();
    let distance = |slot: &u32| {
        let b = bx(slot);
        (eye.clamp(b.min, b.max) - eye).length_squared()
    };
    order.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
    order
}

fn contains(outer: &Aabb, inner: &Aabb) -> bool {
    outer.min.cmple(inner.min).all() && inner.max.cmple(outer.max).all()
}

/// The index list for one patch's [`GRID`] x [`GRID`] vertices: two triangles
/// per cell, every cell -- including the skirt ring's.
fn grid_indices() -> Vec<u16> {
    let mut out = Vec::with_capacity(((GRID - 1) * (GRID - 1) * 6) as usize);
    for z in 0..GRID - 1 {
        for x in 0..GRID - 1 {
            let a = (z * GRID + x) as u16;
            let b = a + 1;
            let c = a + GRID as u16;
            let d = c + 1;
            out.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_covers_every_cell_with_two_triangles() {
        let idx = grid_indices();
        let cells = (GRID - 1) * (GRID - 1);
        assert_eq!(idx.len() as u32, cells * 6);
        assert!(idx.iter().all(|&i| (i as u32) < GRID * GRID));
        // Every vertex is used: none of the skirt ring is left out.
        let mut used = vec![false; (GRID * GRID) as usize];
        for &i in &idx {
            used[i as usize] = true;
        }
        assert!(used.iter().all(|u| *u));
    }

    /// The shader's `VERTS`/`GRID` and this module's must agree, or every
    /// patch reads its neighbour's heights.
    #[test]
    fn the_shader_agrees_on_the_patch_size() {
        let wgsl = include_str!("shaders/far.wgsl");
        assert!(wgsl.contains(&format!("const QUADS: i32 = {FAR_QUADS};")));
        assert!(wgsl.contains(&format!("const VERTS: u32 = {FAR_VERTS}u;")));
        assert!(wgsl.contains(&format!("const GRID: u32 = {GRID}u;")));
    }

    #[test]
    fn a_patch_box_spans_its_ground_and_its_skirt() {
        let heights: Vec<f32> = (0..FAR_VERTS * FAR_VERTS)
            .map(|i| 10.0 + (i % 7) as f32)
            .collect();
        let b = patch_box(&FarPatch {
            origin: [64.0, -128.0],
            quad: 4.0,
            heights: &heights,
            skirt: 8.0,
        });
        assert_eq!(b.min, Vec3::new(64.0, 2.0, -128.0));
        assert_eq!(b.max, Vec3::new(64.0 + 128.0, 16.0, 0.0));
    }

    /// Patches wholly inside the voxels' hole are not drawn; the rest are,
    /// nearest first.
    #[test]
    fn patches_are_drawn_nearest_first_and_none_inside_the_hole() {
        let b = |x: f32| {
            Some(Aabb {
                min: Vec3::new(x, 0.0, 0.0),
                max: Vec3::new(x + 10.0, 10.0, 10.0),
            })
        };
        let hole = Aabb {
            min: Vec3::new(-100.0, -100.0, -100.0),
            max: Vec3::new(100.0, 100.0, 100.0),
        };
        //           inside  crosses  clear     clear     crosses
        let boxes = [b(0.0), b(95.0), b(300.0), b(150.0), b(-105.0)];
        let order = draw_order(&boxes, vec![0, 1, 2, 3, 4], Some(&hole), Vec3::ZERO);
        assert_eq!(order, vec![1, 4, 3, 2]);
        let order = draw_order(&boxes, vec![2, 0, 1], None, Vec3::ZERO);
        assert_eq!(order, vec![0, 1, 2], "no hole: everything, nearest first");
    }

    #[test]
    fn only_what_the_frustum_sees_is_drawn() {
        let proj = glam::Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 10_000.0);
        let view = glam::Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), Vec3::Y);
        let frustum = Frustum::from_view_proj(proj * view);
        let ahead = Aabb {
            min: Vec3::new(-10.0, -10.0, -110.0),
            max: Vec3::new(10.0, 10.0, -90.0),
        };
        let behind = Aabb {
            min: Vec3::new(-10.0, -10.0, 90.0),
            max: Vec3::new(10.0, 10.0, 110.0),
        };
        let boxes = vec![Some(behind), None, Some(ahead), Some(ahead)];
        assert_eq!(visible_slots(&boxes, &frustum), vec![2, 3]);
    }
}
