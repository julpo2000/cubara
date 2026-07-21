//! Headless FPS benchmark.
//!
//! Renders the multi-chunk world to an offscreen target with no window and no
//! vsync, submitting frames pipelined (not waited on per-frame) so we measure real
//! sustained throughput against the 1000-FPS goal. A fixed virtual time step keeps
//! the camera orbit identical regardless of how fast the machine runs.
//!
//! Run with: `cargo run --release -- --bench`

use std::time::Instant;

use cubara_render::{gpu_driven_features, CameraUniform, ChunkArena, Frustum, SceneRenderer};
use cubara_voxel::ChunkCoord;
use cubara_world::World;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const WARMUP_FRAMES: u32 = 200;
const MEASURE_FRAMES: u32 = 2000;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Fixed camera advance per frame, so the path is framerate-independent.
const VIRTUAL_DT: f32 = 1.0 / 240.0;

/// Run the benchmark over a streamed square region of the given chunk `radius`
/// (default 12 = a realistically heavy world). Chunks are streamed at their
/// distance LOD, so a larger radius shows how far render distance can grow without
/// the triangle cost exploding.
pub fn run(radius: i32) {
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

    let (features, multi_draw) = gpu_driven_features(&adapter);
    log::info!("multi_draw_indirect: {multi_draw}");

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("cubara-bench-device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("request device");

    // Held for the duration of the benchmark when built with `--features profile`.
    let _profiler = cubara_render::Profiler::init();

    // Scene: a streamed square region (the same path the live renderer uses), so
    // we measure a realistically heavy world instead of the tiny fixed grid. All
    // geometry goes into one shared arena, drawn with a single indirect submit.
    let world = World::new();
    let mut arena = ChunkArena::from_region(
        &device,
        &queue,
        multi_draw,
        &world,
        ChunkCoord::new(0, 0, 0),
        radius,
        0..=2,
    );
    let total_chunks = arena.len();
    let (min, max) = arena.bounds().expect("bench region produced no geometry");
    let look_target = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let view_radius = (max[0] - min[0]).max(max[2] - min[2]) * 0.75;
    log::info!(
        "rendering {WIDTH}x{HEIGHT}, {total_chunks} chunks via {}",
        if multi_draw {
            "1 multi_draw_indirect"
        } else {
            "draw_indexed loop"
        }
    );

    // The same scene renderer the window uses — ARCHITECTURE.md Rule 5.
    let mut scene = SceneRenderer::new(&device, &queue, COLOR_FORMAT, WIDTH, HEIGHT);

    // Offscreen colour target (the window's equivalent is the surface texture).
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

    let aspect = WIDTH as f32 / HEIGHT as f32;
    let mut virtual_t = 0.0f32;

    // Records one frame (camera upload + frustum cull + indirect-list upload +
    // render-pass encode + submit) and returns the CPU time spent plus how many
    // chunks were drawn. Frames are not individually waited on, so the GPU
    // pipelines them — this measures sustained throughput.
    // `scene` is borrowed mutably here, so this is a closure over it rather than a
    // plain fn: same shared encode_scene the window calls, no bench-local copy.
    let submit_frame =
        |arena: &mut ChunkArena, scene: &mut SceneRenderer, vt: f32| -> (f64, usize) {
            puffin::profile_scope!("frame");
            let vp = CameraUniform::view_proj_matrix(aspect, vt, look_target, view_radius);
            scene.set_camera(&queue, vp);
            let frustum = Frustum::from_view_proj(vp);

            let cpu_start = Instant::now();
            // CPU cull + indirect-list upload — the per-frame work we're measuring.
            let draw_count = arena.prepare(&queue, &frustum);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bench-encoder"),
            });
            // No overlay: the bench measures the world, not the debug HUD.
            scene.encode_scene(&queue, &mut encoder, &color_view, arena, draw_count, None);
            queue.submit(std::iter::once(encoder.finish()));
            (
                cpu_start.elapsed().as_secs_f64() * 1000.0,
                draw_count as usize,
            )
        };

    log::info!("warming up ({WARMUP_FRAMES} frames), then measuring {MEASURE_FRAMES}...");

    for _ in 0..WARMUP_FRAMES {
        submit_frame(&mut arena, &mut scene, virtual_t);
        let _ = device.poll(wgpu::Maintain::Poll);
        virtual_t += VIRTUAL_DT;
    }
    let _ = device.poll(wgpu::Maintain::Wait);

    // Measure sustained throughput over wall-clock time, plus per-frame CPU cost.
    let mut cpu_ms: Vec<f64> = Vec::with_capacity(MEASURE_FRAMES as usize);
    let mut visible_sum = 0u64;
    let wall_start = Instant::now();
    for _ in 0..MEASURE_FRAMES {
        cubara_render::Profiler::new_frame();
        let (ms, visible) = submit_frame(&mut arena, &mut scene, virtual_t);
        cpu_ms.push(ms);
        visible_sum += visible as u64;
        let _ = device.poll(wgpu::Maintain::Poll);
        virtual_t += VIRTUAL_DT;
    }
    let _ = device.poll(wgpu::Maintain::Wait);
    let wall_secs = wall_start.elapsed().as_secs_f64();
    let avg_visible = visible_sum as f64 / MEASURE_FRAMES as f64;

    report(MEASURE_FRAMES, wall_secs, cpu_ms, avg_visible, total_chunks);
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
    // Lead with the numbers so every run is a data point for the performance
    // history in BENCHMARKS.md; the 1000-FPS gate is just a trailing tag now.
    let gate = if throughput >= 1000.0 {
        "MET"
    } else {
        "NOT MET"
    };
    log::info!(
        "SUMMARY: {throughput:.0} FPS | CPU/frame avg {cpu_avg:.3} ms (p99 {cpu_p99:.3}) | \
         {avg_visible:.0}/{total_chunks} chunks | 1000-FPS gate {gate}"
    );
}
