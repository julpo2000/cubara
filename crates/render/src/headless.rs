//! Rendering a frame with no window, to pixels in memory.
//!
//! Used by `--screenshot` and by the golden-image tests. It goes through the same
//! [`SceneRenderer::encode_scene`] the window does (`ARCHITECTURE.md` Rule 5), which
//! is what makes a committed reference image *evidence*: if this rendered anything
//! other than what the game renders, a passing golden test would prove nothing.

use cubara_voxel::ChunkCoord;
use cubara_world::World;

use crate::arena::ChunkArena;
use crate::culling::Frustum;
use crate::render::{gpu_driven_features, CameraUniform};
use crate::scene::SceneRenderer;

/// A rendered frame: tightly-packed RGBA8, `width * height * 4` bytes.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// What to render. Deliberately small and explicit: a golden test's scene must be
/// reproducible from these numbers alone.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    /// Square chunk radius of the region to build.
    pub region_radius: i32,
    /// Virtual time for the orbit camera — fixes the viewpoint deterministically.
    pub orbit_t: f32,
}

impl Default for Shot {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            region_radius: 6,
            orbit_t: 6.0,
        }
    }
}

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Render `world` offscreen and read the pixels back.
///
/// Returns `None` if no GPU adapter is available, so callers can decide whether
/// that is a skip or a failure.
pub fn render(world: &World, shot: Shot) -> Option<Frame> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;

    let (features, multi_draw) = gpu_driven_features(&adapter);
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("cubara-headless-device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()?;

    let Shot {
        width,
        height,
        region_radius,
        orbit_t,
    } = shot;

    let mut arena = ChunkArena::from_region(
        &device,
        &queue,
        multi_draw,
        world,
        ChunkCoord::new(0, 0, 0),
        region_radius,
        0..=2,
    );
    let (min, max) = arena.bounds()?;
    let look_target = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let view_radius = (max[0] - min[0]).max(max[2] - min[2]) * 0.75;

    let vp = CameraUniform::view_proj_matrix(
        width as f32 / height as f32,
        orbit_t,
        look_target,
        view_radius,
    );
    let draw_count = arena.prepare(&queue, &Frustum::from_view_proj(vp));

    let mut scene = SceneRenderer::new(&device, &queue, COLOR_FORMAT, width, height);
    scene.set_camera(&queue, vp);

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless-color"),
        size: wgpu::Extent3d {
            width,
            height,
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

    // Readback rows must be a multiple of 256 bytes.
    let unpadded_bpr = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bpr = unpadded_bpr.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("headless-readback"),
        size: (padded_bpr * height) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("headless-encoder"),
    });
    // No overlay: the debug HUD shows live FPS, which would make any golden
    // reference differ on every run.
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
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map readback"));
    let _ = device.poll(wgpu::Maintain::Wait);

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded_bpr) as usize;
        pixels.extend_from_slice(&data[start..start + unpadded_bpr as usize]);
    }
    drop(data);
    readback.unmap();

    Some(Frame {
        width,
        height,
        pixels,
    })
}

/// How two frames differ.
pub struct Diff {
    /// Fraction of pixels whose per-channel difference exceeds the tolerance.
    pub differing_fraction: f64,
    /// Largest single per-channel difference seen.
    pub max_channel_delta: u8,
}

/// Compare `actual` against `expected`, treating a per-channel difference of up to
/// `tolerance` as equal.
///
/// Tolerance is not laziness: the same scene rasterises slightly differently across
/// backends and driver versions, so an exact match would make this test a
/// false-alarm generator and it would be deleted. What it must catch is a *feature
/// disappearing* — geometry, shading or an overlay — which moves far more than a
/// couple of levels on many pixels.
pub fn compare(actual: &[u8], expected: &[u8], tolerance: u8) -> Diff {
    debug_assert_eq!(actual.len(), expected.len());
    let mut differing = 0u64;
    let mut max_delta = 0u8;
    for (a, e) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
        let mut over = false;
        for c in 0..4 {
            let d = a[c].abs_diff(e[c]);
            max_delta = max_delta.max(d);
            if d > tolerance {
                over = true;
            }
        }
        if over {
            differing += 1;
        }
    }
    let total = (actual.len() / 4) as f64;
    Diff {
        differing_fraction: differing as f64 / total,
        max_channel_delta: max_delta,
    }
}
