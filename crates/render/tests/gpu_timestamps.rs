//! Confirms `SceneFrame::gpu_timestamps` produces a real GPU-side reading
//! through the actual single render path (`SceneRenderer::encode_scene`,
//! `ARCHITECTURE.md` Rule 5) -- not a hand-rolled render pass, which
//! `scripts/check-single-render-path.sh` would (rightly) refuse anyway.
//!
//! This exists because the encoder-level write this used at first
//! (`CommandEncoder::write_timestamp` outside the pass) passed every test
//! and every CI job while being silently wrong on Metal: both its begin and
//! end timestamps sample the same "stage boundary" there and come back
//! identical, so `GPU/frame` read a permanent, plausible-looking 0ms. Only
//! running it on real hardware (the M3, in the cross-session review this is
//! part of) caught it. The fix -- writing via the pass's own
//! `timestamp_writes` -- needs pinning against a real device precisely
//! because it's the kind of thing that looks fine everywhere except the one
//! backend that renders it differently.

use cubara_render::{
    gpu_driven_features, ChunkArena, GpuTimestamps, SceneFrame, SceneRenderer, TimestampRing,
};

/// A device with the timestamp-query features this test needs, or `None` on
/// a CI runner with no GPU adapter, or one whose driver lacks
/// `TIMESTAMP_QUERY` -- the same skip-loudly convention
/// `mesh_arena_integration.rs`'s `test_device()` uses.
fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (features, _) = gpu_driven_features(&adapter);
    if !features.contains(wgpu::Features::TIMESTAMP_QUERY) {
        return None;
    }
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cubara-test-gpu-timestamps-device"),
        required_features: features,
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .ok()
}

#[test]
fn gpu_timestamps_through_encode_scene_produce_a_real_reading() {
    let Some((device, queue)) = test_device() else {
        eprintln!(
            "SKIP gpu_timestamps_through_encode_scene_produce_a_real_reading: \
             no GPU adapter, or no TIMESTAMP_QUERY"
        );
        return;
    };

    let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("test-query-set"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-resolve"),
        size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-read"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    // An arena with nothing in it: this test is about whether the query
    // mechanism itself works, not about scene content -- that's the golden
    // tests' job. `load_mesh_assets` is still needed for a valid texture
    // array to bind (`materials::bind_group_layout` expects `D2Array`, which
    // a plain render-target texture isn't) -- the same real
    // `assets/`-backed array every other entry point binds.
    let arena = ChunkArena::new(&device, false);
    let (_mesh_assets, tex_view, tex_sampler) = cubara_render::load_mesh_assets(&device, &queue);
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-color"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let mut scene = SceneRenderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        4,
        4,
        &tex_view,
        &tex_sampler,
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test-encoder"),
    });
    scene.encode_scene(
        &device,
        &queue,
        &mut encoder,
        &color_view,
        SceneFrame {
            arena: &arena,
            draw_count: 0,
            selected_block: None,
            cracking: None,
            players: &[],
            overlay: None,
            hotbar: None,
            panel: None,
            health: None,
            crosshair: false,
            gpu_timestamps: Some(GpuTimestamps {
                query_set: &query_set,
                begin: 0,
                end: 1,
            }),
        },
    );
    encoder.resolve_query_set(&query_set, 0..2, &resolve_buffer, 0);
    encoder.copy_buffer_to_buffer(&resolve_buffer, 0, &read_buffer, 0, 16);
    queue.submit(std::iter::once(encoder.finish()));

    // One write, one read: `TimestampRing`'s own unit tests (`gpu_timer.rs`)
    // already pin the multi-slot bookkeeping; this drives its single-slot
    // can_write/begin_mapping/mark_ready/take_ready protocol against a real
    // `map_async` this time, via a flag the callback sets (the same shape
    // `bench.rs`'s `GpuTimer::begin_read` uses with `Arc<Mutex<TimestampRing>>`,
    // simplified to one slot here since this test is about the timestamps,
    // not the ring).
    let mut ring = TimestampRing::new(1);
    ring.begin_mapping(0);
    let mapped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mapped_writer = std::sync::Arc::clone(&mapped);
    read_buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| {
            r.expect("map readback");
            mapped_writer.store(true, std::sync::atomic::Ordering::Release);
        });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !mapped.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "GPU map did not complete within 10s -- likely hung"
        );
        let _ = device.poll(wgpu::PollType::Poll);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    ring.mark_ready(0);
    assert!(ring.take_ready(0));

    let data = read_buffer.slice(..).get_mapped_range();
    let raw: &[u64] = bytemuck::cast_slice(&data);
    let (begin, end) = (raw[0], raw[1]);
    drop(data);
    read_buffer.unmap();

    // On Metal, the bug this test exists to catch made `end == begin`
    // always -- a silent, permanent 0ms. `end > begin` here means the
    // pass-scoped write actually measured something, on whatever backend
    // this ran on.
    assert!(
        end > begin,
        "pass-scoped GPU timestamps did not advance (begin={begin}, end={end}) -- \
         this is exactly the bug that made GPU/frame silently read 0ms on Metal"
    );
}
