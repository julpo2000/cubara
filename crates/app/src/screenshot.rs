//! Headless single-frame screenshot, for visual verification without a window.
//!
//! Renders the world once to an offscreen target, reads it back, and writes a PNG.
//! Run with: `cargo run --release -- --screenshot out.png`

use cubara_render::{gpu_driven_features, CameraUniform, ChunkArena, Frustum, SceneRenderer};
use cubara_voxel::ChunkCoord;
use cubara_world::World;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Square chunk radius of the region to frame in the screenshot.
const REGION_RADIUS: i32 = 6;

pub fn run(path: &str) {
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

    let (features, multi_draw) = gpu_driven_features(&adapter);

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("cubara-screenshot-device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("request device");

    // Scene: a streamed region, same path the live renderer and bench use.
    let world = World::new();
    let mut arena = ChunkArena::from_region(
        &device,
        &queue,
        multi_draw,
        &world,
        ChunkCoord::new(0, 0, 0),
        REGION_RADIUS,
        0..=2,
    );
    let (min, max) = arena
        .bounds()
        .expect("screenshot region produced no geometry");
    let look_target = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let view_radius = (max[0] - min[0]).max(max[2] - min[2]) * 0.75;

    // Camera fixed at a pleasant orbit angle.
    let vp = CameraUniform::view_proj_matrix(
        WIDTH as f32 / HEIGHT as f32,
        6.0,
        look_target,
        view_radius,
    );
    let frustum = Frustum::from_view_proj(vp);
    let draw_count = arena.prepare(&queue, &frustum);

    // The same scene renderer the window and bench use — ARCHITECTURE.md Rule 5.
    // This is the whole point of the rule: when these were separate copies, this
    // path silently stopped rendering what the game renders.
    let mut scene = SceneRenderer::new(&device, &queue, COLOR_FORMAT, WIDTH, HEIGHT);
    scene.set_camera(&queue, vp);

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot-color"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

    // Readback buffer: bytes-per-row must be a multiple of 256.
    let unpadded_bpr = WIDTH * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bpr = unpadded_bpr.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot-readback"),
        size: (padded_bpr * HEIGHT) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("screenshot-encoder"),
    });
    scene.encode_scene(&queue, &mut encoder, &color_view, &arena, draw_count, None);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    // Map and read the buffer back.
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map readback"));
    let _ = device.poll(wgpu::Maintain::Wait);

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in 0..HEIGHT {
        let start = (row * padded_bpr) as usize;
        let end = start + unpadded_bpr as usize;
        pixels.extend_from_slice(&data[start..end]);
    }
    drop(data);
    readback.unmap();

    image::save_buffer(
        path,
        &pixels,
        WIDTH,
        HEIGHT,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write png");
    log::info!("screenshot written to {path} ({WIDTH}x{HEIGHT}, {draw_count} chunks drawn)");
}
