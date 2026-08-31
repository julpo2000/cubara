//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline. All resident
//! node geometry lives in a shared [`ChunkArena`], drawn with a single
//! indirect submit. The renderer does not decide what to stream in --
//! `ARCHITECTURE.md` §1: its inputs are meshes, origins and a camera, nothing
//! that knows what a chunk or a `World` is. The caller (`cubara-app`) works
//! out which nodes are wanted (via `cubara_world`), meshes them, and hands
//! the results to [`Renderer::apply_node_updates`]. The shared building
//! blocks (pipeline, depth view, camera) are public so the headless
//! bench/screenshot paths build the same scene.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use winit::window::{CursorGrabMode, Window};

use cubara_voxel::{BlockRegistry, ChunkCoord, Vertex};

use crate::arena::{ChunkArena, MeshedNode, NodeId};
use crate::culling::Frustum;
use crate::materials::{self, MeshAssets};
use crate::scene::{SceneFrame, SceneRenderer};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Camera near/far planes. The far plane covers radius 64's diagonal
/// (64 chunks x 16 blocks = 1,024, so ~1,448 corner to corner) with room over.
const NEAR_PLANE: f32 = 0.1;
const FAR_PLANE: f32 = 2000.0;

/// Depth cleared at the *far* plane, since [`reverse_z`] puts it at 0.
pub const DEPTH_CLEAR: f64 = 0.0;

/// Flip a projection's depth so it runs 1 at the near plane down to 0 at the
/// far plane -- "reversed-Z".
///
/// Paired with a float depth buffer (`DEPTH_FORMAT` is `Depth32Float`) this is
/// close to the best depth precision available, and it is nearly free. A
/// float's precision is concentrated near zero. The conventional mapping
/// spends that precision on the *near* plane, where it is least needed --
/// everything there is close and large -- and leaves almost none for the
/// distance. Reversing it puts the far plane at zero instead.
///
/// That matters here specifically because the near/far ratio is
/// 0.1 : 2,000, i.e. 20,000:1, and block 1.10 made the far end of that range
/// somewhere the player actually looks.
///
/// Built by flipping a standard projection's depth (`z' = w - z`) rather than
/// by swapping the near and far arguments to `perspective_rh`, which produces
/// the same matrix far less obviously.
pub fn reverse_z(proj: glam::Mat4) -> glam::Mat4 {
    let flip = glam::Mat4::from_cols(
        glam::vec4(1.0, 0.0, 0.0, 0.0),
        glam::vec4(0.0, 1.0, 0.0, 0.0),
        glam::vec4(0.0, 0.0, -1.0, 0.0),
        glam::vec4(0.0, 0.0, 1.0, 1.0),
    );
    flip * proj
}

/// Load the real `assets/blocks/*.ron` registry, validated against
/// `assets/textures/` -- the GPU-free half of [`load_mesh_assets`], for a
/// caller that needs to mesh nodes (`cubara_world::mesh`, resolving
/// `tex_layer` via `materials::TextureLayers::from_registry`) before, or
/// entirely without, ever building the actual texture array -- e.g.
/// `cubara-app`'s `--screenshot` mode, or a headless test meshing ahead of a
/// separate call that builds the array later. `CARGO_MANIFEST_DIR` is
/// `crates/render`, so `../..` reaches the repo root regardless of the
/// caller's working directory.
pub fn load_registry() -> BlockRegistry {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry =
        BlockRegistry::load(&repo_root.join("assets/blocks")).expect("assets/blocks must load");
    registry
        .validate_textures(&repo_root.join("assets/textures"))
        .expect("assets/textures must cover every material's faces");
    registry
}

/// [`load_registry`] plus the GPU texture array built from it -- the same
/// materials every entry point (window, `--bench`, `--screenshot`, golden
/// tests) meshes and renders against.
///
/// Returns the CPU-side [`MeshAssets`] (what meshing needs -- ready to `Arc`
/// and share with worker threads) plus the texture array's view and sampler
/// (what [`SceneRenderer`] needs to bind it); these travel separately because
/// they're consumed in different places, not because they're built
/// separately -- `materials::build` does both in one pass.
pub fn load_mesh_assets(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (MeshAssets, wgpu::TextureView, wgpu::Sampler) {
    let registry = load_registry();
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let textures_dir = repo_root.join("assets/textures");
    let (view, sampler, layers) = materials::build(device, queue, &registry, &textures_dir);
    (MeshAssets { registry, layers }, view, sampler)
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Uint32, 1 => Uint32, 2 => Uint32];

/// The GPU vertex layout for [`Vertex`], which is plain data in `cubara-voxel` and
/// knows nothing about the GPU (`ARCHITECTURE.md` Rule 3/4). The layout lives here,
/// with the code that owns pipelines. Three `u32` words, unpacked in the shader --
/// see `docs/PHASE1_ARCHITECTURE.md` §5.2 for the bit layout.
///
/// It must stay in step with the field order of [`Vertex`]; `vertex_layout_matches_vertex`
/// below pins the stride so adding a field there fails here instead of silently
/// mis-reading the buffer on the GPU.
pub const fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

/// Cap on node geometry uploads per frame. A streaming update can hand over a
/// whole ring's worth of newly-meshed nodes at once; spreading the GPU
/// uploads over a few frames avoids the resulting frame-time spike (nodes pop
/// in a hair later, imperceptibly). *What* to stream is the caller's
/// decision (`ARCHITECTURE.md` §1); this is purely about not spiking the
/// frame while applying it, which is why it stays here rather than moving
/// out with the streaming policy.
const MAX_UPLOADS_PER_FRAME: usize = 32;

/// Uniform block shared with `mesh.wgsl`: one column-major view*projection matrix.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

impl CameraUniform {
    /// Orbit `center` at `radius`, framerate-independent via virtual time `t`.
    pub fn new(aspect: f32, t: f32, center: [f32; 3], radius: f32) -> Self {
        Self::from_matrix(Self::view_proj_matrix(aspect, t, center, radius))
    }

    /// The raw orbit view*projection matrix, exposed so callers can also build a
    /// [`Frustum`] from the exact same camera used for the uniform.
    pub fn view_proj_matrix(aspect: f32, t: f32, center: [f32; 3], radius: f32) -> glam::Mat4 {
        let center = glam::Vec3::from(center);
        let angle = t * 0.15;
        let eye = center + glam::vec3(radius * angle.cos(), radius * 0.45, radius * angle.sin());
        Self::look_view_proj(aspect, eye, center - eye)
    }

    /// View*projection for a camera at `eye` looking along `look_dir`, with
    /// reversed-Z depth (see [`reverse_z`]).
    pub fn look_view_proj(aspect: f32, eye: glam::Vec3, look_dir: glam::Vec3) -> glam::Mat4 {
        let proj = reverse_z(glam::Mat4::perspective_rh(
            60f32.to_radians(),
            aspect,
            NEAR_PLANE,
            FAR_PLANE,
        ));
        let view = glam::Mat4::look_at_rh(eye, eye + look_dir, glam::Vec3::Y);
        proj * view
    }

    pub fn from_matrix(m: glam::Mat4) -> Self {
        Self {
            view_proj: m.to_cols_array_2d(),
        }
    }
}

/// A camera position and facing to render from -- the renderer's *entire*
/// idea of "the camera": no input, no movement, no keys held
/// (`ARCHITECTURE.md` Rule 3 -- if the renderer could move the player, the
/// boundary would be wrong). `cubara-app` computes one of these each frame
/// by interpolating the sim's previous and current tick
/// (`docs/PHASE1_ARCHITECTURE.md` §9) and hands it in; headless callers
/// (bench, screenshot, golden tests) build one directly.
#[derive(Clone, Copy, Debug)]
pub struct CameraPose {
    pub eye: glam::Vec3,
    pub look_dir: glam::Vec3,
}

impl CameraPose {
    pub fn view_proj(&self, aspect: f32) -> glam::Mat4 {
        CameraUniform::look_view_proj(aspect, self.eye, self.look_dir)
    }
}

/// Uniform block shared with `outline.wgsl`: the highlighted voxel's
/// world-space min corner. `origin` is `[f32; 4]`, not `[f32; 3]` -- WGSL's
/// uniform address space requires 16-byte alignment for a `vec3<f32>`
/// member, so a plain `vec3` here would need hand-rolled padding to match;
/// a `vec4` with `.w` unused sidesteps that entirely.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OutlineUniform {
    origin: [f32; 4],
}

impl OutlineUniform {
    pub fn new(origin: [f32; 3]) -> Self {
        Self {
            origin: [origin[0], origin[1], origin[2], 0.0],
        }
    }
}

const OUTLINE_VERTEX_ATTRS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];

pub const fn outline_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: (3 * std::mem::size_of::<f32>()) as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &OUTLINE_VERTEX_ATTRS,
    }
}

/// The 12 edges of a unit cube (`0.0..1.0` on each axis, matching one
/// voxel's extent), as 24 line-list vertex positions local to the targeted
/// block. Uploaded once ([`SceneRenderer::new`](crate::scene::SceneRenderer::new));
/// only [`OutlineUniform::origin`] varies per frame.
pub const OUTLINE_CUBE_EDGES: [[f32; 3]; 24] = [
    // Bottom face (y = 0).
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, 0.0],
    // Top face (y = 1).
    [0.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 1.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
    [0.0, 1.0, 0.0],
    // The four vertical edges connecting them.
    [0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 0.0, 1.0],
    [0.0, 1.0, 1.0],
];

/// The wgpu features the GPU-driven path wants, intersected with what `adapter`
/// actually offers — pass the result as `required_features` when requesting the
/// device. Also returns whether `MULTI_DRAW_INDIRECT` made the cut, which selects
/// the arena's fast indirect draw path over the `draw_indexed` fallback (see the
/// #26 spike: both target backends support it, but not all do).
///
/// Deliberately does **not** request `INDIRECT_FIRST_INSTANCE`: block 1.4a
/// tried building per-node origin lookup on `first_instance` +
/// `@builtin(instance_index)` and found it unreliable in *both* directions
/// across real CI backends -- broken with `multi_draw_indexed_indirect` on one
/// software DX12 adapter, broken in the plain `draw_indexed` fallback on
/// another virtualized Metal adapter, with no combination that was safe
/// everywhere. `node_index` is a plain vertex attribute instead (§5.3), so
/// this feature is unused now; see the design doc for the full story.
pub fn gpu_driven_features(adapter: &wgpu::Adapter) -> (wgpu::Features, bool) {
    let features = adapter.features() & wgpu::Features::MULTI_DRAW_INDIRECT;
    let multi_draw = features.contains(wgpu::Features::MULTI_DRAW_INDIRECT);
    (features, multi_draw)
}

/// All GPU + window state. Created once the event loop has `resumed`.
///
/// Owns no `World`, no streaming policy, no mesh-generation pool -- what to
/// stream in is decided entirely by the caller (`cubara-app`, via
/// `cubara_world`), which hands finished node geometry to
/// [`apply_node_updates`](Self::apply_node_updates). This is what makes the
/// renderer rebuildable on its own: its whole vocabulary is meshes, origins,
/// and a camera (`ARCHITECTURE.md` §1).
pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    /// The one scene-render path, shared with `--bench` and `--screenshot`.
    scene: SceneRenderer,
    frustum: Frustum,

    /// All resident node geometry in shared buffers, drawn with one indirect submit.
    arena: ChunkArena,
    /// Which node ids are currently meant to be resident (uploaded, or queued
    /// to become so) -- lets [`drain_uploads`](Self::drain_uploads) skip a
    /// queued upload for a node a newer [`apply_node_updates`](Self::apply_node_updates)
    /// call already unloaded, without this crate needing to know what a node
    /// id actually means.
    desired: HashSet<NodeId>,
    /// Finished meshes waiting to be uploaded, drained at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame to avoid upload spikes.
    upload_queue: VecDeque<MeshedNode>,

    last_frame: Instant,
    visible_chunks: usize,
    frames: u32,
    last_report: Instant,

    /// Whether the F3 debug overlay is shown.
    show_debug: bool,
    /// Smoothed frame time in ms, for a stable on-screen FPS reading.
    frame_ms: f32,
}

impl Renderer {
    /// Bring up the window's GPU surface/device/pipelines and return the
    /// renderer alongside the [`MeshAssets`] its texture array was built
    /// from -- the caller needs those to mesh nodes against the same
    /// registry/texture-layer mapping (`cubara_world::mesh`'s `registry`/
    /// `layer_of` parameters), and building the texture array twice would
    /// waste a real upload, so this is the one place it happens.
    ///
    /// Starts with nothing resident -- no `World`, no priming region. The
    /// caller streams the initial view in via
    /// [`apply_node_updates`](Self::apply_node_updates) exactly like every
    /// later frame.
    pub fn new(window: Arc<Window>, camera: CameraPose) -> (Self, MeshAssets) {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no suitable GPU adapter");

        log::info!("GPU: {:?}", adapter.get_info());

        let (features, multi_draw) = gpu_driven_features(&adapter);
        log::info!("multi_draw_indirect: {multi_draw}");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cubara-device"),
                required_features: features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Uncapped so we can actually measure FPS against the 1000-FPS goal.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let (mesh_assets, tex_view, tex_sampler) = load_mesh_assets(&device, &queue);
        let scene = SceneRenderer::new(
            &device,
            &queue,
            format,
            config.width,
            config.height,
            &tex_view,
            &tex_sampler,
        );

        let aspect = config.width as f32 / config.height as f32;
        let frustum = Frustum::from_view_proj(camera.view_proj(aspect));

        let arena = ChunkArena::new(&device, multi_draw);

        let renderer = Self {
            window,
            surface,
            device,
            queue,
            config,
            scene,
            frustum,
            arena,
            desired: HashSet::new(),
            upload_queue: VecDeque::new(),
            last_frame: Instant::now(),
            visible_chunks: 0,
            frames: 0,
            last_report: Instant::now(),
            show_debug: true,
            frame_ms: 0.0,
        };
        (renderer, mesh_assets)
    }

    /// The device and queue backing this renderer -- what a caller needs to
    /// mesh nodes against the same GPU context (e.g. resolving texture
    /// layers via the [`MeshAssets`] returned alongside this renderer by
    /// [`new`](Self::new)), without this crate needing to know why.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Apply a batch of streaming updates the caller has already decided on:
    /// drop `to_unload`'s geometry immediately, and queue `meshed`'s for
    /// upload (paced at [`MAX_UPLOADS_PER_FRAME`] per frame by
    /// [`drain_uploads`](Self::drain_uploads), called every [`render`](Self::render)).
    /// This is the entire streaming surface the renderer exposes -- *which*
    /// nodes to load/unload is the caller's decision (`ARCHITECTURE.md` §1);
    /// an edit is no different from ordinary streaming from here, just a
    /// single-node update.
    pub fn apply_node_updates(
        &mut self,
        to_unload: impl IntoIterator<Item = NodeId>,
        meshed: impl IntoIterator<Item = MeshedNode>,
    ) {
        for id in to_unload {
            self.desired.remove(&id);
            self.arena.remove(id);
        }
        for node in meshed {
            self.desired.insert(node.id);
            self.upload_queue.push_back(node);
        }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// The surface's current size in pixels -- what screen-space layout needs.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.scene
                .resize(&self.device, self.config.width, self.config.height);
        }
    }

    /// Upload queued nodes from [`apply_node_updates`](Self::apply_node_updates) —
    /// at most [`MAX_UPLOADS_PER_FRAME`] per frame, so a caller handing over a
    /// whole ring's worth of newly-streamed nodes at once doesn't spike the
    /// frame time. Called every [`render`](Self::render); a node's old
    /// geometry (if any) stays drawn until its queued replacement's turn
    /// comes up.
    fn drain_uploads(&mut self) {
        puffin::profile_function!();
        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Some(node) = self.upload_queue.pop_front() else {
                break;
            };
            // Skip if unloaded while it waited in the queue.
            if !self.desired.contains(&node.id) {
                continue;
            }
            self.arena.remove(node.id); // free any prior slot first
            self.arena.insert(
                &self.queue,
                node.id,
                node.origin,
                node.scale,
                &node.mesh,
                node.aabb,
            );
            uploaded += 1;
        }
    }

    /// `hotbar` and `health` are plain data the caller reduces its state to --
    /// this crate never learns what an item is, or what hurt the player
    /// (Rule 3).
    pub fn render(
        &mut self,
        camera: CameraPose,
        selected_block: Option<[i32; 3]>,
        hotbar: Option<crate::scene::HotbarView<'_>>,
        panel: Option<crate::scene::PanelView<'_>>,
        health: Option<crate::scene::HealthView>,
    ) {
        crate::profiling::Profiler::new_frame();
        puffin::profile_function!();
        self.update(camera);

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            // Surface lost/outdated (e.g. during resize) — reconfigure and skip.
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // CPU frustum-cull + upload the indirect draw list before the pass begins.
        let draw_count = self.arena.prepare(&self.queue, &self.frustum);
        self.visible_chunks = draw_count as usize;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            puffin::profile_scope!("encode-pass");
            let overlay = self.show_debug.then(|| self.debug_text(camera));
            self.scene.encode_scene(
                &self.queue,
                &mut encoder,
                &view,
                SceneFrame {
                    arena: &self.arena,
                    draw_count,
                    selected_block,
                    overlay: overlay.as_deref(),
                    hotbar,
                    panel,
                    health,
                },
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        self.report_fps();
    }

    /// Toggle the F3 debug overlay.
    pub fn toggle_debug(&mut self) {
        self.show_debug = !self.show_debug;
    }

    /// Build this frame's debug text. The overlay's drawing (including its drop
    /// shadow) belongs to the shared scene path, so this only produces the string.
    fn debug_text(&self, camera: CameraPose) -> String {
        let p = camera.eye;
        let d = camera.look_dir;
        let facing = if d.x.abs() > d.z.abs() {
            if d.x > 0.0 {
                "east (+x)"
            } else {
                "west (-x)"
            }
        } else if d.z > 0.0 {
            "south (+z)"
        } else {
            "north (-z)"
        };
        let fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };
        let c = ChunkCoord::from_world_pos(p.to_array());
        format!(
            "Cubara  (F3)\n\
             {fps:.0} fps  ({ms:.2} ms)\n\
             xyz  {x:.1} / {y:.1} / {z:.1}\n\
             chunk  {cx} {cy} {cz}\n\
             facing  {facing}\n\
             nodes  {vis} drawn / {res} resident",
            ms = self.frame_ms,
            x = p.x,
            y = p.y,
            z = p.z,
            cx = c.x,
            cy = c.y,
            cz = c.z,
            vis = self.visible_chunks,
            res = self.arena.len(),
        )
    }

    /// Upload whatever's been queued since last frame and refresh the camera
    /// matrix + frustum. The camera's own motion doesn't happen here -- it's
    /// `cubara-sim`'s job (block 1.6); this just tracks frame time for the
    /// on-screen FPS reading and applies streaming the caller already decided
    /// on via [`apply_node_updates`](Self::apply_node_updates).
    fn update(&mut self, camera: CameraPose) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        // Exponentially-smoothed frame time for a steady on-screen FPS reading.
        let ms = dt * 1000.0;
        self.frame_ms = if self.frame_ms == 0.0 {
            ms
        } else {
            self.frame_ms * 0.9 + ms * 0.1
        };
        self.drain_uploads();

        let vp = camera.view_proj(self.scene.aspect());
        self.frustum = Frustum::from_view_proj(vp);
        self.scene.set_camera(&self.queue, vp);
    }

    /// Report frames-per-second roughly once per second.
    fn report_fps(&mut self) {
        self.frames += 1;
        let elapsed = self.last_report.elapsed();
        if elapsed.as_secs_f32() >= 1.0 {
            let fps = self.frames as f32 / elapsed.as_secs_f32();
            log::info!(
                "{fps:.0} FPS | drawn {}/{} resident nodes",
                self.visible_chunks,
                self.arena.len()
            );
            self.frames = 0;
            self.last_report = Instant::now();
        }
    }
}

/// Grab + hide the cursor for first-person look, or release it. Best-effort:
/// `Locked` isn't supported on every platform, so fall back to `Confined`, and
/// never panic if the platform refuses.
pub fn grab_cursor(window: &Window, grab: bool) {
    if grab {
        if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

pub fn camera_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("camera-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// One world-space origin per resident chunk ("node"), read in the vertex
/// shader (`@group(1)` in `mesh.wgsl`) by the `node_index` baked into each
/// [`Vertex`] -- what turns a node-local packed vertex into a world position
/// without a CPU-side translate. [`ChunkArena`] owns the actual buffer/bind
/// group (it's tied to chunk residency); this layout is shared between that
/// and the pipeline so the two stay structurally compatible.
pub fn origins_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("node-origins-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// `@group(1)` in `outline.wgsl`: the highlighted voxel's world-space
/// origin. A separate, tiny bind group rather than folding into the camera
/// one (`@group(0)`, reused unchanged) -- it varies with the selection, the
/// camera uniform doesn't, and the outline pipeline has no use for the mesh
/// pipeline's origins/texture groups at all.
pub fn outline_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("outline-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

pub fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    origins_bgl: &wgpu::BindGroupLayout,
    textures_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh-layout"),
        bind_group_layouts: &[camera_bgl, origins_bgl, textures_bgl],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mesh-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[vertex_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Faces are wound CCW/outward, so cull the back faces.
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            // Reversed-Z: nearer fragments have *greater* depth.
            depth_compare: wgpu::CompareFunction::Greater,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// The selected-block outline's pipeline: a line list, sharing the mesh
/// pipeline's depth buffer and format so it's correctly occluded by terrain
/// in front of it, but with a small negative depth bias so it wins the
/// exact tie against the *targeted* block's own coplanar face instead of
/// z-fighting it (issue #52's Design decisions). Doesn't write depth --
/// nothing needs to be occluded by a wireframe -- and doesn't cull (lines
/// have no winding).
pub fn build_outline_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    outline_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("outline-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/outline.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("outline-layout"),
        bind_group_layouts: &[camera_bgl, outline_bgl],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("outline-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[outline_vertex_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            // Reversed-Z counterpart of LessEqual -- the outline must draw
            // at exactly the depth of the face it outlines, not be rejected by it.
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: -4,
                slope_scale: -2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_layout_matches_vertex() {
        // `Vertex` is plain data in cubara-voxel; its GPU layout lives here. Nothing
        // in the type system ties the two together, so pin it: a field added to
        // Vertex changes the stride and fails here, rather than silently making the
        // GPU read every vertex at the wrong offset.
        let layout = vertex_layout();
        assert_eq!(
            layout.array_stride,
            std::mem::size_of::<Vertex>() as wgpu::BufferAddress
        );
        assert_eq!(layout.array_stride, 12, "three packed u32 words");
        assert_eq!(layout.attributes.len(), 3, "packed0, packed1, packed2");

        // Offsets must land on the real field boundaries.
        let offsets: Vec<u64> = layout.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 4, 8]);
    }
}
