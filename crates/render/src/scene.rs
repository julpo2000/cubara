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
use crate::panel::{InventoryPanel, PanelSlotKind};
use crate::render::{
    build_figure_pipeline, build_mesh_pipeline_from_module, build_outline_pipeline, build_pipeline,
    camera_bind_group_layout, create_depth_view, mesh_pipeline_constants,
    origins_bind_group_layout, outline_bind_group_layout, FrameUniform, Lighting, OutlineUniform,
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
    /// A block being dug and how far along, `0.0..1.0`, to draw cracks on
    /// (see [`crate::crack`]), or `None`.
    pub cracking: Option<([i32; 3], f32)>,
    /// Screen-space debug text, or `None`.
    pub overlay: Option<&'a str>,
    /// The hotbar to draw along the bottom, or `None` to draw none.
    pub hotbar: Option<HotbarView<'a>>,
    /// The open inventory screen, or `None` when it is closed.
    pub panel: Option<PanelView<'a>>,
    /// The other players in sight, to draw as figures (block 2.12b).
    ///
    /// Empty in singleplayer and in every headless shot that does not ask for
    /// one. The local player is **not** in here: you are the camera.
    pub players: &'a [crate::figure::PlayerView],
    /// Health in **points**, or `None` to draw no hearts.
    ///
    /// Points, not hearts, and not a fraction: this crate is told the number
    /// and works out how many full and half hearts that is. It does not know
    /// what full health is, what hurt the player, or that 20 is the maximum --
    /// the app passes both numbers (`ARCHITECTURE.md` Rule 3, the same
    /// boundary [`HotbarView`] draws).
    pub health: Option<HealthView>,
    /// Whether to draw the crosshair at the centre of the screen.
    ///
    /// The caller decides, because only it knows whether the player is looking
    /// through the camera or at a screen: a crosshair over an inventory is a
    /// mark on nothing.
    pub crosshair: bool,
    /// Where to write GPU timestamps immediately before and after the main
    /// scene pass, or `None` to write none. `cubara-app`'s bench uses this
    /// for GPU/frame timing; the window and headless paths pass `None` so
    /// this changes nothing about what either draws.
    pub gpu_timestamps: Option<GpuTimestamps<'a>>,
}

/// Where [`SceneRenderer::encode_scene`] writes the two timestamps bracketing
/// the main scene pass (terrain + figures + outline -- not the overlay text
/// pass, which is comparatively free and not what a caller measuring draw
/// cost wants included).
///
/// The caller owns the query set, any resolve/readback buffers, and the
/// bookkeeping that makes reading them back safe ([`crate::TimestampRing`]);
/// this only says *where* to write. Written via the pass descriptor's own
/// `timestamp_writes` field (not `CommandEncoder::write_timestamp` outside
/// the pass, which this used at first: on Metal, wgpu-hal's encoder-level
/// timestamps both sample at the same "stage boundary" and come back
/// identical, always reading 0ms). That needs only the base
/// `wgpu::Features::TIMESTAMP_QUERY` -- not `TIMESTAMP_QUERY_INSIDE_PASSES`,
/// which gates the separate, imperative `RenderPass::write_timestamp` call
/// (for writing more than one timestamp inside a single pass), a different
/// wgpu-core code path this crate doesn't use.
pub struct GpuTimestamps<'a> {
    pub query_set: &'a wgpu::QuerySet,
    /// Index written just before the main pass begins.
    pub begin: u32,
    /// Index written just after the main pass ends.
    pub end: u32,
}

/// Everything drawn over the world in screen space, as the window hands it to
/// [`crate::Renderer::render`]. Grouped because each is one more optional
/// piece of HUD, and a new one should be a field here rather than another
/// positional argument at every call site.
#[derive(Clone, Copy, Debug)]
pub struct Hud<'a> {
    pub hotbar: Option<HotbarView<'a>>,
    pub panel: Option<PanelView<'a>>,
    pub health: Option<HealthView>,
    /// See [`SceneFrame::crosshair`].
    pub crosshair: bool,
    /// The pause menu or command console's text, drawn the same top-left
    /// overlay the F3 debug text uses (`Renderer::render` picks one -- both
    /// are "read this instead of looking at the world", so there is nothing
    /// useful about showing both at once). `None` the rest of the time,
    /// which is every existing call site (`Hud { .. }`'s `Default`-style
    /// construction covers them, no golden image changes).
    pub menu: Option<&'a str>,
}

/// What the renderer needs to draw hearts: two numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthView {
    pub points: u8,
    pub max_points: u8,
}

/// What the renderer needs to draw the inventory screen.
///
/// Like [`HotbarView`], colours and counts rather than items -- `contents` is
/// parallel to `panel.slots()`, so the app fills it by walking the same layout
/// the renderer draws from. One layout, two consumers (see [`crate::panel`]).
#[derive(Clone, Copy, Debug)]
pub struct PanelView<'a> {
    pub panel: &'a InventoryPanel,
    /// One entry per slot in `panel.slots()`, same order.
    pub contents: &'a [Option<HotbarSlot>],
    /// What the cursor is carrying, and where the cursor is in pixels.
    pub held: Option<HotbarSlot>,
    pub cursor: (f32, f32),
    /// A label to show beside the cursor -- the name of the item under it --
    /// or `None`. Already a string: this crate does not know what an item is.
    pub tooltip: Option<&'a str>,
    /// A furnace's two meters, or `None` on any other screen.
    pub gauges: Option<FurnaceGauges>,
}

/// How far a furnace has got, as two fractions in `0.0..=1.0`.
///
/// Fractions rather than tick counts, so this crate never learns how long a
/// log burns or a recipe takes (Rule 3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FurnaceGauges {
    /// Burn left in the fuel alight: 1 just lit, 0 out.
    pub burn: f32,
    /// Progress on the item being smelted: 0 just started, 1 done.
    pub progress: f32,
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
    /// Which icon to draw instead of the swatch -- an index into the icons
    /// last given to [`SceneRenderer::set_icons`] -- or `None` for the swatch.
    /// An index the renderer has no icon for also falls back to the swatch, so
    /// an item is never drawn as nothing.
    pub icon: Option<u32>,
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
    /// The `Lighting` `pipeline` was actually built with -- compared against
    /// on every [`set_lighting`](Self::set_lighting) call so an unchanged
    /// value (the common case: most frames pass the same `Lighting` as last
    /// frame) never spawns a rebuild.
    pipeline_lighting: Lighting,
    /// `mesh.wgsl`'s shader module and pipeline layout, kept around so a
    /// rebuild only re-specializes the pipeline rather than re-parsing WGSL
    /// or rebuilding the layout -- both fixed by lighting-independent things
    /// (the WGSL source; the bind group layouts).
    mesh_shader: wgpu::ShaderModule,
    mesh_layout: wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    /// A lighting rebuild in flight on a background thread, if
    /// [`set_lighting`](Self::set_lighting) has kicked one off since the
    /// last time it landed. `encode_scene` polls this every frame so a
    /// finished rebuild swaps in without the caller doing anything; `resize`
    /// and [`wait_for_lighting_rebuild`](Self::wait_for_lighting_rebuild)
    /// both also drain it directly, for the callers that can't tolerate a
    /// frame or two of stale lighting (a resized pipeline must match the new
    /// viewport immediately; a one-shot screenshot has only one frame to be
    /// right in).
    lighting_rebuild: Option<std::sync::mpsc::Receiver<(Lighting, wgpu::RenderPipeline)>>,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// `@group(2)` in `mesh.wgsl`: the block texture array + sampler.
    texture_bind_group: wgpu::BindGroup,
    /// The selected-block wireframe (issue #52): a dedicated line-list
    /// pipeline sharing `camera_bind_group` (`@group(0)`), plus its own tiny
    /// uniform for the highlighted voxel's world position and a static
    /// vertex buffer of unit-cube edges uploaded once here.
    figure_pipeline: wgpu::RenderPipeline,
    /// Rebuilt every frame from the players in sight, and grown when it has to
    /// be. A figure is 216 vertices, so this stays small enough that reusing
    /// one buffer beats managing per-player ones.
    figure_vertex_buffer: wgpu::Buffer,
    figure_capacity: usize,
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
            size: std::mem::size_of::<FrameUniform>() as u64,
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

        let figure_pipeline = build_figure_pipeline(device, format, &camera_bgl);
        let figure_capacity = crate::figure::VERTICES_PER_FIGURE * 8;
        let figure_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("figure-vertices"),
            size: (figure_capacity * 6 * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

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

        let pipeline_lighting = Lighting::default();
        let constants = mesh_pipeline_constants(&pipeline_lighting, width, height);
        let (mesh_shader, mesh_layout, pipeline) = build_pipeline(
            device,
            format,
            &camera_bgl,
            &origins_bgl,
            &textures_bgl,
            &constants,
        );

        Self {
            pipeline,
            pipeline_lighting,
            mesh_shader,
            mesh_layout,
            format,
            lighting_rebuild: None,
            camera_buffer,
            camera_bind_group,
            texture_bind_group,
            figure_pipeline,
            figure_vertex_buffer,
            figure_capacity,
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

    /// Replace the item icons [`HotbarSlot::icon`] indexes into: one 16x16
    /// RGBA tile per entry, `None` where an item has no art.
    pub fn set_icons(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        icons: &[Option<Vec<u8>>],
    ) {
        self.text.set_icons(device, queue, icons);
    }

    /// Rebuild the depth buffer, and the mesh pipeline, for a new target
    /// size. The pipeline rebuild is synchronous (unlike
    /// [`set_lighting`](Self::set_lighting)'s background one): resizing
    /// already recreates the depth buffer on the spot, is a rare, deliberate
    /// action rather than a per-frame occurrence, and radial fog's ray
    /// direction depends on the viewport size (`viewport_width`/`height`/
    /// `aspect` in `mesh.wgsl`) -- drawing even one frame at the old size's
    /// constants against the new depth buffer would be a visible mismatch,
    /// not just stale lighting. Drops any in-flight *lighting* rebuild: it
    /// targeted the old viewport size and would stomp this one when it
    /// landed.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.width = width;
            self.height = height;
            self.depth_view = create_depth_view(device, width, height);
            let constants = mesh_pipeline_constants(&self.pipeline_lighting, width, height);
            self.pipeline = build_mesh_pipeline_from_module(
                device,
                self.format,
                &self.mesh_shader,
                &self.mesh_layout,
                &constants,
            );
            self.lighting_rebuild = None;
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// Rebuild `mesh.wgsl`'s pipeline with `lighting`'s values, on a
    /// background thread, if they actually differ from what the current
    /// pipeline already has (the common per-frame case: [`set_camera`]
    /// calls this every frame with whatever `Lighting` the caller has, and
    /// most frames it hasn't changed since the last one -- the equality
    /// check makes that free rather than a rebuild).
    ///
    /// Lighting lives in `mesh.wgsl` as pipeline `override` constants, not a
    /// per-frame uniform read (`render.rs`'s `mesh_pipeline_constants`
    /// doc comment has the measurement this is built on), so changing it
    /// costs a pipeline rebuild rather than a buffer write. A rebuild is
    /// ~0.2-0.4 ms once Metal has compiled that exact override permutation
    /// before, but the *first* time a given permutation is ever used it is
    /// closer to 40 ms -- too slow to do on the frame that wants it,
    /// especially at the >1000 FPS this engine targets. Doing it on a
    /// background thread and swapping in once
    /// [`poll_lighting_rebuild`](Self::poll_lighting_rebuild) (called every
    /// frame from [`encode_scene`](Self::encode_scene)) finds it ready means
    /// the current frame, and every frame until the new one lands, keeps
    /// rendering with the old lighting rather than stalling for it --
    /// exactly the "spread over frames, smooth on-the-go updates" the
    /// project owner asked for instead of pre-building every value up front.
    ///
    /// A rebuild already in flight for a value that's since changed again is
    /// simply superseded: replacing `lighting_rebuild` drops the old
    /// receiver, so that thread's eventual `send` finds no one listening and
    /// is silently discarded.
    pub fn set_lighting(&mut self, device: &wgpu::Device, lighting: Lighting) {
        if lighting == self.pipeline_lighting {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.lighting_rebuild = Some(rx);
        let device = device.clone();
        let format = self.format;
        let shader = self.mesh_shader.clone();
        let layout = self.mesh_layout.clone();
        let (width, height) = (self.width, self.height);
        std::thread::spawn(move || {
            let constants = mesh_pipeline_constants(&lighting, width, height);
            let pipeline =
                build_mesh_pipeline_from_module(&device, format, &shader, &layout, &constants);
            let _ = tx.send((lighting, pipeline));
        });
    }

    /// Pick up a background [`set_lighting`](Self::set_lighting) rebuild if
    /// one has finished since the last call -- a non-blocking channel check,
    /// cheap enough to call every frame regardless of whether a rebuild is
    /// actually in flight. Called from [`encode_scene`](Self::encode_scene)
    /// so no caller needs to remember to do this themselves.
    fn poll_lighting_rebuild(&mut self) {
        let Some(rx) = &self.lighting_rebuild else {
            return;
        };
        match rx.try_recv() {
            Ok((lighting, pipeline)) => {
                self.pipeline = pipeline;
                self.pipeline_lighting = lighting;
                self.lighting_rebuild = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.lighting_rebuild = None;
            }
        }
    }

    /// Block until an in-flight [`set_lighting`](Self::set_lighting) rebuild
    /// lands, for a caller that draws exactly one measured/captured frame
    /// and cannot tolerate the window's usual "stale lighting for a frame or
    /// two while the rebuild finishes in the background" -- `--bench` and
    /// `--screenshot` both call this right after `set_camera` and before
    /// their one draw. A no-op when nothing is in flight (the pipeline
    /// already matches, the overwhelmingly common case once a scene's first
    /// frame has built its real pipeline).
    pub fn wait_for_lighting_rebuild(&mut self) {
        let Some(rx) = self.lighting_rebuild.take() else {
            return;
        };
        if let Ok((lighting, pipeline)) = rx.recv() {
            self.pipeline = pipeline;
            self.pipeline_lighting = lighting;
        }
    }

    /// Upload the view-projection matrix, the eye position, and the figure/
    /// outline lighting this frame draws with (`figure.wgsl`'s own uniform
    /// read -- unaffected by `mesh.wgsl`'s move to overrides), and kick off
    /// a mesh pipeline rebuild via [`set_lighting`](Self::set_lighting) if
    /// `lighting` has actually changed. `eye` is a separate parameter rather
    /// than derived from `view_proj` (which is possible but a needless round
    /// trip through a matrix inverse) since every caller already knows where
    /// its camera is.
    pub fn set_camera(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        eye: glam::Vec3,
        lighting: Lighting,
    ) {
        let uniform = FrameUniform::new(view_proj, eye, lighting);
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
        self.set_lighting(device, lighting);
    }

    /// Encode one frame: the world, then an optional screen-space text overlay.
    ///
    /// **This is the only place a scene render pass is begun.** A caller that wants
    /// something drawn in the world adds it here, where every caller gets it — that
    /// is the whole point of the rule.
    pub fn encode_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        frame: SceneFrame<'_>,
    ) {
        // Non-blocking: picks up a set_lighting rebuild if one finished
        // since last frame, otherwise costs one channel check and returns.
        self.poll_lighting_rebuild();

        let SceneFrame {
            arena,
            draw_count,
            selected_block,
            cracking,
            players,
            overlay,
            hotbar,
            panel,
            health,
            crosshair,
            gpu_timestamps,
        } = frame;

        // Build this frame's figures and upload them. Done before the pass so
        // the write is ordered against the draw that reads it.
        let mut figure_vertices: Vec<crate::figure::FigureVertex> =
            Vec::with_capacity(players.len() * crate::figure::VERTICES_PER_FIGURE);
        for &view in players {
            crate::figure::figure_vertices(view, &mut figure_vertices);
        }
        // Cracks share the figures' buffer and draw: both are coloured,
        // depth-tested triangles built on the CPU.
        if let Some((block, progress)) = cracking {
            crate::crack::crack_vertices(block, progress, &mut figure_vertices);
        }
        if figure_vertices.len() > self.figure_capacity {
            // More people came into view than the buffer holds. Grown rather
            // than clamped: silently dropping the last player to arrive is a
            // bug that only shows up in a crowd, which is the worst place to
            // find one.
            self.figure_capacity = figure_vertices.len().next_power_of_two();
            self.figure_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("figure-vertices"),
                size: (self.figure_capacity * 6 * std::mem::size_of::<f32>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let figure_vertices_count = figure_vertices.len();
        if figure_vertices_count > 0 {
            queue.write_buffer(
                &self.figure_vertex_buffer,
                0,
                bytemuck::cast_slice(&figure_vertices),
            );
        }

        if let Some(block) = selected_block {
            let origin = [block[0] as f32, block[1] as f32, block[2] as f32];
            queue.write_buffer(
                &self.outline_uniform_buffer,
                0,
                bytemuck::bytes_of(&OutlineUniform::new(origin)),
            );
        }
        {
            // Pass-scoped, not the encoder-level `write_timestamp` pair this
            // used before: on Metal, wgpu-hal's encoder-level timestamps both
            // sample "at stage boundaries" through a shared blit encoder and
            // come back identical, reading as a permanent 0ms GPU pass. The
            // pass-scoped form is what wgpu-hal actually maps to each
            // backend's real per-pass timing primitive (Metal's
            // `sample_buffer_attachments`, Vulkan/DX12's pass timestamps),
            // and it is also a more honest description of what's being
            // measured -- the pass, not whatever the encoder happened to be
            // doing around it. Needs only the base `TIMESTAMP_QUERY` --
            // setting this descriptor field is a different wgpu-core path
            // than the encoder-level form's `_INSIDE_ENCODERS` tier, or the
            // imperative in-pass `RenderPass::write_timestamp`'s
            // `_INSIDE_PASSES` tier (neither of which this uses).
            let timestamp_writes =
                gpu_timestamps
                    .as_ref()
                    .map(|ts| wgpu::RenderPassTimestampWrites {
                        query_set: ts.query_set,
                        beginning_of_pass_write_index: Some(ts.begin),
                        end_of_pass_write_index: Some(ts.end),
                    });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
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
                timestamp_writes,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, arena.origins_bind_group(), &[]);
            pass.set_bind_group(2, &self.texture_bind_group, &[]);
            arena.encode(&mut pass, draw_count);

            // Other players, same pass so a figure behind a hill is behind it.
            if figure_vertices_count > 0 {
                pass.set_pipeline(&self.figure_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, self.figure_vertex_buffer.slice(..));
                pass.draw(0..figure_vertices_count as u32, 0..1);
            }

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
        if overlay.is_none()
            && hotbar.is_none()
            && panel.is_none()
            && health.is_none()
            && !crosshair
        {
            return;
        }
        if crosshair {
            self.queue_crosshair();
        }
        if let Some(text) = overlay {
            const SCALE: f32 = 2.0;
            // Shadow first (dark, offset), then the white text on top.
            self.text.queue(text, 10.0, 10.0, SCALE, [0.0, 0.0, 0.0]);
            self.text.queue(text, 8.0, 8.0, SCALE, [1.0, 1.0, 1.0]);
        }
        if let Some(health) = health {
            self.queue_hearts(health);
        }
        if let Some(hotbar) = hotbar {
            self.queue_hotbar(hotbar);
        }
        // After the hotbar: the screen covers it, and the overlay pass has no
        // depth to sort with, so order is the only thing deciding what is on top.
        if let Some(panel) = panel {
            self.queue_panel(panel);
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.text
            .flush(queue, &mut pass, self.width as f32, self.height as f32);
    }

    /// Draw the inventory screen from the same layout `hit` tests against.
    fn queue_panel(&mut self, view: PanelView<'_>) {
        const DIM: [f32; 3] = [0.02, 0.02, 0.03];
        const BACK: [f32; 3] = [0.13, 0.13, 0.16];
        const WELL: [f32; 3] = [0.24, 0.24, 0.27];
        const RESULT: [f32; 3] = [0.32, 0.29, 0.20];
        const PAD: f32 = 6.0;

        // A dimmed sheet over the world, so the screen reads as modal even
        // though the sim keeps running behind it. Alpha, not an opaque
        // rectangle: hiding the world entirely reads as a scene change rather
        // than a menu, and you lose the sense of where you were standing.
        self.text
            .queue_rect_alpha(0.0, 0.0, self.width as f32, self.height as f32, DIM, 0.72);
        let p = view.panel;
        self.text.queue_rect(p.x, p.y, p.width, p.height, BACK);

        for (slot, content) in p.slots().iter().zip(view.contents) {
            let well = if slot.kind == PanelSlotKind::Result {
                RESULT
            } else {
                WELL
            };
            self.text
                .queue_rect(slot.x, slot.y, slot.size, slot.size, well);
            if let Some(item) = content {
                self.queue_item(slot.x, slot.y, slot.size, PAD, *item);
            }
        }

        // The cursor's stack last, so it is above everything -- it is the thing
        // being moved, and it has to be visible over whatever it is moved onto.
        if let Some(held) = view.held {
            let (cx, cy) = view.cursor;
            let s = crate::panel::SLOT;
            self.queue_item(cx - s * 0.5, cy - s * 0.5, s, PAD, held);
        }

        if let Some(gauges) = view.gauges {
            self.queue_gauges(p, gauges);
        }

        if let Some(label) = view.tooltip {
            self.queue_tooltip(label, view.cursor);
        }
    }

    /// A flame meter beside the fuel slot and an arrow from the input to the
    /// output that fills as the item smelts.
    ///
    /// Without them a furnace that is working looks exactly like one that is
    /// not until an ingot appears -- which in play read as "the furnace does not
    /// work". Placed from the layout's own slots, so they move with it.
    fn queue_gauges(&mut self, p: &InventoryPanel, g: FurnaceGauges) {
        const TRACK: [f32; 3] = [0.08, 0.08, 0.10];
        const FLAME: [f32; 3] = [0.98, 0.55, 0.12];
        const ARROW: [f32; 3] = [0.92, 0.92, 0.92];
        const BAR: f32 = 6.0;
        let find = |kind| p.slots().iter().find(|s| s.kind == kind).copied();
        let (Some(fuel), Some(output)) = (find(PanelSlotKind::Fuel), find(PanelSlotKind::Result))
        else {
            return;
        };

        // Flame: a vertical bar left of the fuel slot, draining downward.
        let (fx, fy, fh) = (fuel.x - BAR - 4.0, fuel.y, fuel.size);
        self.text.queue_rect(fx, fy, BAR, fh, TRACK);
        let lit = fh * g.burn.clamp(0.0, 1.0);
        self.text.queue_rect(fx, fy + fh - lit, BAR, lit, FLAME);

        // Arrow: a horizontal bar from the input column to the output slot.
        let x0 = fuel.x + fuel.size + 6.0;
        let x1 = output.x - 6.0;
        let ay = output.y + output.size * 0.5 - BAR * 0.5;
        self.text.queue_rect(x0, ay, x1 - x0, BAR, TRACK);
        self.text
            .queue_rect(x0, ay, (x1 - x0) * g.progress.clamp(0.0, 1.0), BAR, ARROW);
    }

    /// An item's name in a dark box just above and right of the cursor, kept
    /// on screen at the edges.
    fn queue_tooltip(&mut self, label: &str, (cx, cy): (f32, f32)) {
        const SCALE: f32 = 2.0;
        const PAD: f32 = 5.0;
        const BACK: [f32; 3] = [0.05, 0.04, 0.08];
        const EDGE: [f32; 3] = [0.30, 0.20, 0.50];
        let glyph = font::GLYPH as f32 * SCALE;
        let w = label.chars().count() as f32 * glyph + PAD * 2.0;
        let h = glyph + PAD * 2.0;
        let x = (cx + 14.0).min(self.width as f32 - w).max(0.0);
        let y = (cy - h - 6.0).max(0.0);
        self.text
            .queue_rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, EDGE);
        self.text.queue_rect_alpha(x, y, w, h, BACK, 0.94);
        self.text
            .queue(label, x + PAD, y + PAD, SCALE, [1.0, 1.0, 1.0]);
    }

    /// A small plus at the centre of the screen: where a click lands.
    ///
    /// White with a dark rim, so it reads against sky and against snow-pale
    /// stone alike; the overlay pass has no blend mode that would invert what
    /// is behind it, and adding one for four rectangles is not worth a second
    /// 2D path.
    fn queue_crosshair(&mut self) {
        const ARM: f32 = 9.0;
        const THICK: f32 = 2.0;
        const RIM: [f32; 3] = [0.05, 0.05, 0.06];
        const MARK: [f32; 3] = [0.95, 0.95, 0.95];
        // Whole pixels, so a 2 px line is 2 px and not a blurred 3.
        let cx = (self.width as f32 * 0.5).floor();
        let cy = (self.height as f32 * 0.5).floor();
        for (color, grow) in [(RIM, 1.0), (MARK, 0.0)] {
            self.text.queue_rect(
                cx - ARM - grow,
                cy - THICK * 0.5 - grow,
                ARM * 2.0 + grow * 2.0,
                THICK + grow * 2.0,
                color,
            );
            self.text.queue_rect(
                cx - THICK * 0.5 - grow,
                cy - ARM - grow,
                THICK + grow * 2.0,
                ARM * 2.0 + grow * 2.0,
                color,
            );
        }
    }

    /// One item swatch plus its count, inside a slot of `size` at (`x`, `y`).
    /// Shared by the hotbar and the screen so the two cannot drift apart.
    fn queue_item(&mut self, x: f32, y: f32, size: f32, pad: f32, item: HotbarSlot) {
        let inner = size - pad * 2.0;
        let drawn = item
            .icon
            .is_some_and(|i| self.text.queue_icon(i, x + pad, y + pad, inner, inner));
        if !drawn {
            self.text
                .queue_rect(x + pad, y + pad, inner, inner, item.color);
        }
        // Counts of 1 are noise -- a slot with one thing in it is already
        // visibly a slot with something in it.
        if item.count > 1 {
            const SCALE: f32 = 1.5;
            let label = item.count.to_string();
            let w = label.len() as f32 * font::GLYPH as f32 * SCALE;
            let tx = x + size - w - 2.0;
            let ty = y + size - font::GLYPH as f32 * SCALE - 2.0;
            self.text
                .queue(&label, tx + 1.0, ty + 1.0, SCALE, [0.0, 0.0, 0.0]);
            self.text.queue(&label, tx, ty, SCALE, [1.0, 1.0, 1.0]);
        }
    }

    /// Hearts, in a row just above the hotbar.
    ///
    /// **Two points per heart**, so odd health draws a half. The half is drawn
    /// as a narrower filled quad over the empty one rather than as its own
    /// shape: the font/quad overlay has no sprites, and half a heart is exactly
    /// half a heart's width of fill.
    ///
    /// Left-aligned with the hotbar rather than centred, because a row that
    /// grows and shrinks from its centre makes it hard to read at a glance how
    /// many are missing.
    fn queue_hearts(&mut self, view: HealthView) {
        const HEART: f32 = 18.0;
        const GAP: f32 = 3.0;
        const MARGIN: f32 = 16.0;
        const HOTBAR: f32 = 48.0;
        const LIFT: f32 = 10.0;
        const EMPTY: [f32; 3] = [0.18, 0.06, 0.08];
        const FULL: [f32; 3] = [0.86, 0.16, 0.22];
        const BORDER: [f32; 3] = [0.06, 0.02, 0.03];
        const PER_HEART: u8 = 2;

        if view.max_points == 0 {
            return;
        }
        let hearts = view.max_points.div_ceil(PER_HEART) as usize;
        let total = hearts as f32 * HEART + (hearts as f32 - 1.0) * GAP;
        // Aligned with the hotbar's left edge, and sitting above it.
        let hotbar_total = 9.0 * 48.0 + 8.0 * 4.0;
        let x0 = (self.width as f32 - hotbar_total) * 0.5;
        let y = self.height as f32 - HOTBAR - MARGIN - HEART - LIFT;
        let _ = total;

        for i in 0..hearts {
            let x = x0 + i as f32 * (HEART + GAP);
            self.text
                .queue_rect(x - 1.0, y - 1.0, HEART + 2.0, HEART + 2.0, BORDER);
            self.text.queue_rect(x, y, HEART, HEART, EMPTY);

            // How much of *this* heart is filled: 2 points full, 1 half, 0 none.
            let filled = view
                .points
                .saturating_sub(i as u8 * PER_HEART)
                .min(PER_HEART);
            if filled > 0 {
                let w = HEART * (filled as f32 / PER_HEART as f32);
                self.text.queue_rect(x, y, w, HEART, FULL);
            }
        }
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
            self.queue_item(x, y, SLOT, PAD, *item);
        }
    }
}
