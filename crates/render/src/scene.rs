//! The one scene-render path.
//!
//! Everything that draws the world — the window, `--bench`, `--screenshot` — goes
//! through [`SceneRenderer::encode_scene`]. There is exactly one implementation, and
//! [`scripts/check-single-render-path.sh`](../../../scripts/check-single-render-path.sh)
//! fails the build if a second one appears (`ARCHITECTURE.md` Rule 5).
//!
//! This exists because the three paths used to be separate copies. The bitmap text
//! overlay landed in the window's copy only, so `--screenshot` silently stopped
//! rendering what the game renders — which quietly destroyed its value as
//! verification and made a whole class of change unprovable. Callers now supply a
//! camera, a target and geometry; what a frame *is* lives here.

use glam::Mat4;

use crate::arena::ChunkArena;
use crate::materials;
use crate::render::{
    build_outline_pipeline, build_pipeline, camera_bind_group_layout, create_depth_view,
    origins_bind_group_layout, outline_bind_group_layout, CameraUniform, OutlineUniform,
    OUTLINE_CUBE_EDGES,
};
use crate::text::font;
use crate::text::TextRenderer;

/// The sky colour a frame clears to.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.45,
    g: 0.62,
    b: 0.80,
    a: 1.0,
};

/// What one frame draws, beyond the camera (already uploaded via
/// [`SceneRenderer::set_camera`]) and the destination (`encode_scene`'s own
/// `color`/`encoder` parameters -- mechanism, not content). Bundled rather
/// than passed as five separate arguments, the same reasoning [`crate::Shot`]
/// documents for headless rendering: what a frame *is* should be small and
/// explicit at the call site.
pub struct SceneFrame<'a> {
    /// All resident chunk geometry, drawn with one indirect submit.
    pub arena: &'a ChunkArena,
    /// From [`ChunkArena::prepare`], which the caller runs first so it can
    /// also report how many chunks survived the cull.
    pub draw_count: u32,
    /// A block to draw the selection outline around (issue #52), or `None`.
    pub selected_block: Option<[i32; 3]>,
    /// Screen-space debug text, or `None`.
    pub overlay: Option<&'a str>,
    /// The hotbar to draw along the bottom, or `None` to draw none.
    pub hotbar: Option<HotbarView<'a>>,
}

/// One hotbar slot's contents, already reduced to what drawing needs.
#[derive(Clone, Copy, Debug)]
pub struct HotbarSlot {
    /// The swatch colour standing in for an item icon. Item art does not exist
    /// yet; the caller derives a stable colour from the item's name, the same
    /// way a block with no texture file gets a placeholder
    /// (`materials::placeholder_color`).
    pub color: [f32; 3],
    pub count: u8,
}

/// What the renderer needs to draw a hotbar: colours and counts, and which slot
/// is held.
///
/// Deliberately **not** an inventory. This crate does not know what an item is,
/// what a stack is, or that slots 0..9 of a 36-slot array are special
/// (`ARCHITECTURE.md` Rule 3 -- the renderer's inputs are meshes, origins, a
/// camera, and now a strip of coloured boxes). The app reduces its `Inventory`
/// to this, which is also what keeps the hotbar drawable in a headless golden
/// test with no sim involved.
#[derive(Clone, Copy, Debug)]
pub struct HotbarView<'a> {
    pub slots: &'a [Option<HotbarSlot>],
    pub selected: u8,
}

/// Owns everything a frame needs that is not the geometry or the camera pose:
/// the pipeline, the camera uniform, the depth buffer, and the debug-text overlay.
///
/// Construct one per target format/size; call [`set_camera`](Self::set_camera) then
/// [`encode_scene`](Self::encode_scene) per frame.
pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// `@group(2)` in `mesh.wgsl`: the block texture array + sampler.
    texture_bind_group: wgpu::BindGroup,
    /// The selected-block wireframe (issue #52): a dedicated line-list
    /// pipeline sharing `camera_bind_group` (`@group(0)`), plus its own tiny
    /// uniform for the highlighted voxel's world position and a static
    /// vertex buffer of unit-cube edges uploaded once here.
    outline_pipeline: wgpu::RenderPipeline,
    outline_vertex_buffer: wgpu::Buffer,
    outline_uniform_buffer: wgpu::Buffer,
    outline_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    text: TextRenderer,
    width: u32,
    height: u32,
}

impl SceneRenderer {
    /// `texture_view`/`texture_sampler` are the block texture array built by
    /// [`crate::render::load_mesh_assets`] — `SceneRenderer` only binds it,
    /// it doesn't know about the registry that produced it.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        texture_view: &wgpu::TextureView,
        texture_sampler: &wgpu::Sampler,
    ) -> Self {
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = camera_bind_group_layout(device);
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let origins_bgl = origins_bind_group_layout(device);
        let textures_bgl = materials::bind_group_layout(device);
        let texture_bind_group =
            materials::bind_group(device, &textures_bgl, texture_view, texture_sampler);

        let outline_bgl = outline_bind_group_layout(device);
        let outline_pipeline = build_outline_pipeline(device, format, &camera_bgl, &outline_bgl);
        let outline_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("outline-vertices"),
            size: std::mem::size_of_val(&OUTLINE_CUBE_EDGES) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &outline_vertex_buffer,
            0,
            bytemuck::cast_slice(&OUTLINE_CUBE_EDGES),
        );
        let outline_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("outline-uniform"),
            size: std::mem::size_of::<OutlineUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let outline_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("outline-bind-group"),
            layout: &outline_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: outline_uniform_buffer.as_entire_binding(),
            }],
        });

        Self {
            pipeline: build_pipeline(device, format, &camera_bgl, &origins_bgl, &textures_bgl),
            camera_buffer,
            camera_bind_group,
            texture_bind_group,
            outline_pipeline,
            outline_vertex_buffer,
            outline_uniform_buffer,
            outline_bind_group,
            depth_view: create_depth_view(device, width, height),
            text: TextRenderer::new(device, queue, format),
            width,
            height,
        }
    }

    /// Rebuild the depth buffer for a new target size.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.width = width;
            self.height = height;
            self.depth_view = create_depth_view(device, width, height);
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// Upload the view-projection matrix this frame draws with.
    pub fn set_camera(&self, queue: &wgpu::Queue, view_proj: Mat4) {
        let uniform = CameraUniform::from_matrix(view_proj);
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Encode one frame: the world, then an optional screen-space text overlay.
    ///
    /// **This is the only place a scene render pass is begun.** A caller that wants
    /// something drawn in the world adds it here, where every caller gets it — that
    /// is the whole point of the rule.
    pub fn encode_scene(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        frame: SceneFrame<'_>,
    ) {
        let SceneFrame {
            arena,
            draw_count,
            selected_block,
            overlay,
            hotbar,
        } = frame;
        if let Some(block) = selected_block {
            let origin = [block[0] as f32, block[1] as f32, block[2] as f32];
            queue.write_buffer(
                &self.outline_uniform_buffer,
                0,
                bytemuck::bytes_of(&OutlineUniform::new(origin)),
            );
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(crate::render::DEPTH_CLEAR as f32),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, arena.origins_bind_group(), &[]);
            pass.set_bind_group(2, &self.texture_bind_group, &[]);
            arena.encode(&mut pass, draw_count);

            // The selected-block outline, same pass so it's depth-tested
            // against the geometry just drawn (issue #52).
            if selected_block.is_some() {
                pass.set_pipeline(&self.outline_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, &self.outline_bind_group, &[]);
                pass.set_vertex_buffer(0, self.outline_vertex_buffer.slice(..));
                pass.draw(0..OUTLINE_CUBE_EDGES.len() as u32, 0..1);
            }
        }

        // Overlay: a second pass over the same colour target (loaded, no depth).
        // Text and hotbar share it -- both are screen-space quads out of the
        // same vertex buffer, so drawing the HUD costs no extra pass.
        if overlay.is_none() && hotbar.is_none() {
            return;
        }
        if let Some(text) = overlay {
            const SCALE: f32 = 2.0;
            // Shadow first (dark, offset), then the white text on top.
            self.text.queue(text, 10.0, 10.0, SCALE, [0.0, 0.0, 0.0]);
            self.text.queue(text, 8.0, 8.0, SCALE, [1.0, 1.0, 1.0]);
        }
        if let Some(hotbar) = hotbar {
            self.queue_hotbar(hotbar);
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        self.text
            .flush(queue, &mut pass, self.width as f32, self.height as f32);
    }

    /// Lay the hotbar out along the bottom centre and queue its quads.
    ///
    /// Pixel sizes rather than fractions of the window: the hotbar should be
    /// the same physical size on a 1080p and a 4K screen, not four times
    /// bigger, which is what scaling with the viewport would give.
    fn queue_hotbar(&mut self, view: HotbarView<'_>) {
        const SLOT: f32 = 48.0;
        const GAP: f32 = 4.0;
        const MARGIN: f32 = 16.0;
        const BORDER: f32 = 3.0;
        const FRAME: [f32; 3] = [0.10, 0.10, 0.12];
        const EMPTY: [f32; 3] = [0.24, 0.24, 0.27];
        const HELD: [f32; 3] = [1.0, 1.0, 1.0];

        let n = view.slots.len() as f32;
        if n == 0.0 {
            return;
        }
        let total = n * SLOT + (n - 1.0) * GAP;
        let x0 = (self.width as f32 - total) * 0.5;
        let y = self.height as f32 - SLOT - MARGIN;

        for (i, slot) in view.slots.iter().enumerate() {
            let x = x0 + i as f32 * (SLOT + GAP);
            let held = i as u8 == view.selected;

            // Border, then the well, then the item -- painted back to front,
            // since the overlay pass has no depth to sort with.
            self.text.queue_rect(
                x - BORDER,
                y - BORDER,
                SLOT + BORDER * 2.0,
                SLOT + BORDER * 2.0,
                if held { HELD } else { FRAME },
            );
            self.text.queue_rect(x, y, SLOT, SLOT, EMPTY);

            let Some(item) = slot else { continue };
            const PAD: f32 = 8.0;
            self.text.queue_rect(
                x + PAD,
                y + PAD,
                SLOT - PAD * 2.0,
                SLOT - PAD * 2.0,
                item.color,
            );

            // Counts of 1 are noise -- a slot with one thing in it is already
            // visibly a slot with something in it.
            if item.count > 1 {
                const SCALE: f32 = 1.5;
                let label = item.count.to_string();
                let w = label.len() as f32 * font::GLYPH as f32 * SCALE;
                let tx = x + SLOT - w - 2.0;
                let ty = y + SLOT - font::GLYPH as f32 * SCALE - 2.0;
                self.text
                    .queue(&label, tx + 1.0, ty + 1.0, SCALE, [0.0, 0.0, 0.0]);
                self.text.queue(&label, tx, ty, SCALE, [1.0, 1.0, 1.0]);
            }
        }
    }
}
