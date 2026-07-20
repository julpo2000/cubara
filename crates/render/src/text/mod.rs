//! Screen-space bitmap text.
//!
//! A minimal text renderer for the debug overlay (and later, menus/options): it
//! bakes the public-domain [`font8x8`](font) into a single-channel atlas texture and
//! draws strings as pixel-positioned textured quads on top of the frame. No external
//! text stack, so it's decoupled from the wgpu version and fits the blocky aesthetic.
//! See issue #49-follow-up (F3 debug screen).

mod font;

use wgpu::util::DeviceExt;

/// Max characters drawn per frame (debug text is tiny; this is generous).
const MAX_CHARS: usize = 4096;
const ATLAS_W: u32 = font::FONT8X8.len() as u32 * font::GLYPH as u32;
const ATLAS_H: u32 = font::GLYPH as u32;

/// One text-quad vertex: screen-pixel position, atlas UV, and RGB colour.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TextVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 3],
}

const TEXT_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x3];

/// Draws bitmap-font strings in screen space. Accumulate lines with
/// [`queue`](Self::queue), then [`flush`](Self::flush) once per frame in a render
/// pass that loads (doesn't clear) the colour target.
pub struct TextRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
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
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text-bind-group"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: screen_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

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
                    blend: None,
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
            screen_buffer,
            vertex_buffer,
            verts: Vec::new(),
        }
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

    fn push_quad(&mut self, x: f32, y: f32, size: f32, u0: f32, u1: f32, color: [f32; 3]) {
        if self.verts.len() + 6 > MAX_CHARS * 6 {
            return;
        }
        let (x1, y1) = (x + size, y + size);
        let v = |px, py, u, vv| TextVertex {
            pos: [px, py],
            uv: [u, vv],
            color,
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
