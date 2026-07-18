//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline. All resident chunk
//! geometry lives in a shared [`ChunkArena`], drawn with a single indirect submit;
//! the arena streams as the camera flies (chunks in range are meshed + uploaded,
//! ones that fall out are freed). The shared building blocks (pipeline, depth view,
//! camera) are public so the headless bench/screenshot paths build the same scene.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use winit::window::Window;

use cubara_voxel::{ChunkCoord, Vertex};
use cubara_world::{streaming, World};

use crate::arena::ChunkArena;
use crate::culling::Frustum;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// How many chunks out (square radius) to keep resident around the camera.
const STREAM_RADIUS: i32 = 8;
/// Vertical chunk band to stream — the terrain sits comfortably inside it.
const STREAM_Y_MIN: i32 = 0;
const STREAM_Y_MAX: i32 = 2;
/// Camera fly speed through the world, in blocks per second.
const FLY_SPEED: f32 = 24.0;

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

/// The unit direction the camera flies along (a gentle horizontal diagonal so it
/// keeps crossing chunk boundaries in both x and z).
fn fly_dir() -> glam::Vec3 {
    glam::vec3(1.0, 0.0, 0.35).normalize()
}

/// The camera's look direction: the fly heading pitched down a little.
fn look_dir() -> glam::Vec3 {
    (fly_dir() + glam::vec3(0.0, -0.35, 0.0)).normalize()
}

/// All GPU + window state. Created once the event loop has `resumed`.
pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    frustum: Frustum,

    /// All resident chunk geometry in shared buffers, drawn with one indirect submit.
    arena: ChunkArena,
    /// Every coord we've streamed in, including ones that produced no geometry —
    /// so empty chunks aren't re-generated every frame.
    resident: HashSet<ChunkCoord>,
    /// Chunk the camera is currently in; streaming re-runs when this changes.
    center: ChunkCoord,

    cam_pos: glam::Vec3,
    last_frame: Instant,
    visible_chunks: usize,
    frames: u32,
    last_report: Instant,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Self {
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

        // Camera uniform + bind group.
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = camera_bind_group_layout(&device);
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline = build_pipeline(&device, format, &camera_bgl);
        let depth_view = create_depth_view(&device, config.width, config.height);

        // Start above the terrain near the origin, looking along the fly heading.
        let cam_pos = glam::vec3(0.0, 48.0, 0.0);
        let aspect = config.width as f32 / config.height as f32;
        let frustum =
            Frustum::from_view_proj(CameraUniform::look_view_proj(aspect, cam_pos, look_dir()));
        let center = ChunkCoord::from_world_pos(cam_pos.to_array());

        let arena = ChunkArena::new(&device, multi_draw);

        let mut renderer = Self {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            camera_buffer,
            camera_bind_group,
            depth_view,
            frustum,
            arena,
            resident: HashSet::new(),
            center,
            cam_pos,
            last_frame: Instant::now(),
            visible_chunks: 0,
            frames: 0,
            last_report: Instant::now(),
        };
        // Prime the initial region so the first frame has something to draw.
        renderer.stream_around(center);
        renderer
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.depth_view =
                create_depth_view(&self.device, self.config.width, self.config.height);
        }
    }

    /// Bring the resident set in line with `center`: drop chunks that fell outside
    /// the radius, then generate + upload newly desired ones.
    fn stream_around(&mut self, center: ChunkCoord) {
        puffin::profile_function!();
        let updates = streaming::plan_updates(
            &self.resident,
            center,
            STREAM_RADIUS,
            STREAM_Y_MIN..=STREAM_Y_MAX,
        );

        for coord in updates.to_unload {
            self.arena.remove(coord);
            self.resident.remove(&coord);
        }
        for coord in updates.to_load {
            self.resident.insert(coord);
            if let Some(chunk) = World::chunk_at(coord) {
                self.arena.upload_chunk(&self.queue, coord, &chunk);
            }
        }
        self.center = center;
    }

    pub fn render(&mut self) {
        crate::profiling::Profiler::new_frame();
        puffin::profile_function!();
        self.update();

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
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.45,
                            g: 0.62,
                            b: 0.80,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            self.arena.encode(&mut pass, draw_count);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        self.report_fps();
    }

    /// Advance the flying camera, stream if we crossed a chunk boundary, and upload
    /// the new camera matrix + frustum.
    fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.cam_pos += fly_dir() * FLY_SPEED * dt;

        let center = ChunkCoord::from_world_pos(self.cam_pos.to_array());
        if center != self.center {
            self.stream_around(center);
        }

        let aspect = self.config.width as f32 / self.config.height as f32;
        let vp = CameraUniform::look_view_proj(aspect, self.cam_pos, look_dir());
        self.frustum = Frustum::from_view_proj(vp);
        let uniform = CameraUniform::from_matrix(vp);
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
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
            buffers: &[Vertex::layout()],
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
