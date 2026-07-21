//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline. All resident chunk
//! geometry lives in a shared [`ChunkArena`], drawn with a single indirect submit;
//! the arena streams as the camera flies (chunks in range are meshed + uploaded,
//! ones that fall out are freed). The shared building blocks (pipeline, depth view,
//! camera) are public so the headless bench/screenshot paths build the same scene.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use winit::window::{CursorGrabMode, Window};

use cubara_voxel::{ChunkCoord, Vertex};
use cubara_world::{streaming, World};

use crate::arena::ChunkArena;
use crate::camera::FlyCamera;
use crate::culling::Frustum;
use crate::mesher::{BuiltChunk, MeshPool};
use crate::scene::SceneRenderer;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];

/// The GPU vertex layout for [`Vertex`], which is plain data in `cubara-voxel` and
/// knows nothing about the GPU (`ARCHITECTURE.md` Rule 3/4). The layout lives here,
/// with the code that owns pipelines.
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

/// How many chunks out (square radius) to keep resident around the camera. The inner
/// core is full resolution and only the far rings drop to a coarser LOD (see
/// [`streaming::lod_for`]), so this reaches well past the detailed core for a distant
/// horizon without the triangle/upload cost a fully full-resolution radius would carry.
const STREAM_RADIUS: i32 = 28;
/// Vertical chunk band to stream — the terrain sits comfortably inside it.
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

/// The wgpu features the GPU-driven path wants, intersected with what `adapter`
/// actually offers — pass the result as `required_features` when requesting the
/// device. Also returns whether `MULTI_DRAW_INDIRECT` made the cut, which selects
/// the arena's fast indirect draw path over the `draw_indexed` fallback (see the
/// #26 spike: both target backends support it, but not all do).
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
    /// Background worker pool that generates + meshes chunks off the main thread.
    mesh_pool: MeshPool,
    /// Finished meshes waiting to be uploaded, drained at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame to avoid upload spikes.
    upload_queue: VecDeque<BuiltChunk>,
    /// Coords that are meshed and uploaded (or known empty), mapped to the LOD level
    /// they're currently at — so we only re-mesh when a chunk's desired LOD changes.
    resident: HashMap<ChunkCoord, u32>,
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
    pub fn new(window: Arc<Window>, world: &Arc<World>, camera: &FlyCamera) -> Self {
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

        let scene = SceneRenderer::new(&device, &queue, format, config.width, config.height);

        let aspect = config.width as f32 / config.height as f32;
        let frustum = Frustum::from_view_proj(camera.view_proj(aspect));
        let center = ChunkCoord::from_world_pos(camera.pos.to_array());

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
            mesh_pool: MeshPool::new(),
            upload_queue: VecDeque::new(),
            resident: HashMap::new(),
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

    /// Force a re-mesh of `cc` (e.g. after an edit): the worker re-reads the edit
    /// overlay, and [`drain_meshes`](Self::drain_meshes) swaps the geometry in
    /// atomically, so there's no gap.
    pub fn invalidate(&mut self, world: &Arc<World>, cc: ChunkCoord) {
        self.mesh_pool.cancel(cc);
        self.mesh_pool
            .request(world, cc, streaming::lod_for(cc, self.center));
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

    /// Bring the streamed set in line with `center`: drop chunks that fell outside
    /// the radius, and *request* each desired chunk at its distance-based LOD — new
    /// ones, and resident ones whose LOD changed as the camera moved. Meshing happens
    /// on the worker pool; results are uploaded in [`drain_meshes`](Self::drain_meshes),
    /// so this never meshes on the main thread.
    fn stream_around(&mut self, world: &Arc<World>, center: ChunkCoord) {
        puffin::profile_function!();
        let mut desired =
            streaming::desired_chunks(center, STREAM_RADIUS, STREAM_Y_MIN..=STREAM_Y_MAX);
        let desired_set: HashSet<ChunkCoord> = desired.iter().copied().collect();

        // Unload anything no longer desired — uploaded or still in flight.
        let stale: Vec<ChunkCoord> = self
            .resident
            .keys()
            .chain(self.mesh_pool.in_flight().keys())
            .filter(|c| !desired_set.contains(c))
            .copied()
            .collect();
        for coord in stale {
            if self.resident.remove(&coord).is_some() {
                self.arena.remove(coord);
            }
            self.mesh_pool.cancel(coord);
        }

        // Request each desired chunk at its LOD unless it's already there. Nearest
        // first so detail around the camera streams in before the fringe.
        desired.sort_by_key(|c| (c.x - center.x).pow(2) + (c.z - center.z).pow(2));
        for coord in desired {
            let level = streaming::lod_for(coord, center);
            if self.resident.get(&coord) == Some(&level)
                || self.mesh_pool.is_in_flight(coord, level)
            {
                continue;
            }
            self.mesh_pool.request(world, coord, level);
        }
        self.center = center;
    }

    /// Take finished meshes from the worker pool and upload them — but at most
    /// [`MAX_UPLOADS_PER_FRAME`] per frame, so a boundary crossing (which re-LODs a
    /// whole ring at once) doesn't spike the frame time. Completed chunks are marked
    /// resident immediately (so they aren't re-requested) and their old geometry
    /// stays drawn until the new upload swaps it in.
    fn drain_meshes(&mut self) {
        puffin::profile_function!();
        // Claim everything finished; mark resident now, upload over the next frames.
        for built in self.mesh_pool.poll() {
            self.resident.insert(built.coord, built.level);
            self.upload_queue.push_back(built);
        }

        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Some(built) = self.upload_queue.pop_front() else {
                break;
            };
            // Skip if superseded/unloaded while it waited (its LOD is no longer wanted).
            if self.resident.get(&built.coord) != Some(&built.level) {
                continue;
            }
            self.arena.remove(built.coord); // free any prior (coarser) LOD first
            if let Some((mesh, aabb)) = built.geometry {
                self.arena.insert(&self.queue, built.coord, &mesh, aabb);
            }
            uploaded += 1;
        }
    }

    pub fn render(&mut self, world: &Arc<World>, camera: &FlyCamera) {
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
                &self.arena,
                draw_count,
                overlay.as_deref(),
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
    fn debug_text(&self, camera: &FlyCamera) -> String {
        let p = camera.pos;
        let d = camera.look_dir();
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
             chunks  {vis} drawn / {res} resident",
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

    /// Advance the flying camera, stream if we crossed a chunk boundary, and upload
    /// the new camera matrix + frustum.
    fn update(&mut self, world: &Arc<World>, camera: &FlyCamera) {
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
        let center = ChunkCoord::from_world_pos(camera.pos.to_array());
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
                "{fps:.0} FPS | drawn {}/{} resident chunks",
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
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh-layout"),
        bind_group_layouts: &[camera_bgl],
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
        assert_eq!(layout.array_stride, 28, "3 + 3 + 1 floats");
        assert_eq!(layout.attributes.len(), 3, "position, normal, ao");

        // Offsets must land on the real field boundaries.
        let offsets: Vec<u64> = layout.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 12, 24]);
    }
}
