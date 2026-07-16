//! Headless FPS benchmark.
//!
//! Renders the multi-chunk world to an offscreen target with no window and no
//! vsync, submitting frames pipelined (not waited on per-frame) so we measure real
//! sustained throughput against the 1000-FPS goal. A fixed virtual time step keeps
//! the camera orbit identical regardless of how fast the machine runs.
//!
//! Run with: `cargo run --release -- --bench`

use std::time::Instant;

use crate::culling::Frustum;
use crate::render::{
    build_pipeline, camera_bind_group_layout, create_depth_view, upload_world, CameraUniform,
};
use crate::world::World;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const WARMUP_FRAMES: u32 = 200;
const MEASURE_FRAMES: u32 = 2000;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Fixed camera advance per frame, so the path is framerate-independent.
const VIRTUAL_DT: f32 = 1.0 / 240.0;

pub fn run() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("no suitable GPU adapter");
    log::info!("GPU: {:?}", adapter.get_info());

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("cubara-bench-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("request device");

    // Held for the duration of the benchmark when built with `--features profile`.
    let _profiler = crate::profiling::Profiler::init();

    // Scene: the same world the live app renders.
    let world = World::generate();
    let chunks = upload_world(&device, &world);
    let look_target = world.look_target();
    let view_radius = world.view_radius();
    log::info!(
        "rendering {WIDTH}x{HEIGHT}, {} chunk draw calls",
        chunks.len()
    );

    // Camera uniform + bind group.
    let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bench-camera"),
        size: std::mem::size_of::<CameraUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let camera_bgl = camera_bind_group_layout(&device);
    let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bench-camera-bind-group"),
        layout: &camera_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: camera_buffer.as_entire_binding(),
        }],
    });

    let pipeline = build_pipeline(&device, COLOR_FORMAT, &camera_bgl);

    // Offscreen render targets.
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-color"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth_view = create_depth_view(&device, WIDTH, HEIGHT);

    let aspect = WIDTH as f32 / HEIGHT as f32;
    let mut virtual_t = 0.0f32;

    // Records one frame (camera upload + frustum cull + render-pass encode +
    // submit) and returns the CPU time spent plus how many chunks were drawn.
    // Frames are not individually waited on, so the GPU pipelines them — this
    // measures sustained throughput.
    let submit_frame = |vt: f32| -> (f64, usize) {
        puffin::profile_scope!("frame");
        let vp = CameraUniform::view_proj_matrix(aspect, vt, look_target, view_radius);
        let uniform = CameraUniform::from_matrix(vp);
        queue.write_buffer(&camera_buffer, 0, bytemuck::bytes_of(&uniform));
        let frustum = Frustum::from_view_proj(vp);

        let cpu_start = Instant::now();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bench-encoder"),
        });
        let mut visible = 0usize;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bench-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
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
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &camera_bind_group, &[]);
            for chunk in &chunks {
                if frustum.intersects_aabb(&chunk.aabb) {
                    chunk.draw(&mut pass);
                    visible += 1;
                }
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
        (cpu_start.elapsed().as_secs_f64() * 1000.0, visible)
    };

    log::info!("warming up ({WARMUP_FRAMES} frames), then measuring {MEASURE_FRAMES}...");

    for _ in 0..WARMUP_FRAMES {
        submit_frame(virtual_t);
        let _ = device.poll(wgpu::Maintain::Poll);
        virtual_t += VIRTUAL_DT;
    }
    let _ = device.poll(wgpu::Maintain::Wait);

    // Measure sustained throughput over wall-clock time, plus per-frame CPU cost.
    let mut cpu_ms: Vec<f64> = Vec::with_capacity(MEASURE_FRAMES as usize);
    let mut visible_sum = 0u64;
    let wall_start = Instant::now();
    for _ in 0..MEASURE_FRAMES {
        crate::profiling::Profiler::new_frame();
        let (ms, visible) = submit_frame(virtual_t);
        cpu_ms.push(ms);
        visible_sum += visible as u64;
        let _ = device.poll(wgpu::Maintain::Poll);
        virtual_t += VIRTUAL_DT;
    }
    let _ = device.poll(wgpu::Maintain::Wait);
    let wall_secs = wall_start.elapsed().as_secs_f64();
    let avg_visible = visible_sum as f64 / MEASURE_FRAMES as f64;

    report(MEASURE_FRAMES, wall_secs, cpu_ms, avg_visible, chunks.len());
}

fn report(
    frames: u32,
    wall_secs: f64,
    mut cpu_ms: Vec<f64>,
    avg_visible: f64,
    total_chunks: usize,
) {
    let throughput = frames as f64 / wall_secs;

    cpu_ms.sort_by(|a, b| a.partial_cmp(b).expect("no NaN frame times"));
    let n = cpu_ms.len();
    let cpu_avg = cpu_ms.iter().sum::<f64>() / n as f64;
    let cpu_p50 = cpu_ms[n / 2];
    let cpu_p99 = cpu_ms[((n as f64 * 0.99) as usize).min(n - 1)];

    log::info!("=========== BENCHMARK RESULT ===========");
    log::info!("frames            : {frames}");
    log::info!("throughput        : {throughput:.0} FPS (sustained, pipelined)");
    log::info!("CPU submit / frame: avg {cpu_avg:.3} ms | p50 {cpu_p50:.3} | p99 {cpu_p99:.3}");
    log::info!("chunks drawn      : avg {avg_visible:.1} / {total_chunks} (frustum-culled)");
    log::info!("========================================");
    log::info!(
        "goal 1000+ FPS: {}",
        if throughput >= 1000.0 {
            "MET"
        } else {
            "not yet"
        }
    );
}
