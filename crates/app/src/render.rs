//! GPU bring-up and per-frame rendering.
//!
//! Owns the wgpu surface/device/queue and the render pipeline(s). Kept separate
//! from the windowing/event-loop code in `main.rs` so the renderer can grow into
//! the real engine without turning `main` into a monolith.

use std::sync::Arc;
use std::time::Instant;

use wgpu::util::DeviceExt;
use winit::window::Window;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// A single mesh vertex: object-space position and normal.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

impl Vertex {
    const fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VERTEX_ATTRS,
        }
    }
}

/// Uniform block shared with `cube.wgsl` (`std140`-compatible: two column-major mat4).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
}

/// All GPU + window state. Created once the event loop has `resumed`.
pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,

    start: Instant,
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

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cubara-device"),
                required_features: wgpu::Features::empty(),
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

        // Geometry.
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube-vertices"),
            contents: bytemuck::cast_slice(CUBE_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube-indices"),
            contents: bytemuck::cast_slice(CUBE_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        // Camera uniform + bind group.
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline = build_pipeline(&device, format, &camera_bgl);
        let depth_view = create_depth_view(&device, &config);

        Self {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count: CUBE_INDICES.len() as u32,
            camera_buffer,
            camera_bind_group,
            depth_view,
            start: Instant::now(),
            frames: 0,
            last_report: Instant::now(),
        }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.depth_view = create_depth_view(&self.device, &self.config);
        }
    }

    pub fn render(&mut self) {
        self.update_camera();

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

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.08,
                            b: 0.13,
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
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        self.report_fps();
    }

    /// Recompute the view/projection and model matrices and upload them.
    fn update_camera(&mut self) {
        let t = self.start.elapsed().as_secs_f32();
        let aspect = self.config.width as f32 / self.config.height as f32;

        let proj = glam::Mat4::perspective_rh(60f32.to_radians(), aspect, 0.1, 100.0);
        let view =
            glam::Mat4::look_at_rh(glam::vec3(0.0, 1.2, 3.0), glam::Vec3::ZERO, glam::Vec3::Y);
        let model = glam::Mat4::from_rotation_y(t * 0.8) * glam::Mat4::from_rotation_x(t * 0.4);

        let uniform = CameraUniform {
            view_proj: (proj * view).to_cols_array_2d(),
            model: model.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Report frames-per-second roughly once per second.
    fn report_fps(&mut self) {
        self.frames += 1;
        let elapsed = self.last_report.elapsed();
        if elapsed.as_secs_f32() >= 1.0 {
            let fps = self.frames as f32 / elapsed.as_secs_f32();
            log::info!("{fps:.0} FPS");
            self.frames = 0;
            self.last_report = Instant::now();
        }
    }
}

fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
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

fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cube-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/cube.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cube-layout"),
        bind_group_layouts: &[camera_bgl],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cube-pipeline"),
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
            // Draw both faces for now; back-face culling comes with meshing (M2).
            cull_mode: None,
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

/// A unit cube centered on the origin: 6 faces × 4 corners, with per-face normals.
#[rustfmt::skip]
const CUBE_VERTICES: &[Vertex] = &[
    // +X
    Vertex { position: [ 0.5, -0.5,  0.5], normal: [ 1.0,  0.0,  0.0] },
    Vertex { position: [ 0.5, -0.5, -0.5], normal: [ 1.0,  0.0,  0.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], normal: [ 1.0,  0.0,  0.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], normal: [ 1.0,  0.0,  0.0] },
    // -X
    Vertex { position: [-0.5, -0.5, -0.5], normal: [-1.0,  0.0,  0.0] },
    Vertex { position: [-0.5, -0.5,  0.5], normal: [-1.0,  0.0,  0.0] },
    Vertex { position: [-0.5,  0.5,  0.5], normal: [-1.0,  0.0,  0.0] },
    Vertex { position: [-0.5,  0.5, -0.5], normal: [-1.0,  0.0,  0.0] },
    // +Y
    Vertex { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0] },
    Vertex { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0] },
    // -Y
    Vertex { position: [-0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0] },
    Vertex { position: [ 0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0] },
    Vertex { position: [ 0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0] },
    Vertex { position: [-0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0] },
    // +Z
    Vertex { position: [-0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0] },
    Vertex { position: [ 0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0] },
    Vertex { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0] },
    // -Z
    Vertex { position: [ 0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0] },
    Vertex { position: [-0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0] },
    Vertex { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0] },
];

#[rustfmt::skip]
const CUBE_INDICES: &[u16] = &[
     0,  1,  2,  0,  2,  3, // +X
     4,  5,  6,  4,  6,  7, // -X
     8,  9, 10,  8, 10, 11, // +Y
    12, 13, 14, 12, 14, 15, // -Y
    16, 17, 18, 16, 18, 19, // +Z
    20, 21, 22, 20, 22, 23, // -Z
];
