//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline. All resident chunk
//! geometry lives in a shared [`ChunkArena`], drawn with a single indirect submit;
//! the arena streams as the camera flies (chunks in range are meshed + uploaded,
//! ones that fall out are freed). The shared building blocks (pipeline, depth view,
//! camera) are public so the headless bench/screenshot paths build the same scene.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use winit::window::{CursorGrabMode, Window};

use cubara_voxel::{BlockRegistry, ChunkCoord, Vertex};
use cubara_world::node::{self, NodeKey};
use cubara_world::World;

use crate::arena::ChunkArena;
use crate::culling::Frustum;
use crate::materials::{self, MeshAssets};
use crate::mesher::{sort_batch, BuiltNode, MeshPool};
use crate::scene::{SceneFrame, SceneRenderer};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Load the real `assets/blocks/*.ron` registry and build the texture array
/// from `assets/textures/` -- the same materials every entry point (window,
/// `--bench`, `--screenshot`, golden tests) meshes and renders against.
/// `CARGO_MANIFEST_DIR` is `crates/render`, so `../..` reaches the repo root
/// regardless of the caller's working directory.
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
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registry =
        BlockRegistry::load(&repo_root.join("assets/blocks")).expect("assets/blocks must load");
    let textures_dir = repo_root.join("assets/textures");
    registry
        .validate_textures(&textures_dir)
        .expect("assets/textures must cover every material's faces");
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

/// Vertical chunk band to stream — the terrain sits comfortably inside it. How
/// far out (and at what LOD) is [`node::DEFAULT_RING_SCHEDULE`]'s job now,
/// not a single radius here.
const STREAM_Y_MIN: i32 = 0;
const STREAM_Y_MAX: i32 = 2;
/// Cap on chunk geometry uploads per frame. Crossing a chunk boundary re-LODs a
/// whole ring at once; spreading the GPU uploads over a few frames avoids the
/// resulting frame-time spike (chunks pop in a hair later, imperceptibly).
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

    /// View*projection for a camera at `eye` looking along `look_dir`.
    pub fn look_view_proj(aspect: f32, eye: glam::Vec3, look_dir: glam::Vec3) -> glam::Mat4 {
        let proj = glam::Mat4::perspective_rh(60f32.to_radians(), aspect, 0.1, 2000.0);
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
pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    /// The one scene-render path, shared with `--bench` and `--screenshot`.
    scene: SceneRenderer,
    frustum: Frustum,

    /// All resident chunk geometry in shared buffers, drawn with one indirect submit.
    arena: ChunkArena,
    /// Which blocks exist and what texture layer represents each -- loaded
    /// once at startup, shared with every mesh job (`ARCHITECTURE.md` Rule 4:
    /// the registry half stays GPU-free; only the layer numbers came from us).
    mesh_assets: Arc<MeshAssets>,
    /// Background worker pool that generates + meshes nodes off the main thread.
    mesh_pool: MeshPool,
    /// Finished meshes waiting to be uploaded, drained at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame to avoid upload spikes.
    upload_queue: VecDeque<BuiltNode>,
    /// Nodes that are meshed and uploaded (or known empty) -- a `NodeKey` already
    /// names both position and detail level, so residency is just set membership,
    /// unlike the old per-chunk map that tracked level as separate data.
    resident: HashSet<NodeKey>,
    /// Chunk the camera is currently in; streaming re-runs when this changes.
    center: ChunkCoord,

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
    pub fn new(window: Arc<Window>, world: &Arc<World>, camera: CameraPose) -> Self {
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
        let mesh_assets = Arc::new(mesh_assets);
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
        let center = ChunkCoord::from_world_pos(camera.eye.to_array());

        let arena = ChunkArena::new(&device, multi_draw);

        let mut renderer = Self {
            window,
            surface,
            device,
            queue,
            config,
            scene,
            frustum,
            arena,
            mesh_assets,
            mesh_pool: MeshPool::new(),
            upload_queue: VecDeque::new(),
            resident: HashSet::new(),
            center,
            last_frame: Instant::now(),
            visible_chunks: 0,
            frames: 0,
            last_report: Instant::now(),
            show_debug: true,
            frame_ms: 0.0,
        };
        // Prime the initial region so the first frame has something to draw.
        renderer.stream_around(world, center);
        renderer
    }

    /// Force a re-mesh of the chunk `cc` (e.g. after an edit): the worker re-reads
    /// the edit overlay, and [`drain_meshes`](Self::drain_meshes) swaps the
    /// geometry in atomically, so there's no gap.
    ///
    /// Always re-meshes `cc`'s level-0 node, not whatever node is currently
    /// resident there: an edit only ever happens within player reach, which is
    /// well inside the always-full-resolution near field the ring schedule
    /// keeps around the player (`World::node_at`'s doc comment) -- a coarser
    /// node can never be the one actually representing an editable chunk.
    pub fn invalidate(&mut self, world: &Arc<World>, cc: ChunkCoord) {
        let node = NodeKey::containing(cc, 0);
        self.mesh_pool.cancel(node);
        self.mesh_pool.request(world, &self.mesh_assets, node);
    }

    pub fn window(&self) -> &Window {
        &self.window
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

    /// Bring the streamed set in line with `center`: drop nodes that fell outside
    /// the ring schedule, and *request* each desired node that isn't already
    /// resident or in flight, nearest first. Meshing happens on the worker pool;
    /// results are uploaded in [`drain_meshes`](Self::drain_meshes), so this never
    /// meshes on the main thread.
    ///
    /// A boundary crossing typically unloads some nodes and loads others at a
    /// *different* level for the same area (a coarse node splitting into finer
    /// ones, or the reverse) -- unlike the old per-chunk LOD change, this isn't a
    /// same-key swap, so the outgoing node's geometry disappears from the arena
    /// immediately rather than staying drawn until the replacement is ready. A
    /// momentary gap at a ring boundary is an accepted, known limitation at this
    /// stage (see issue #107) -- the same category as the LOD-boundary cracks
    /// skirts (#108) will fix, not a correctness bug.
    fn stream_around(&mut self, world: &Arc<World>, center: ChunkCoord) {
        puffin::profile_function!();
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
        for node in stale {
            if self.resident.remove(&node) {
                self.arena.remove(node);
            }
            self.mesh_pool.cancel(node);
        }

        // Request whatever's desired but not yet resident, nearest first --
        // reuses `plan_node_updates`'s tested nearest-first ordering rather than
        // a second distance sort here.
        let updates =
            node::plan_node_updates(&self.resident, center, y_range, node::DEFAULT_RING_SCHEDULE);
        for node in updates.to_load {
            if self.mesh_pool.is_in_flight(node) {
                continue;
            }
            self.mesh_pool.request(world, &self.mesh_assets, node);
        }
        self.center = center;
    }

    /// Take finished meshes from the worker pool and upload them — but at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame, so a boundary crossing (which streams a
    /// whole ring at once) doesn't spike the frame time. Completed nodes are marked
    /// resident immediately (so they aren't re-requested) and their old geometry
    /// stays drawn until the new upload swaps it in.
    fn drain_meshes(&mut self) {
        puffin::profile_function!();
        // Claim everything finished this frame, in a fixed order (ascending
        // NodeKey) rather than whatever order the workers happened to finish
        // in -- see sort_batch (issue #83). Mark resident now, upload over the
        // next frames.
        for built in sort_batch(self.mesh_pool.poll()) {
            self.resident.insert(built.node);
            self.upload_queue.push_back(built);
        }

        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Some(built) = self.upload_queue.pop_front() else {
                break;
            };
            // Skip if unloaded while it waited in the queue.
            if !self.resident.contains(&built.node) {
                continue;
            }
            self.arena.remove(built.node); // free any prior slot first
            if let Some((mesh, aabb)) = built.geometry {
                self.arena.insert(&self.queue, built.node, &mesh, aabb);
            }
            uploaded += 1;
        }
    }

    pub fn render(
        &mut self,
        world: &Arc<World>,
        camera: CameraPose,
        selected_block: Option<[i32; 3]>,
    ) {
        crate::profiling::Profiler::new_frame();
        puffin::profile_function!();
        self.update(world, camera);

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
            cx = self.center.x,
            cy = self.center.y,
            cz = self.center.z,
            vis = self.visible_chunks,
            res = self.arena.len(),
        )
    }

    /// Stream if the camera crossed a chunk boundary, and upload the new
    /// camera matrix + frustum. The camera's own motion no longer happens
    /// here -- it's `cubara-sim`'s job now (block 1.6); this just tracks
    /// frame time for the on-screen FPS reading.
    fn update(&mut self, world: &Arc<World>, camera: CameraPose) {
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
        let center = ChunkCoord::from_world_pos(camera.eye.to_array());
        if center != self.center {
            self.stream_around(world, center);
        }
        // Take whatever the workers finished meshing since last frame.
        self.drain_meshes();

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
            depth_compare: wgpu::CompareFunction::Less,
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
            depth_compare: wgpu::CompareFunction::LessEqual,
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
