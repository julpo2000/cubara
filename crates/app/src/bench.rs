//! Headless FPS benchmark.
//!
//! Renders the multi-chunk world to an offscreen target with no window and no
//! vsync, submitting frames pipelined (not waited on per-frame) so we measure real
//! sustained throughput against the 1000-FPS goal. A fixed virtual time step keeps
//! the camera orbit identical regardless of how fast the machine runs.
//!
//! Run with: `cargo run --release -- --bench [radius] [--size WIDTHxHEIGHT] [--overlay]`
//!
//! **Resolution matters, and 1920x1080 is only the default.** Frame cost has a
//! part that grows with pixels -- every covered pixel is shaded -- and a bench
//! pinned to one size cannot see it. The owner noticed FPS dropping as the
//! window grew; `--size` is how that is measured rather than guessed. The
//! history in `BENCHMARKS.md` is all at the default, so rows stay comparable.
//!
//! `--overlay` draws through the same debug-text path the window's F3 overlay
//! does (off by default in both places, `render.rs`'s `show_debug`), so its
//! cost shows up in the numbers rather than being silently excluded.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use cubara_render::{
    gpu_driven_features, load_mesh_assets, CameraUniform, ChunkArena, Frustum, GpuTimestamps,
    SceneFrame, SceneRenderer, TimestampRing,
};
use cubara_voxel::ChunkCoord;
use cubara_world::mesh::mesh_nodes;
use cubara_world::node::{desired_nodes, desired_nodes_3d, schedule_for_radius};
use cubara_world::World;

use crate::streaming::to_meshed_node;

/// The resolution every row in `BENCHMARKS.md` was measured at.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
const WARMUP_FRAMES: u32 = 200;
const MEASURE_FRAMES: u32 = 2000;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Fixed camera advance per frame, so the path is framerate-independent.
const VIRTUAL_DT: f32 = 1.0 / 240.0;

/// How many frames of slack the GPU-timestamp readback keeps in flight
/// (`cubara_render::TimestampRing`). Started at 3 -- enough that a slot's map
/// isn't needed again the instant it's written -- but measured on this
/// machine at only 27 of 2000 measured frames actually producing a GPU
/// sample: the CPU submits far faster than the GPU retires work (the whole
/// point of "sustained pipelined throughput"), so a shallow ring runs out of
/// free slots almost immediately and spends most frames skipping. 1024
/// brings that to ~975/2000 (~49%) on this machine -- plenty for a stable
/// avg/p99 -- at a cost that's still just small buffers (a `depth`-sized
/// query set and `depth` 16-byte readback buffers, created once).
const GPU_TIMER_DEPTH: usize = 1024;

/// The two query-set indices `slot` writes its begin/end timestamps to.
/// Pulled out of [`GpuTimer`] as plain arithmetic (no `&self`, no wgpu) so it
/// is unit-tested without a device: an off-by-one here would have a slot's
/// `end` alias the next slot's `begin`, silently mixing two frames' readings.
fn timestamp_indices(slot: usize) -> (u32, u32) {
    let begin = (slot * 2) as u32;
    (begin, begin + 1)
}

/// Owns a timestamp query set and its readback buffers around the main scene
/// pass, and turns completed reads into milliseconds. The bookkeeping for
/// which slot is safe to write or read is [`TimestampRing`]
/// (`cubara-render`, unit-tested there with no GPU involved); this is the
/// thin GPU-owning wrapper around it.
struct GpuTimer {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    read_buffers: Vec<wgpu::Buffer>,
    ring: Arc<Mutex<TimestampRing>>,
    period_ns: f64,
}

impl GpuTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("bench-gpu-timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: (GPU_TIMER_DEPTH * 2) as u32,
        });
        // Each slot's region in the resolve buffer must start at a multiple
        // of `wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT` (256 bytes) -- the two
        // 8-byte timestamps it actually holds don't need that much room, but
        // `resolve_query_set` validates the destination offset regardless.
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-gpu-timestamps-resolve"),
            size: (GPU_TIMER_DEPTH as u64) * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read_buffers = (0..GPU_TIMER_DEPTH)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("bench-gpu-timestamps-read"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        Self {
            query_set,
            resolve_buffer,
            read_buffers,
            ring: Arc::new(Mutex::new(TimestampRing::new(GPU_TIMER_DEPTH))),
            period_ns: queue.get_timestamp_period() as f64,
        }
    }

    /// This frame's slot to write into, or `None` if every slot still has
    /// outstanding GPU work -- skip GPU timing this frame rather than stall
    /// waiting for one to free up or overwrite one still in flight.
    fn write_slot(&self, frame: u64) -> Option<usize> {
        let slot = (frame as usize) % GPU_TIMER_DEPTH;
        self.ring.lock().unwrap().can_write(slot).then_some(slot)
    }

    fn timestamps(&self, slot: usize) -> GpuTimestamps<'_> {
        let (begin, end) = timestamp_indices(slot);
        GpuTimestamps {
            query_set: &self.query_set,
            begin,
            end,
        }
    }

    /// Resolve `slot`'s two timestamps into the readback buffer -- called
    /// within the same encoder that wrote them, before submit.
    fn resolve(&self, encoder: &mut wgpu::CommandEncoder, slot: usize) {
        let src_offset = (slot as u64) * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
        let (begin, end) = timestamp_indices(slot);
        encoder.resolve_query_set(
            &self.query_set,
            begin..end + 1,
            &self.resolve_buffer,
            src_offset,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            src_offset,
            &self.read_buffers[slot],
            0,
            16,
        );
    }

    /// Call after submit: marks `slot` in-flight and starts its async map.
    fn begin_read(&self, slot: usize) {
        self.ring.lock().unwrap().begin_mapping(slot);
        let ring = Arc::clone(&self.ring);
        self.read_buffers[slot]
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // An error leaves the slot `Mapping` forever -- it just never
                // contributes another GPU sample, which beats panicking a
                // whole benchmark run over one dropped reading.
                if result.is_ok() {
                    ring.lock().unwrap().mark_ready(slot);
                }
            });
    }

    /// If `slot`'s async map has completed, read its two timestamps, unmap,
    /// and return the pass duration in milliseconds.
    fn take_ms(&self, slot: usize) -> Option<f64> {
        if !self.ring.lock().unwrap().take_ready(slot) {
            return None;
        }
        let buf = &self.read_buffers[slot];
        let ms = {
            let data = buf.slice(..).get_mapped_range();
            let raw: &[u64] = bytemuck::cast_slice(&data);
            let (begin, end) = (raw[0], raw[1]);
            end.saturating_sub(begin) as f64 * self.period_ns / 1_000_000.0
        };
        buf.unmap();
        Some(ms)
    }
}

/// Run the benchmark over a streamed square region of the given chunk `radius`
/// (default 12 = a realistically heavy world). The region streams as LOD nodes
/// at their distance-based level (`cubara_world::mesh::mesh_region`,
/// `schedule_for_radius`), so a larger radius shows how far render distance
/// can grow without the draw/triangle cost exploding.
/// Where the camera is, for [`run`].
#[derive(Clone, Copy, Debug, Default)]
pub struct View {
    /// A first-person camera at this eye position, turning slowly on the spot
    /// and looking a little down. `None` is the orbit above the whole region
    /// every earlier row was measured with.
    pub eye: Option<[f32; 3]>,
    /// Select nodes in three dimensions with this vertical squash
    /// (`cubara_world::node::desired_nodes_3d`) -- by default the one the game
    /// streams with. `None` is the old ±2-layer band, kept so the rows measured
    /// with it can still be compared against (`--band`).
    pub squash: Option<i32>,
}

pub fn run(radius: i32, (width, height): (u32, u32), view: View, overlay: bool) {
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
    let gpu_timing_supported = features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

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

    let gpu_timer = gpu_timing_supported.then(|| GpuTimer::new(&device, &queue));
    if !gpu_timing_supported {
        log::info!("GPU/frame: n/a (no TIMESTAMP_QUERY_INSIDE_ENCODERS)");
    }

    // Held for the duration of the benchmark when built with `--features profile`.
    let _profiler = cubara_render::Profiler::init();

    // Scene: a streamed square region (the same path the live renderer uses), so
    // we measure a realistically heavy world instead of the tiny fixed grid. All
    // geometry goes into one shared arena, drawn with a single indirect submit.
    // Meshing happens through `cubara_world::mesh` (this crate is the one place
    // allowed to depend on both `cubara-render` and `cubara-world`,
    // `ARCHITECTURE.md` §1) and hands the renderer already-built geometry.
    let world = World::new();
    let (mesh_assets, tex_view, tex_sampler) = load_mesh_assets(&device, &queue);
    let layer_of = |name: &str| mesh_assets.layers.layer_of(name);
    let schedule = schedule_for_radius(radius);
    let center = match view.eye {
        Some(eye) => ChunkCoord::from_world_pos(eye),
        None => ChunkCoord::new(0, 0, 0),
    };
    let nodes = match view.squash {
        Some(k) => desired_nodes_3d(center, k, &schedule),
        // The same player-relative band the live game streamed, centred on the
        // bench origin. Measuring the old fixed 0..=2 slab would measure a
        // world the game no longer builds.
        None => desired_nodes(center, (center.y - 2)..=(center.y + 2), &schedule),
    };
    let meshing = Instant::now();
    let meshed = mesh_nodes(
        &world,
        &mesh_assets.registry,
        &layer_of,
        nodes,
        cubara_world::TerrainBlocks::from_registry(&mesh_assets.registry)
            .with_oak(
                &crate::game::load_structure_registry(),
                &mesh_assets.registry,
            )
            .with_ores(&crate::game::load_ore_registry(), &mesh_assets.registry),
    );
    log::info!(
        "meshed in {:.2} s (single thread)",
        meshing.elapsed().as_secs_f64()
    );
    // Every camera draws only what it could see, the way the game does
    // (`cubara_world::visibility`). The orbit's path is fixed by the whole
    // region's bounds -- worked out before culling, so what is culled cannot
    // move the camera -- and it moves, so it draws the union of what is visible
    // from points along the arc it covers.
    let built_nodes = meshed.len();
    let mut lo = glam::Vec3::splat(f32::MAX);
    let mut hi = glam::Vec3::splat(f32::MIN);
    for g in meshed.iter().filter_map(|b| b.geometry.as_ref()) {
        lo = lo.min(g.aabb.min);
        hi = hi.max(g.aabb.max);
    }
    let look_target = ((lo + hi) * 0.5).to_array();
    let view_radius = (hi.x - lo.x).max(hi.z - lo.z) * 0.75;
    let eyes: Vec<[f32; 3]> = match view.eye {
        Some(eye) => vec![eye],
        None => {
            let frames = (WARMUP_FRAMES + MEASURE_FRAMES) as f32;
            let samples = 64;
            (0..=samples)
                .map(|i| {
                    let t = frames * VIRTUAL_DT * i as f32 / samples as f32;
                    orbit_eye(t, look_target, view_radius)
                })
                .collect()
        }
    };
    let desired: std::collections::HashSet<_> = meshed.iter().map(|b| b.node).collect();
    let links: std::collections::HashMap<_, _> = meshed.iter().map(|b| (b.node, b.links)).collect();
    let searching = Instant::now();
    let mut visible = std::collections::HashSet::new();
    for eye in &eyes {
        visible.extend(cubara_world::visibility::visible_nodes(
            ChunkCoord::from_world_pos(*eye),
            &desired,
            |n| links.get(&n).copied(),
        ));
    }
    log::info!(
        "visible: {} of {} nodes from {} camera position(s), searched in {:.1} ms",
        visible.len(),
        built_nodes,
        eyes.len(),
        searching.elapsed().as_secs_f64() * 1000.0
    );
    let mut arena = ChunkArena::from_meshed(
        &device,
        &queue,
        multi_draw,
        meshed
            .into_iter()
            .filter(|b| visible.contains(&b.node))
            .filter_map(to_meshed_node),
    );
    let total_nodes = arena.len();
    // A scene with nothing in it renders very fast, and would pass the gate.
    assert!(
        total_nodes > 0,
        "the bench scene drew nothing -- the measurement would mean nothing"
    );
    // Nor would a scene the arena could not hold: it skips what does not fit,
    // and the frame rate of a world with holes in it is not the one asked for.
    let full = arena.usage().exhausted();
    assert!(
        full.is_empty(),
        "the arena ran out of {full:?} -- the bench would measure a world with parts missing"
    );
    log::info!(
        "rendering {width}x{height}, {total_nodes} nodes via {}",
        if multi_draw {
            "1 multi_draw_indirect"
        } else {
            "draw_indexed loop"
        }
    );

    // The same scene renderer the window uses — ARCHITECTURE.md Rule 5.
    let mut scene = SceneRenderer::new(
        &device,
        &queue,
        COLOR_FORMAT,
        width,
        height,
        &tex_view,
        &tex_sampler,
    );

    // Offscreen colour target (the window's equivalent is the surface texture).
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-color"),
        size: wgpu::Extent3d {
            width,
            height,
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

    let aspect = width as f32 / height as f32;
    let mut virtual_t = 0.0f32;

    // Records one frame (camera upload + frustum cull + indirect-list upload +
    // render-pass encode + submit) and returns the CPU time spent plus how many
    // draws/chunks were drawn. Frames are not individually waited on, so the GPU
    // pipelines them — this measures sustained throughput.
    // `scene` is borrowed mutably here, so this is a closure over it rather than a
    // plain fn: same shared encode_scene the window calls, no bench-local copy.
    //
    // The timed window starts before the camera upload and frustum build, not
    // after: both are real per-frame CPU cost (a `queue.write_buffer` plus six
    // plane extractions), and excluding them understated what "CPU/frame" claims
    // to measure.
    let submit_frame = |arena: &mut ChunkArena,
                        scene: &mut SceneRenderer,
                        vt: f32,
                        gpu_timer: Option<&GpuTimer>,
                        frame_index: u64|
     -> (f64, u32, usize) {
        puffin::profile_scope!("frame");
        let cpu_start = Instant::now();
        let vp = match view.eye {
            Some(eye) => {
                // A full turn every ~20 virtual seconds, pitched down a
                // little: what a player looking around sees.
                let yaw = vt * 0.3;
                let dir = glam::vec3(yaw.cos(), -0.25, yaw.sin());
                CameraUniform::look_view_proj(aspect, glam::Vec3::from(eye), dir)
            }
            None => CameraUniform::view_proj_matrix(aspect, vt, look_target, view_radius),
        };
        scene.set_camera(&queue, vp);
        let frustum = Frustum::from_view_proj(vp);

        // CPU cull + indirect-list upload — the per-frame work we're measuring.
        let draw_count = arena.prepare(&queue, &frustum);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bench-encoder"),
        });
        let overlay_text = overlay.then(|| {
            format!(
                "cubara --bench  ({width}x{height})\n\
                 draws {draws}  nodes {visible}/{total_nodes}",
                draws = draw_count,
                visible = arena.visible_nodes(),
            )
        });
        let gpu_slot = gpu_timer.and_then(|t| t.write_slot(frame_index));
        // No selected block, no players/hotbar/panel/health/crosshair: the
        // bench measures the world, not a UI the game overlays on top of it.
        // `--overlay` draws the same debug-text path the F3 overlay does
        // (through the one shared `encode_scene`, ARCHITECTURE.md Rule 5),
        // with bench-specific content rather than the live HUD's, since the
        // bench has no smoothed frame time or player position to show.
        scene.encode_scene(
            &device,
            &queue,
            &mut encoder,
            &color_view,
            SceneFrame {
                arena,
                draw_count,
                selected_block: None,
                cracking: None,
                players: &[],
                overlay: overlay_text.as_deref(),
                health: None,
                hotbar: None,
                panel: None,
                crosshair: false,
                gpu_timestamps: gpu_slot.map(|slot| gpu_timer.unwrap().timestamps(slot)),
            },
        );
        if let (Some(timer), Some(slot)) = (gpu_timer, gpu_slot) {
            timer.resolve(&mut encoder, slot);
        }
        queue.submit(std::iter::once(encoder.finish()));
        if let (Some(timer), Some(slot)) = (gpu_timer, gpu_slot) {
            timer.begin_read(slot);
        }
        (
            cpu_start.elapsed().as_secs_f64() * 1000.0,
            draw_count,
            arena.visible_nodes() as usize,
        )
    };

    log::info!("warming up ({WARMUP_FRAMES} frames), then measuring {MEASURE_FRAMES}...");

    let mut frame_index = 0u64;
    for _ in 0..WARMUP_FRAMES {
        submit_frame(
            &mut arena,
            &mut scene,
            virtual_t,
            gpu_timer.as_ref(),
            frame_index,
        );
        let _ = device.poll(wgpu::Maintain::Poll);
        virtual_t += VIRTUAL_DT;
        frame_index += 1;
    }
    let _ = device.poll(wgpu::Maintain::Wait);
    // Warmup's GPU-timing slots are never read back — only measurement
    // frames count — so drain whatever they left ready rather than let a
    // stale reading leak into the first measured sample.
    if let Some(timer) = &gpu_timer {
        for slot in 0..GPU_TIMER_DEPTH {
            timer.take_ms(slot);
        }
    }

    // Measure sustained throughput over wall-clock time, plus per-frame CPU cost.
    let mut cpu_ms: Vec<f64> = Vec::with_capacity(MEASURE_FRAMES as usize);
    let mut gpu_ms: Vec<f64> = Vec::new();
    let mut draws_sum = 0u64;
    let mut visible_sum = 0u64;
    let mut triangles_sum = 0u64;
    let wall_start = Instant::now();
    for _ in 0..MEASURE_FRAMES {
        cubara_render::Profiler::new_frame();
        let (ms, draws, visible) = submit_frame(
            &mut arena,
            &mut scene,
            virtual_t,
            gpu_timer.as_ref(),
            frame_index,
        );
        cpu_ms.push(ms);
        draws_sum += draws as u64;
        visible_sum += visible as u64;
        triangles_sum += arena.visible_triangles();
        let _ = device.poll(wgpu::Maintain::Poll);
        // One slot per frame, round-robin, not all `GPU_TIMER_DEPTH` of them:
        // scanning every slot every frame was measured to slow down
        // *subsequent* frames' CPU submit time even though the scan itself
        // sits outside `submit_frame`'s timed window -- `take_ms`'s
        // `get_mapped_range`/`unmap` contend with wgpu's internal device
        // locking that `arena.prepare`/`create_command_encoder` also use, so
        // a big scan leaks into the next frame's numbers. One check per frame
        // still visits every slot once every `GPU_TIMER_DEPTH` frames, which
        // is plenty -- this is sampling for a distribution, not a per-frame
        // reading each frame needs by the next frame.
        if let Some(timer) = &gpu_timer {
            let check_slot = (frame_index as usize) % GPU_TIMER_DEPTH;
            if let Some(ms) = timer.take_ms(check_slot) {
                gpu_ms.push(ms);
            }
        }
        virtual_t += VIRTUAL_DT;
        frame_index += 1;
    }
    let _ = device.poll(wgpu::Maintain::Wait);
    let wall_secs = wall_start.elapsed().as_secs_f64();
    let avg_draws = draws_sum as f64 / MEASURE_FRAMES as f64;
    let avg_visible = visible_sum as f64 / MEASURE_FRAMES as f64;
    log::info!(
        "triangles drawn: avg {:.0} (faces turned away from the camera left out)",
        triangles_sum as f64 / MEASURE_FRAMES as f64
    );

    report(
        MEASURE_FRAMES,
        wall_secs,
        cpu_ms,
        gpu_ms,
        avg_draws,
        avg_visible,
        total_nodes,
    );
}

#[allow(clippy::too_many_arguments)]
fn report(
    frames: u32,
    wall_secs: f64,
    mut cpu_ms: Vec<f64>,
    mut gpu_ms: Vec<f64>,
    avg_draws: f64,
    avg_visible: f64,
    total_nodes: usize,
) {
    let throughput = frames as f64 / wall_secs;

    cpu_ms.sort_by(|a, b| a.partial_cmp(b).expect("no NaN frame times"));
    let n = cpu_ms.len();
    let cpu_avg = cpu_ms.iter().sum::<f64>() / n as f64;
    let cpu_p50 = cpu_ms[n / 2];
    let cpu_p99 = cpu_ms[((n as f64 * 0.99) as usize).min(n - 1)];

    // GPU/frame: how long the main scene pass itself took on the GPU,
    // separate from `CPU submit / frame` above -- at high resolutions the
    // two move together (submit stalls on a full GPU), which is exactly the
    // GPU-backpressure `CPU/frame` alone can't tell apart from real CPU cost.
    // `None` when the device has no `TIMESTAMP_QUERY_INSIDE_ENCODERS`, or
    // (in principle) if every readback is still in flight at report time.
    // How many of `frames` actually got a GPU reading -- the ring skips a
    // frame's timing rather than stall when a readback is still in flight
    // (see `GpuTimer::write_slot`), so a p99 built from far fewer samples
    // than `frames` would otherwise look identical to one from all of them.
    let gpu_sample_count = gpu_ms.len();
    let gpu_stats = (!gpu_ms.is_empty()).then(|| {
        gpu_ms.sort_by(|a, b| a.partial_cmp(b).expect("no NaN GPU frame times"));
        let n = gpu_ms.len();
        let avg = gpu_ms.iter().sum::<f64>() / n as f64;
        let p99 = gpu_ms[((n as f64 * 0.99) as usize).min(n - 1)];
        (avg, p99)
    });
    let gpu_line = match gpu_stats {
        Some((avg, p99)) => {
            format!("avg {avg:.3} ms | p99 {p99:.3} | samples {gpu_sample_count}/{frames}")
        }
        None => "n/a".to_string(),
    };

    log::info!("=========== BENCHMARK RESULT ===========");
    log::info!("frames            : {frames}");
    log::info!("throughput        : {throughput:.0} FPS (sustained, pipelined)");
    log::info!("CPU submit / frame: avg {cpu_avg:.3} ms | p50 {cpu_p50:.3} | p99 {cpu_p99:.3}");
    log::info!("GPU pass / frame  : {gpu_line}");
    log::info!("draws issued      : avg {avg_draws:.1}");
    log::info!("nodes drawn       : avg {avg_visible:.1} / {total_nodes} (frustum-culled)");
    match peak_rss_mib() {
        Some(mib) => log::info!("peak RSS          : {mib:.0} MiB"),
        None => log::info!("peak RSS          : n/a"),
    }
    log::info!("========================================");
    // Lead with the numbers so every run is a data point for the performance
    // history in BENCHMARKS.md; the 1000-FPS gate is just a trailing tag now.
    let gate = if throughput >= 1000.0 {
        "MET"
    } else {
        "NOT MET"
    };
    let gpu_summary = match gpu_stats {
        Some((avg, p99)) => {
            format!("GPU/frame avg {avg:.3} ms (p99 {p99:.3}, {gpu_sample_count} samples)")
        }
        None => "GPU/frame n/a".to_string(),
    };
    log::info!(
        "SUMMARY: {throughput:.0} FPS | CPU/frame avg {cpu_avg:.3} ms (p99 {cpu_p99:.3}) | \
         {gpu_summary} | {avg_draws:.0} draws ({avg_visible:.0}/{total_nodes} nodes) | \
         1000-FPS gate {gate}"
    );
}

/// Peak resident set size since process start, in MiB -- `VmHWM` from
/// `/proc/self/status`. `None` off Linux: `getrusage`'s `ru_maxrss` would
/// cover macOS/Windows too, but that's a `libc` dependency this crate
/// doesn't otherwise need, so it's left for whoever next measures on those
/// platforms to add rather than pulled in for a `n/a` line to say less often.
fn peak_rss_mib() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kib: f64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
                return Some(kib / 1024.0);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Parse a `--size` value: `WIDTHxHEIGHT`, both positive, e.g. `2560x1440`.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Where the orbit camera is at virtual time `t` -- the same path
/// `CameraUniform::view_proj_matrix` draws from.
fn orbit_eye(t: f32, center: [f32; 3], radius: f32) -> [f32; 3] {
    CameraUniform::orbit_eye(t, center, radius)
}

/// Parse a `--eye` value: `X,Y,Z` in blocks, e.g. `8,40,8`.
pub fn parse_eye(text: &str) -> Option<[f32; 3]> {
    let parts: Vec<f32> = text
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    <[f32; 3]>::try_from(parts).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_slots_timestamp_indices_are_adjacent_and_slots_never_overlap() {
        let mut seen = std::collections::HashSet::new();
        for slot in 0..GPU_TIMER_DEPTH {
            let (begin, end) = timestamp_indices(slot);
            assert_eq!(
                end,
                begin + 1,
                "slot {slot}'s end index must immediately follow its begin index"
            );
            assert!(
                seen.insert(begin) && seen.insert(end),
                "slot {slot}'s indices ({begin}, {end}) overlap an earlier slot's -- \
                 two slots would alias the same GPU timestamp"
            );
        }
    }

    /// A real device (or `None` on a CI runner with no GPU adapter, or one
    /// whose driver lacks `TIMESTAMP_QUERY_INSIDE_ENCODERS`) -- the same
    /// skip-loudly convention `mesh_arena_integration.rs` uses.
    fn test_gpu_timer_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (features, _) = gpu_driven_features(&adapter);
        if !features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
            return None;
        }
        pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cubara-test-gpu-timer-device"),
                required_features: features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()
    }

    /// Pins `GpuTimer::take_ms`'s `take_ready` guard (the other half of the
    /// timestamp-ring safety the mutation check flagged as untested): it must
    /// return `None` both before anything was ever submitted for a slot and
    /// in the window after submit where the async map has not completed yet
    /// -- reading either would be reading a buffer that isn't mapped.
    #[test]
    fn take_ms_only_returns_a_value_once_the_slot_has_actually_finished_mapping() {
        let Some((device, queue)) = test_gpu_timer_device() else {
            eprintln!(
                "SKIP take_ms_only_returns_a_value_once_the_slot_has_actually_finished_mapping: \
                 no GPU adapter, or no TIMESTAMP_QUERY_INSIDE_ENCODERS"
            );
            return;
        };
        let timer = GpuTimer::new(&device, &queue);
        let slot = timer
            .write_slot(0)
            .expect("a fresh timer's slot 0 must be writable");
        assert_eq!(
            timer.take_ms(slot),
            None,
            "nothing has been submitted for this slot yet"
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test-gpu-timer-encoder"),
        });
        let ts = timer.timestamps(slot);
        encoder.write_timestamp(ts.query_set, ts.begin);
        encoder.write_timestamp(ts.query_set, ts.end);
        timer.resolve(&mut encoder, slot);
        queue.submit(std::iter::once(encoder.finish()));
        timer.begin_read(slot);

        assert_eq!(
            timer.take_ms(slot),
            None,
            "the async map cannot have completed synchronously with begin_read"
        );

        // `Maintain::Wait` (not `Poll`, which this test found out the hard
        // way): on a software adapter (lavapipe on Linux CI, WARP on
        // Windows CI) a bare `Poll` loop can spin past any fixed iteration
        // count without the map ever completing, since nothing forces the
        // backend to make progress. `Wait` blocks until it has -- the same
        // pattern `headless.rs` and this file's own warmup drain use.
        let _ = device.poll(wgpu::Maintain::Wait);
        let ms = timer
            .take_ms(slot)
            .expect("Maintain::Wait must block until the map has completed");
        assert!(
            ms >= 0.0,
            "a pass duration read back off the GPU cannot be negative"
        );
    }

    #[test]
    fn an_eye_is_three_numbers() {
        assert_eq!(parse_eye("8,40.5,-3"), Some([8.0, 40.5, -3.0]));
        for bad in ["", "1,2", "1,2,3,4", "a,b,c"] {
            assert_eq!(parse_eye(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_size_is_width_x_height() {
        assert_eq!(parse_size("2560x1440"), Some((2560, 1440)));
        assert_eq!(parse_size("1280X720"), Some((1280, 720)));
    }

    #[test]
    fn a_malformed_size_is_refused_rather_than_guessed() {
        for bad in [
            "", "1920", "x1080", "1920x", "0x1080", "1920x0", "-1x5", "wide",
        ] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
    }
}
