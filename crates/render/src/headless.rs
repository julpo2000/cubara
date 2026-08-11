//! Rendering a frame with no window, to pixels in memory.
//!
//! Used by `--screenshot` and by the golden-image tests. It goes through the same
//! [`SceneRenderer::encode_scene`] the window does (`ARCHITECTURE.md` Rule 5), which
//! is what makes a committed reference image *evidence*: if this rendered anything
//! other than what the game renders, a passing golden test would prove nothing.

use cubara_voxel::{Chunk, ChunkCoord, MeshContext};

use crate::arena::{ChunkArena, MeshedNode, NodeId};
use crate::culling::Frustum;
use crate::render::{gpu_driven_features, load_mesh_assets, CameraUniform};
use crate::scene::{SceneFrame, SceneRenderer};

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
    /// Ignored when `camera` overrides the viewpoint.
    pub orbit_t: f32,
    /// An explicit `(eye, look_dir)` camera, overriding the default auto-framed
    /// orbit. `None` (the default) keeps the orbit: frame the whole scene's AABB
    /// and view it from `orbit_t`'s angle, always somewhat above it (`eye.y =
    /// center.y + radius * 0.45` — see `CameraUniform::view_proj_matrix`). That
    /// fixed elevation can't produce a grazing, near-ground viewpoint at any
    /// scale (shrinking the region shrinks the eye's height by the same factor
    /// as its distance, so the angle never changes) — which a shot that needs to
    /// look *into* something at ground level, like a cave mouth, needs.
    pub camera: Option<(glam::Vec3, glam::Vec3)>,
    /// A block to draw the selected-block outline around (issue #52), or
    /// `None` for no outline. In the real game this is `cubara_sim::Sim::target`
    /// (the sim's own raycast); a golden test sets it explicitly to whatever
    /// block its `camera` is known to be looking at.
    pub highlighted_block: Option<[i32; 3]>,
}

impl Default for Shot {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            region_radius: 6,
            orbit_t: 6.0,
            camera: None,
            highlighted_block: None,
        }
    }
}

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Render an already-meshed scene offscreen and read the pixels back.
///
/// This crate never touches a `World` (`ARCHITECTURE.md` §1): a caller that
/// wants to render a real world region meshes it first --
/// `cubara_world::mesh::mesh_region` for a whole region synchronously (what
/// `--screenshot` and the golden-image tests do), or the live renderer's own
/// `cubara_world::mesh::MeshPool` for the incremental case -- and hands the
/// result here. Returns `None` if no GPU adapter is available, so callers can
/// decide whether that is a skip or a failure.
pub fn render(meshed: impl IntoIterator<Item = MeshedNode>, shot: Shot) -> Option<Frame> {
    render_arena(shot, |device, queue, multi_draw, _ctx| {
        ChunkArena::from_meshed(device, queue, multi_draw, meshed)
    })
}

/// Render explicit `(coord, chunk)` pairs offscreen -- for scenes worldgen
/// can't produce yet, e.g. several distinct block ids side by side. Otherwise
/// identical to [`render`]: same device setup, same
/// [`SceneRenderer::encode_scene`] path (`ARCHITECTURE.md` Rule 5). Each
/// chunk is its own level-0 node (`scale` 1.0, one lattice cell = one world
/// block), placed at its own `ChunkCoord::world_offset`.
pub fn render_chunks(chunks: &[(ChunkCoord, Chunk)], shot: Shot) -> Option<Frame> {
    render_arena(shot, |device, queue, multi_draw, ctx| {
        let mut arena = ChunkArena::new(device, multi_draw);
        for (coord, chunk) in chunks {
            let id = NodeId {
                level: 0,
                pos: [coord.x, coord.y, coord.z],
            };
            arena.upload_node(queue, id, coord.world_offset(), 1.0, chunk, ctx);
        }
        arena
    })
}

/// Shared device setup, camera framing and readback for [`render`] and
/// [`render_chunks`]; `build_arena` is the one part that differs between them.
fn render_arena(
    shot: Shot,
    build_arena: impl FnOnce(&wgpu::Device, &wgpu::Queue, bool, &MeshContext) -> ChunkArena,
) -> Option<Frame> {
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
        region_radius: _,
        orbit_t,
        camera,
        highlighted_block,
    } = shot;

    let (mesh_assets, tex_view, tex_sampler) = load_mesh_assets(&device, &queue);
    let layer_of = |name: &str| mesh_assets.layers.layer_of(name);
    let ctx = MeshContext {
        registry: &mesh_assets.registry,
        layer_of: &layer_of,
    };
    let mut arena = build_arena(&device, &queue, multi_draw, &ctx);
    let (min, max) = arena.bounds()?;

    let vp = match camera {
        Some((eye, look_dir)) => {
            CameraUniform::look_view_proj(width as f32 / height as f32, eye, look_dir)
        }
        None => {
            let look_target = [
                (min[0] + max[0]) * 0.5,
                (min[1] + max[1]) * 0.5,
                (min[2] + max[2]) * 0.5,
            ];
            let view_radius = (max[0] - min[0]).max(max[2] - min[2]) * 0.75;
            CameraUniform::view_proj_matrix(
                width as f32 / height as f32,
                orbit_t,
                look_target,
                view_radius,
            )
        }
    };
    let draw_count = arena.prepare(&queue, &Frustum::from_view_proj(vp));

    let mut scene = SceneRenderer::new(
        &device,
        &queue,
        COLOR_FORMAT,
        width,
        height,
        &tex_view,
        &tex_sampler,
    );
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
    // reference differ on every run. `highlighted_block` does go through --
    // a golden test needs to be able to show the outline.
    scene.encode_scene(
        &queue,
        &mut encoder,
        &color_view,
        SceneFrame {
            arena: &arena,
            draw_count,
            selected_block: highlighted_block,
            overlay: None,
        },
    );
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
