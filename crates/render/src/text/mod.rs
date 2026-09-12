//! Screen-space bitmap text.
//!
//! A minimal text renderer for the debug overlay (and later, menus/options): it
//! bakes the public-domain [`font8x8`](font) into a single-channel atlas texture and
//! draws strings as pixel-positioned textured quads on top of the frame. No external
//! text stack, so it's decoupled from the wgpu version and fits the blocky aesthetic.
//! See issue #49-follow-up (F3 debug screen).

pub(crate) mod font;

use wgpu::util::DeviceExt;

/// Max characters drawn per frame (debug text is tiny; this is generous).
const MAX_CHARS: usize = 4096;
/// One cell per glyph, plus one **solid** cell at the end. That extra cell is
/// what lets this same pipeline draw filled rectangles (see
/// [`TextRenderer::queue_rect`]) instead of needing a second screen-space
/// pipeline for the HUD -- one 2D path, per `ARCHITECTURE.md` Rule 5.
const ATLAS_W: u32 = (font::FONT8X8.len() as u32 + 1) * font::GLYPH as u32;

/// Index of the solid cell: one past the last glyph.
const SOLID_CELL: usize = font::FONT8X8.len();
const ATLAS_H: u32 = font::GLYPH as u32;

/// One text-quad vertex: screen-pixel position, atlas UV, and RGB colour.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TextVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    /// Linear RGB **and alpha**. Everything except the inventory screen's
    /// backdrop draws at alpha 1, where blending is a no-op -- the alpha exists
    /// so a modal screen can dim the world rather than replace it, which
    /// without blending is what an opaque rectangle does.
    color: [f32; 4],
    /// Which atlas `uv` points into: 0 the font (and its solid cell), 1 the
    /// item icons. A float rather than a second pipeline, so an icon is one
    /// more quad in the same draw as the slot behind it (Rule 5).
    mode: f32,
}

const TEXT_ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
    0 => Float32x2,
    1 => Float32x2,
    2 => Float32x4,
    3 => Float32
];

/// Edge of one icon cell in pixels -- the 16x16 of the block and item art.
const ICON: u32 = 16;

/// Draws bitmap-font strings in screen space. Accumulate lines with
/// [`queue`](Self::queue), then [`flush`](Self::flush) once per frame in a render
/// pass that loads (doesn't clear) the colour target.
pub struct TextRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    /// Kept so [`set_icons`](Self::set_icons) can rebuild the bind group
    /// around a new icon atlas.
    bgl: wgpu::BindGroupLayout,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// How many icon cells the current icon atlas holds.
    icon_count: u32,
    screen_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    verts: Vec<TextVertex>,
}

impl TextRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // Bake the font bitmap into an R8 atlas: 255 where a glyph pixel is set.
        let mut pixels = vec![0u8; (ATLAS_W * ATLAS_H) as usize];
        for (g, glyph) in font::FONT8X8.iter().enumerate() {
            for (y, &row) in glyph.iter().enumerate() {
                for x in 0..font::GLYPH {
                    if (row >> x) & 1 == 1 {
                        // LSB of each row is the leftmost pixel.
                        let px = g * font::GLYPH + x;
                        pixels[y * ATLAS_W as usize + px] = 255;
                    }
                }
            }
        }
        // The solid cell: every pixel set, so a quad sampling it is a filled
        // rectangle of whatever colour the vertex carries.
        for y in 0..font::GLYPH {
            for x in 0..font::GLYPH {
                let px = SOLID_CELL * font::GLYPH + x;
                pixels[y * ATLAS_W as usize + px] = 255;
            }
        }
        let atlas = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("text-atlas"),
                size: wgpu::Extent3d {
                    width: ATLAS_W,
                    height: ATLAS_H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &pixels,
        );
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("text-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let screen_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text-screen"),
            size: 16, // vec2 size + padding
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        // No icons until someone sets them: one transparent cell, so the
        // binding is always valid.
        let icons_view = icon_atlas(device, queue, &[]);
        let bind_group = text_bind_group(
            device,
            &bgl,
            &screen_buffer,
            &atlas_view,
            &sampler,
            &icons_view,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TextVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &TEXT_ATTRS,
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Straight alpha. Every existing caller draws at alpha 1,
                    // for which this is the identity -- so the hotbar and the
                    // debug text are byte-identical with it on.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text-vertices"),
            size: (MAX_CHARS * 6 * std::mem::size_of::<TextVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group,
            bgl,
            atlas_view,
            sampler,
            icon_count: 0,
            screen_buffer,
            vertex_buffer,
            verts: Vec::new(),
        }
    }

    /// Replace the item icons: one 16x16 RGBA tile per entry, `None` for an
    /// item with no art. Entry `i` is what [`queue_icon`](Self::queue_icon)
    /// draws for icon `i`.
    ///
    /// Called when the icons are known rather than at construction, because
    /// this crate does not know what items exist -- the app does (Rule 3).
    pub fn set_icons(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        icons: &[Option<Vec<u8>>],
    ) {
        let icons_view = icon_atlas(device, queue, icons);
        self.bind_group = text_bind_group(
            device,
            &self.bgl,
            &self.screen_buffer,
            &self.atlas_view,
            &self.sampler,
            &icons_view,
        );
        self.icon_count = icons.len() as u32;
    }

    /// Draw icon `index` stretched over a rectangle. Returns `false`, drawing
    /// nothing, for an index the atlas does not have -- so the caller can fall
    /// back to something visible instead.
    pub fn queue_icon(&mut self, index: u32, x: f32, y: f32, w: f32, h: f32) -> bool {
        if index >= self.icon_count {
            return false;
        }
        let cells = self.icon_count as f32;
        let u0 = index as f32 / cells;
        let u1 = (index + 1) as f32 / cells;
        self.push_rect_mode(x, y, w, h, u0, u1, [1.0, 1.0, 1.0, 1.0], 1.0);
        true
    }

    /// Queue a line of text with its top-left at (`x`, `y`) pixels, each glyph
    /// `scale`× the 8px font size, in `color` (linear RGB). Newlines advance a line.
    pub fn queue(&mut self, text: &str, x: f32, y: f32, scale: f32, color: [f32; 3]) {
        let g = font::GLYPH as f32 * scale;
        let (mut cx, mut cy) = (x, y);
        for ch in text.chars() {
            if ch == '\n' {
                cx = x;
                cy += g;
                continue;
            }
            let byte = ch as u32;
            if byte < font::FIRST as u32 || byte > font::LAST as u32 {
                cx += g; // unknown glyph → blank space
                continue;
            }
            let idx = (byte - font::FIRST as u32) as f32;
            let u0 = idx * font::GLYPH as f32 / ATLAS_W as f32;
            let u1 = (idx + 1.0) * font::GLYPH as f32 / ATLAS_W as f32;
            self.push_quad(cx, cy, g, u0, u1, color);
            cx += g;
        }
    }

    /// A filled rectangle in screen pixels. Uses the atlas's solid cell, so it
    /// shares the glyph pipeline, its vertex buffer and its draw call -- the
    /// HUD costs no extra pass.
    pub fn queue_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 3]) {
        self.queue_rect_alpha(x, y, w, h, color, 1.0);
    }

    /// A filled rectangle with an explicit alpha -- what lets the inventory
    /// screen dim the world instead of hiding it.
    pub fn queue_rect_alpha(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 3],
        alpha: f32,
    ) {
        let cell = SOLID_CELL as f32 * font::GLYPH as f32;
        // Inset half a texel on each side so bilinear-free sampling cannot
        // catch the neighbouring glyph's edge column.
        let u0 = (cell + 0.5) / ATLAS_W as f32;
        let u1 = (cell + font::GLYPH as f32 - 0.5) / ATLAS_W as f32;
        self.push_rect(x, y, w, h, u0, u1, [color[0], color[1], color[2], alpha]);
    }

    fn push_quad(&mut self, x: f32, y: f32, size: f32, u0: f32, u1: f32, color: [f32; 3]) {
        self.push_rect(
            x,
            y,
            size,
            size,
            u0,
            u1,
            [color[0], color[1], color[2], 1.0],
        );
    }

    /// The one place a quad is built. Eight arguments is over clippy's
    /// threshold and grouping them into a struct would only move the same
    /// eight values one line up -- this is a private leaf that both public
    /// entry points funnel into, which is the shape Rule 5 wants.
    #[allow(clippy::too_many_arguments)]
    fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, u0: f32, u1: f32, color: [f32; 4]) {
        self.push_rect_mode(x, y, w, h, u0, u1, color, 0.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_rect_mode(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        u0: f32,
        u1: f32,
        color: [f32; 4],
        mode: f32,
    ) {
        if self.verts.len() + 6 > MAX_CHARS * 6 {
            return;
        }
        let (x1, y1) = (x + w, y + h);
        let v = |px, py, u, vv| TextVertex {
            pos: [px, py],
            uv: [u, vv],
            color,
            mode,
        };
        let tl = v(x, y, u0, 0.0);
        let tr = v(x1, y, u1, 0.0);
        let br = v(x1, y1, u1, 1.0);
        let bl = v(x, y1, u0, 1.0);
        self.verts.extend_from_slice(&[tl, tr, br, tl, br, bl]);
    }

    /// Draw and clear everything queued this frame. Call inside a render pass whose
    /// colour target is the frame (loaded, not cleared) with no depth attachment.
    pub fn flush(
        &mut self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        screen_w: f32,
        screen_h: f32,
    ) {
        if self.verts.is_empty() {
            return;
        }
        queue.write_buffer(
            &self.screen_buffer,
            0,
            bytemuck::cast_slice(&[screen_w, screen_h, 0.0, 0.0]),
        );
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.verts));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..self.verts.len() as u32, 0..1);
        self.verts.clear();
    }
}

/// Pack `icons` side by side into one RGBA texture, one 16x16 cell each. A
/// `None` entry, and the empty list, become transparent cells: every icon
/// index stays valid, and a missing icon draws nothing rather than garbage.
fn icon_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    icons: &[Option<Vec<u8>>],
) -> wgpu::TextureView {
    let cells = icons.len().max(1) as u32;
    let mut pixels = vec![0u8; (cells * ICON * ICON * 4) as usize];
    for (i, icon) in icons.iter().enumerate() {
        let Some(tile) = icon else { continue };
        if tile.len() != (ICON * ICON * 4) as usize {
            log::warn!("icon {i} is not a 16x16 RGBA tile; leaving it blank");
            continue;
        }
        for y in 0..ICON as usize {
            for x in 0..ICON as usize {
                let src = (y * ICON as usize + x) * 4;
                let dst = (y * (cells * ICON) as usize + i * ICON as usize + x) * 4;
                pixels[dst..dst + 4].copy_from_slice(&tile[src..src + 4]);
            }
        }
    }
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("icon-atlas"),
            size: wgpu::Extent3d {
                width: cells * ICON,
                height: ICON,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB like the block textures, so the same art is the same colour
            // in the world and in the hand.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &pixels,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn text_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    screen: &wgpu::Buffer,
    atlas: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    icons: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("text-bind-group"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: screen.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(atlas),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(icons),
            },
        ],
    })
}
