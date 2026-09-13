//! Occlusion culling on the GPU: what is behind a hill is not drawn.
//!
//! The owner's rule for rendering is that whatever can be seen is really drawn,
//! and whatever cannot be seen need not be. Frustum culling and the world's
//! visibility search (`cubara_world::visibility`) both answer "could this be
//! seen" from geometry alone, so they must keep everything a line of sight
//! *could* reach through caves. From the surface that was 1.25M triangles, while
//! the nodes real rays hit held about 326k. What the screen already shows is
//! the only thing that knows the difference.
//!
//! A frame is drawn in two passes ([`crate::SceneRenderer::encode_scene`]):
//!
//! 1. **The nodes seen last time** are drawn as usual.
//! 2. **A depth pyramid** is built from that depth buffer ([`hiz.wgsl`]), each
//!    texel the farthest depth under it.
//! 3. **Every node in the frame's list is tested** against it ([`cull.wgsl`]).
//!    A node not drawn in step 1 is drawn in the second pass only if its box is
//!    not entirely behind what step 1 drew.
//! 4. The results are read back, a frame or two later, and become the next
//!    frame's step 1 ([`ChunkArena::prepare`]).
//!
//! **Correct whatever the readback says.** A pixel of a node not drawn in step
//! 1 that shows in the final image is nearer than what step 1 left there, so its
//! box is not behind the pyramid and the node is drawn. Stale or missing results
//! only move nodes between the passes, which costs time, never pixels. The
//! golden `occlusion_culling_never_changes_the_image` holds that.
//!
//! A node drawn in step 1 is also tested against a depth buffer it helped write,
//! and its own surface can never hide it: its pixels are no farther than its
//! box's nearest point. So a result of "hidden" for it means something else
//! covered it, and the list converges on what is really seen.
//!
//! [`hiz.wgsl`]: ../shaders/hiz.wgsl
//! [`cull.wgsl`]: ../shaders/cull.wgsl
//! [`ChunkArena::prepare`]: crate::ChunkArena::prepare

use crate::arena::{ChunkArena, Draws};

/// `Params` in `cull.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    screen: [f32; 2],
    levels: u32,
    first_candidate: u32,
    count: u32,
    _pad: [u32; 3],
}

/// The depth pyramid for one screen size.
struct Pyramid {
    #[cfg_attr(not(test), allow(dead_code))]
    texture: wgpu::Texture,
    /// All levels, for the cull.
    view: wgpu::TextureView,
    levels: u32,
    /// Per level: its bind group for building it, and its size in texels.
    steps: Vec<(wgpu::BindGroup, [u32; 2])>,
}

pub(crate) struct Occlusion {
    from_depth: wgpu::ComputePipeline,
    from_finer: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    depth_layout: wgpu::BindGroupLayout,
    finer_layout: wgpu::BindGroupLayout,
    cull_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    /// Nearest and clamped: what `hiz.wgsl`'s gathers assume.
    sampler: wgpu::Sampler,
    pyramid: Pyramid,
    screen: [u32; 2],
    /// The cull's bind group, for the arena whose bounds buffer this is.
    /// Creating one is real work at these frame rates, and nothing in it
    /// changes until the arena or the pyramid does.
    cull_group: Option<(wgpu::Buffer, wgpu::BindGroup)>,
}

/// Level 0 of the pyramid is the screen divided by `2^BASE_SHIFT`.
///
/// Building the pyramid costs time in proportion to the pixels read, and
/// measured it was nearly all of what occlusion culling costs a frame. One
/// texel per 4x4 pixels at the base reads each pixel once, as a half-size base
/// would, but writes a quarter as many texels -- and a box smaller than four
/// pixels is not worth culling.
const BASE_SHIFT: u32 = 2;

/// Levels built above the base, at most. Each is a dispatch, and at these frame
/// rates a dispatch's fixed cost is felt; the levels above this would only
/// serve boxes wider than ~64 pixels, which the test then reads texel by texel
/// at the top level instead -- a few hundred loads for the few nodes that
/// large.
const MAX_LEVELS: u32 = 4;

/// Texels a side of level 0 for a screen `pixels` wide: every pixel in one,
/// the last reaching past the edge if it must.
fn level_zero(pixels: u32) -> u32 {
    pixels.max(1).div_ceil(1 << BASE_SHIFT)
}

/// A shader's source with `BASE_SHIFT` declared in front of it.
fn with_base_shift(source: &str) -> String {
    format!("const BASE_SHIFT: u32 = {BASE_SHIFT}u;\n{source}")
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn buffer_entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    entry: &str,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::ComputePipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl Occlusion {
    pub(crate) fn new(
        device: &wgpu::Device,
        depth: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Self {
        let hiz = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hiz-shader"),
            source: wgpu::ShaderSource::Wgsl(
                with_base_shift(include_str!("shaders/hiz.wgsl")).into(),
            ),
        });
        let cull = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cull-shader"),
            source: wgpu::ShaderSource::Wgsl(
                with_base_shift(include_str!("shaders/cull.wgsl")).into(),
            ),
        });
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hiz-from-depth-layout"),
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Depth),
                storage_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let finer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hiz-from-finer-layout"),
            entries: &[
                texture_entry(1, wgpu::TextureSampleType::Float { filterable: false }),
                storage_entry(2),
            ],
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cull-layout"),
            entries: &[
                buffer_entry(0, wgpu::BufferBindingType::Uniform),
                buffer_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer_entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
                buffer_entry(3, wgpu::BufferBindingType::Storage { read_only: false }),
                texture_entry(4, wgpu::TextureSampleType::Float { filterable: false }),
            ],
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cull-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hiz-depth-sampler"),
            ..Default::default()
        });
        let pyramid = Self::pyramid(
            device,
            &depth_layout,
            &finer_layout,
            &sampler,
            depth,
            width,
            height,
        );
        Self {
            from_depth: compute_pipeline(
                device,
                "hiz-from-depth",
                &hiz,
                "from_depth",
                &depth_layout,
            ),
            from_finer: compute_pipeline(
                device,
                "hiz-from-finer",
                &hiz,
                "from_finer",
                &finer_layout,
            ),
            cull: compute_pipeline(device, "occlusion-cull", &cull, "main", &cull_layout),
            depth_layout,
            finer_layout,
            cull_layout,
            params,
            sampler,
            pyramid,
            screen: [width, height],
            cull_group: None,
        }
    }

    /// Rebuild the pyramid for a new depth buffer.
    pub(crate) fn resize(
        &mut self,
        device: &wgpu::Device,
        depth: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        self.pyramid = Self::pyramid(
            device,
            &self.depth_layout,
            &self.finer_layout,
            &self.sampler,
            depth,
            width,
            height,
        );
        self.screen = [width, height];
        self.cull_group = None;
    }

    fn pyramid(
        device: &wgpu::Device,
        depth_layout: &wgpu::BindGroupLayout,
        finer_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        depth: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Pyramid {
        let size = [level_zero(width), level_zero(height)];
        let levels = (size[0].max(size[1]).ilog2() + 1).min(MAX_LEVELS);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hiz-pyramid"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            // COPY_SRC: so the tests below can read it back.
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let level_view = |level: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("hiz-level"),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let steps = (0..levels)
            .map(|level| {
                let out = level_view(level);
                let group = if level == 0 {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("hiz-from-depth"),
                        layout: depth_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(depth),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::Sampler(sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(&out),
                            },
                        ],
                    })
                } else {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("hiz-from-finer"),
                        layout: finer_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&level_view(
                                    level - 1,
                                )),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(&out),
                            },
                        ],
                    })
                };
                (
                    group,
                    [(size[0] >> level).max(1), (size[1] >> level).max(1)],
                )
            })
            .collect();
        Pyramid {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            texture,
            levels,
            steps,
        }
    }

    /// Build the pyramid from the depth the first pass left, and test this
    /// frame's list against it: candidates get their draws switched on or off,
    /// and every entry's result is written for the arena to read back.
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        arena: &ChunkArena,
        view_proj: glam::Mat4,
        draws: Draws,
    ) {
        if draws.total() == 0 {
            return;
        }
        let params = Params {
            view_proj: view_proj.to_cols_array_2d(),
            screen: [self.screen[0] as f32, self.screen[1] as f32],
            levels: self.pyramid.levels,
            first_candidate: draws.first,
            count: draws.total(),
            _pad: [0; 3],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let (bounds, indirect, seen) = arena.occlusion_buffers();
        if self.cull_group.as_ref().map(|(b, _)| b) != Some(bounds) {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("occlusion-cull"),
                layout: &self.cull_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: bounds.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: indirect.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: seen.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&self.pyramid.view),
                    },
                ],
            });
            self.cull_group = Some((bounds.clone(), group));
        }
        let (_, group) = self.cull_group.as_ref().expect("made just above");

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("occlusion"),
            timestamp_writes: None,
        });
        self.dispatch_pyramid(&mut pass);
        pass.set_pipeline(&self.cull);
        pass.set_bind_group(0, group, &[]);
        pass.dispatch_workgroups(draws.total().div_ceil(64), 1, 1);
    }

    fn dispatch_pyramid(&self, pass: &mut wgpu::ComputePass<'_>) {
        for (level, (step, [w, h])) in self.pyramid.steps.iter().enumerate() {
            pass.set_pipeline(if level == 0 {
                &self.from_depth
            } else {
                &self.from_finer
            });
            pass.set_bind_group(0, step, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_has_a_texel_for_every_pixel() {
        assert_eq!(level_zero(1920), 480);
        assert_eq!(level_zero(1081), 271);
        assert_eq!(level_zero(1), 1);
        assert_eq!(level_zero(4), 1);
        assert_eq!(level_zero(5), 2);
    }

    #[test]
    fn params_match_the_shaders_uniform_layout() {
        // mat4 (64) + vec2 (8) + three u32 (12), rounded up to the struct's
        // 16-byte alignment.
        assert_eq!(std::mem::size_of::<Params>(), 96);
    }
}

/// The shaders against a CPU reference, on a real adapter (skipped without
/// one). The image test in `tests/golden.rs` shows culling never changes a
/// natural scene, but a scene rarely lands on the edges where conservative
/// rounding matters -- an odd pixel left out of a minimum, the last texel of a
/// row, the corner of a box nearest the camera. These put depth and boxes
/// exactly there.
#[cfg(test)]
mod gpu_tests {
    use super::*;
    use crate::arena::NodeId;
    use crate::culling::{Aabb, Frustum};
    use cubara_voxel::{Mesh, Vertex};

    /// Odd on both axes, and not a multiple of the base texel.
    const W: u32 = 37;
    const H: u32 = 23;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
    }

    /// What a test depth buffer holds: every value a whole number of 1/1024ths,
    /// which the depth buffer stores exactly.
    #[derive(Clone, Copy, Debug)]
    enum Fill {
        /// Noise, so a minimum over any set of pixels is likely to differ from
        /// the minimum over a slightly different set.
        Hash,
        /// Nearer towards the bottom right, so the farthest depth in any span is
        /// always its *last* pixel -- the one rounding at an edge leaves out.
        Gradient,
        Constant(f32),
    }

    impl Fill {
        fn at(self, x: u32, y: u32) -> f32 {
            match self {
                Fill::Hash => {
                    let h = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663))
                        .wrapping_mul(2_654_435_761)
                        >> 22;
                    h as f32 / 1024.0
                }
                Fill::Gradient => 1.0 - (x + 2 * y) as f32 / 1024.0,
                Fill::Constant(d) => d,
            }
        }

        /// The same, in WGSL, of a pixel `p`.
        fn wgsl(self) -> String {
            match self {
                Fill::Hash => {
                    "f32((((p.x * 73856093u) ^ (p.y * 19349663u)) * 2654435761u) >> 22u) / 1024.0"
                        .to_string()
                }
                Fill::Gradient => "1.0 - f32(p.x + 2u * p.y) / 1024.0".to_string(),
                Fill::Constant(d) => format!("{d:?}"),
            }
        }
    }

    /// A depth buffer holding `fill`.
    fn depth_buffer(device: &wgpu::Device, queue: &wgpu::Queue, fill: Fill) -> wgpu::TextureView {
        crate::scene::fill_depth_for_test(device, queue, W, H, &fill.wgsl())
    }

    /// One level of the pyramid, read back.
    fn read_level(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        occlusion: &Occlusion,
        level: u32,
    ) -> Vec<f32> {
        let [w, h] = occlusion.pyramid.steps[level as usize].1;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let row = (w * 4).div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-level"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &occlusion.pyramid.texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map level"));
        let _ = device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let start = (y * row) as usize;
            for texel in data[start..start + (w * 4) as usize].as_chunks::<4>().0 {
                out.push(f32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]));
            }
        }
        out
    }

    #[test]
    fn each_texel_holds_the_farthest_depth_of_exactly_the_pixels_it_covers() {
        let Some((device, queue)) = device() else {
            eprintln!("SKIP: no GPU adapter");
            return;
        };
        for fill in [Fill::Hash, Fill::Gradient] {
            let depth = depth_buffer(&device, &queue, fill);
            let occlusion = Occlusion::new(&device, &depth, W, H);
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                occlusion.dispatch_pyramid(&mut pass);
            }
            queue.submit([encoder.finish()]);

            assert!(
                occlusion.pyramid.levels > 2,
                "the test wants rounding at several levels"
            );
            for level in 0..occlusion.pyramid.levels {
                let [w, h] = occlusion.pyramid.steps[level as usize].1;
                let got = read_level(&device, &queue, &occlusion, level);
                let shift = level + BASE_SHIFT;
                // Texel `t` covers from `t << shift` to the next texel's start --
                // or, the last one, to the edge of the screen.
                let span = |t: u32, texels: u32, pixels: u32| {
                    let end = if t + 1 == texels {
                        pixels
                    } else {
                        (t + 1) << shift
                    };
                    (t << shift).min(pixels)..end.min(pixels)
                };
                for ty in 0..h {
                    for tx in 0..w {
                        let mut want = 1.0f32;
                        for y in span(ty, h, H) {
                            for x in span(tx, w, W) {
                                want = want.min(fill.at(x, y));
                            }
                        }
                        assert_eq!(
                            got[(ty * w + tx) as usize],
                            want,
                            "{fill:?}: level {level} texel ({tx}, {ty})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_box_is_hidden_only_when_its_nearest_corner_is_behind_the_depth() {
        let Some((device, queue)) = device() else {
            eprintln!("SKIP: no GPU adapter");
            return;
        };
        // A flat wall of depth everywhere, and boxes around it: some wholly in
        // front, some wholly behind, many straddling it with only a corner or
        // an edge in front.
        let wall = 0.5f32;
        let depth = depth_buffer(&device, &queue, Fill::Constant(wall));
        let mut occlusion = Occlusion::new(&device, &depth, W, H);
        // Looking obliquely, so which corner of a box is nearest depends on all
        // three of its coordinates rather than on one.
        let forward = glam::vec3(1.0, -0.6, -1.0).normalize();
        let right = forward.cross(glam::Vec3::Y).normalize();
        let up = right.cross(forward);
        let view_proj = crate::render::CameraUniform::look_view_proj(
            W as f32 / H as f32,
            glam::Vec3::ZERO,
            forward,
        );
        let clip = |p: glam::Vec3| view_proj * p.extend(1.0);

        let mut arena = crate::arena::ChunkArena::new(&device, false);
        let mesh = Mesh {
            vertices: vec![<Vertex as bytemuck::Zeroable>::zeroed(); 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        let mut seed = 12_345u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut expected = Vec::new();
        while expected.len() < 200 {
            // In view, around the distance where the wall's depth lies.
            let distance = 0.12 + 0.3 * next();
            let centre = forward * distance
                + right * (next() - 0.5) * distance * 0.8
                + up * (next() - 0.5) * distance * 0.5;
            let half = glam::vec3(next(), next(), next()) * 0.15 * distance;
            let (lo, hi) = (centre - half, centre + half);
            let corners: Vec<glam::Vec4> = (0..8)
                .map(|c| {
                    clip(glam::vec3(
                        if c & 1 == 0 { lo.x } else { hi.x },
                        if c & 2 == 0 { lo.y } else { hi.y },
                        if c & 4 == 0 { lo.z } else { hi.z },
                    ))
                })
                .collect();
            let nearest = corners.iter().map(|c| c.z / c.w).fold(0.0f32, f32::max);
            // Leave out boxes the margin itself decides, and any reaching the
            // near plane.
            let near_plane = corners.iter().any(|c| c.w < 0.11);
            if (nearest * 1.0001 - wall).abs() < 1e-3 || nearest >= 1.0 || near_plane {
                continue;
            }
            let id = NodeId {
                level: 0,
                pos: [expected.len() as i32, 0, 0],
            };
            arena.insert(&queue, id, [0.0; 3], 1.0, &mesh, Aabb::new(lo, hi));
            expected.push((id, nearest * 1.0001 >= wall));
        }
        assert!(
            expected.iter().any(|(_, v)| *v) && expected.iter().any(|(_, v)| !*v),
            "the boxes should fall on both sides of the wall"
        );

        let frustum = Frustum::from_view_proj(view_proj);
        let draws = arena.prepare(&queue, &frustum);
        assert_eq!(
            draws.total() as usize,
            expected.len(),
            "every box is in view"
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        occlusion.encode(&device, &queue, &mut encoder, &arena, view_proj, draws);
        arena.encode_readback(&mut encoder);
        queue.submit([encoder.finish()]);
        let _ = device.poll(wgpu::Maintain::Wait);
        // Mapped by one prepare, taken in by the next.
        arena.prepare(&queue, &frustum);
        let _ = device.poll(wgpu::Maintain::Wait);
        arena.prepare(&queue, &frustum);

        let wrong: Vec<_> = expected
            .iter()
            .filter(|(id, visible)| arena.was_seen(*id) != *visible)
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} boxes judged wrong (id, should be visible): {wrong:?}",
            wrong.len(),
            expected.len()
        );
    }
}
