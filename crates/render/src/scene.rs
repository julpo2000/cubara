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
    build_figure_pipeline, build_outline_pipeline, build_pipeline, camera_bind_group_layout,
    create_depth_view, origins_bind_group_layout, outline_bind_group_layout, CameraUniform,
    OutlineUniform, OUTLINE_CUBE_EDGES,
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

        Self {
            pipeline: build_pipeline(device, format, &camera_bgl, &origins_bgl, &textures_bgl),
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
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        frame: SceneFrame<'_>,
    ) {
        let SceneFrame {
            arena,
            draw_count,
            selected_block,
            players,
            overlay,
            hotbar,
            panel,
            health,
            crosshair,
        } = frame;

        // Build this frame's figures and upload them. Done before the pass so
        // the write is ordered against the draw that reads it.
        let mut figure_vertices: Vec<crate::figure::FigureVertex> =
            Vec::with_capacity(players.len() * crate::figure::VERTICES_PER_FIGURE);
        for &view in players {
            crate::figure::figure_vertices(view, &mut figure_vertices);
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
        self.text.queue_rect(
            x + pad,
            y + pad,
            size - pad * 2.0,
            size - pad * 2.0,
            item.color,
        );
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
