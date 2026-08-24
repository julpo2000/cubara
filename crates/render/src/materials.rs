//! Texture array + per-block texture-layer resolution.
//!
//! Builds a `texture_2d_array` from the registry's texture names — 16×16
//! tiles, one layer per name, magnified nearest (pixel art) and minified
//! trilinearly through a full mip chain, matching
//! `docs/PHASE1_ARCHITECTURE.md` §11. Each name loads `{textures_dir}/
//! {name}.png` (block 1.4c's original art, `assets/textures/`); a name with
//! no matching file falls back to a flat, deterministically-derived
//! placeholder colour, so a registry entry a texture file hasn't caught up
//! to yet still renders as *something* distinct rather than failing to load.

use std::collections::HashMap;
use std::path::Path;

use cubara_voxel::BlockRegistry;

const TILE_SIZE: u32 = 16;
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Mip levels per tile: 16×16 down to 1×1 inclusive.
const MIP_LEVELS: u32 = TILE_SIZE.ilog2() + 1;

/// Maps a texture *name* (as authored in a material's `faces`) to its
/// texture-array layer. Keyed by name rather than `BlockId` because face
/// selection already happened by the time this is consulted -- the mesher
/// resolves a quad's `(BlockId, Face)` to a texture name via the registry's
/// `Sided`/`All` data (block 1.4b), and this is the last step, turning that
/// name into a GPU layer index.
pub struct TextureLayers {
    layer_of: HashMap<String, u32>,
}

impl TextureLayers {
    pub fn layer_of(&self, name: &str) -> u32 {
        self.layer_of.get(name).copied().unwrap_or(0)
    }

    /// The same deterministic name → layer mapping [`build`] produces,
    /// without needing a GPU device to get it. `build`'s own mapping is a
    /// pure function of `registry.texture_names()`'s sorted order (see its
    /// doc comment) with no GPU calls mixed in, so this and a `build`'s
    /// worth of texture array agree on every layer number by construction —
    /// for a caller that needs to resolve `tex_layer` per quad (mesh a node)
    /// before, or entirely without, building the actual texture array: e.g.
    /// `cubara-app`'s mesh-worker pool (which must stay `wgpu`-free,
    /// `ARCHITECTURE.md` §1), or a headless test meshing ahead of a separate
    /// `render_arena` call that builds its own array later.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        Self {
            layer_of: name_to_layer(registry),
        }
    }
}

/// Every texture name the registry references, mapped to its layer, in the
/// same sorted order [`BlockRegistry::texture_names`] returns — deterministic,
/// so the mapping doesn't depend on `HashMap` iteration order anywhere
/// upstream. The one place this number is decided; both [`build`] and
/// [`TextureLayers::from_registry`] call this rather than each computing
/// their own copy.
fn name_to_layer(registry: &BlockRegistry) -> HashMap<String, u32> {
    registry
        .texture_names()
        .iter()
        .enumerate()
        .map(|(i, &n)| (n.to_string(), i as u32))
        .collect()
}

/// A deterministic placeholder colour for a texture name — stands in for real
/// art (block 1.4c). Hashing the name, rather than a hand-picked table, keeps
/// this generic: it works for any material a data file adds, not just
/// today's three.
fn placeholder_color(name: &str) -> [u8; 3] {
    let mut hash: u32 = 2166136261; // FNV-1a offset basis
    for b in name.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    // Three separated bytes of the hash, each folded into the upper half of
    // 0..255 so tiles read as distinct, reasonably bright colours rather than
    // a muddy dark cluster.
    let r = 128 + (hash & 0x7F) as u8;
    let g = 128 + ((hash >> 8) & 0x7F) as u8;
    let b = 128 + ((hash >> 16) & 0x7F) as u8;
    [r, g, b]
}

fn solid_tile(color: [u8; 3]) -> Vec<u8> {
    let [r, g, b] = color;
    (0..TILE_SIZE * TILE_SIZE)
        .flat_map(|_| [r, g, b, 255])
        .collect()
}

/// The sRGB transfer function, both directions.
///
/// [`TEXTURE_FORMAT`] is `Rgba8UnormSrgb`, so a tile's bytes are *encoded*
/// values, not light. Box-filtering them directly -- the obvious way to build
/// a mip -- averages in the wrong space and comes out visibly darker than the
/// surface it is standing in for, which reads as the terrain dimming with
/// distance. Decode to linear, average there, re-encode.
fn srgb_to_linear(b: u8) -> f32 {
    let c = b as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let v = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// One 2×2 box-filter step: `src` is `size`×`size` RGBA8, the result is half
/// that on each axis. RGB is averaged in linear light (see [`srgb_to_linear`]);
/// alpha is already linear and is averaged as-is.
fn downsample(src: &[u8], size: u32) -> Vec<u8> {
    let half = size / 2;
    let mut out = Vec::with_capacity((half * half * 4) as usize);
    for y in 0..half {
        for x in 0..half {
            let mut rgb = [0f32; 3];
            let mut alpha = 0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = (((y * 2 + dy) * size + (x * 2 + dx)) * 4) as usize;
                    for (c, acc) in rgb.iter_mut().enumerate() {
                        *acc += srgb_to_linear(src[i + c]);
                    }
                    alpha += src[i + 3] as f32;
                }
            }
            out.extend_from_slice(&[
                linear_to_srgb(rgb[0] / 4.0),
                linear_to_srgb(rgb[1] / 4.0),
                linear_to_srgb(rgb[2] / 4.0),
                (alpha / 4.0).round() as u8,
            ]);
        }
    }
    out
}

/// The full mip chain for one tile, level 0 (the source) first.
///
/// Generated on the CPU rather than with a GPU blit pass: the tiles are 16×16
/// and this runs once at startup, so the whole chain for every layer is a few
/// microseconds of work -- and doing it here keeps it a pure function that
/// unit tests can check, instead of something only observable by rendering.
fn mip_chain(base: Vec<u8>) -> Vec<Vec<u8>> {
    let mut levels = vec![base];
    let mut size = TILE_SIZE;
    while size > 1 {
        let next = downsample(levels.last().expect("chain is never empty"), size);
        size /= 2;
        levels.push(next);
    }
    debug_assert_eq!(levels.len() as u32, MIP_LEVELS);
    levels
}

/// `{textures_dir}/{name}.png` as raw RGBA8 tile bytes, or `None` if the file
/// doesn't exist or isn't exactly [`TILE_SIZE`]-square -- either way, the
/// caller falls back to a placeholder rather than failing to start, since a
/// registry entry can outpace the art that names it (a data file adding a
/// material is meant to need no code change, per #54; it just won't have
/// real art until someone draws it).
fn load_tile(textures_dir: &Path, name: &str) -> Option<Vec<u8>> {
    let path = textures_dir.join(format!("{name}.png"));
    let img = match image::open(&path) {
        Ok(img) => img,
        Err(err) => {
            log::warn!("no texture for {name:?} at {}: {err}", path.display());
            return None;
        }
    };
    if img.width() != TILE_SIZE || img.height() != TILE_SIZE {
        log::warn!(
            "{}: {}x{}, expected {TILE_SIZE}x{TILE_SIZE} -- using a placeholder instead",
            path.display(),
            img.width(),
            img.height()
        );
        return None;
    }
    Some(img.to_rgba8().into_raw())
}

/// Build the texture array (view + sampler) and the texture-name -> layer
/// table from `registry`. Every texture name the registry references gets
/// its own layer, in the same sorted order [`BlockRegistry::texture_names`]
/// returns — deterministic, so the mapping doesn't depend on `HashMap`
/// iteration order anywhere upstream. `textures_dir` is where each name's
/// `.png` lives (`assets/textures/` for the real app; see [`load_tile`]).
pub fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    registry: &BlockRegistry,
    textures_dir: &Path,
) -> (wgpu::TextureView, wgpu::Sampler, TextureLayers) {
    let names = registry.texture_names();
    // A texture array needs at least one layer even in the (never-happens-in-
    // phase-1) case of a registry with no materials at all.
    let layer_count = names.len().max(1) as u32;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("block-texture-array"),
        size: wgpu::Extent3d {
            width: TILE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: layer_count,
        },
        // A full mip chain. The comment that stood here said phase 1's camera
        // never gets far enough from a block for minification aliasing to be
        // worth spending on -- true when it was written against radius 12, and
        // no longer true since block 1.10 made the horizon 64 chunks (1,024
        // blocks) away. At that range a 16x16 tile covers well under a pixel,
        // and sampling it with no mip chain is textbook minification aliasing:
        // it shimmers under movement, which a screenshot does not show.
        mip_level_count: MIP_LEVELS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // Uploads level 0 *and* every mip below it, so no caller can add a layer
    // and leave its chain undefined.
    let write_layer = |layer: u32, pixels: Vec<u8>| {
        for (level, data) in mip_chain(pixels).iter().enumerate() {
            let size = TILE_SIZE >> level;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 4),
                    rows_per_image: Some(size),
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
        }
    };

    if names.is_empty() {
        write_layer(0, solid_tile([255, 255, 255]));
    }
    for (layer, &name) in names.iter().enumerate() {
        let pixels =
            load_tile(textures_dir, name).unwrap_or_else(|| solid_tile(placeholder_color(name)));
        write_layer(layer as u32, pixels);
    }

    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("block-texture-sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        // Nearest magnification keeps the pixel-art look up close; linear
        // min + mipmap filtering makes that trilinear at distance, which is
        // what the mip chain above is for. These two lines were already here
        // before the chain existed, where they were inert -- filtering
        // settings that looked configured and did nothing.
        //
        // No `anisotropy_clamp`: wgpu requires min, mag *and* mipmap filters
        // all be Linear for it, and Nearest magnification is a deliberate
        // look, not an oversight.
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    (view, sampler, TextureLayers::from_registry(registry))
}

/// The bind group layout for the texture array + sampler (`@group(2)` in
/// `mesh.wgsl`), fragment-stage only.
pub fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("texture-array-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

pub fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("texture-array-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Registry + the render-specific texture-layer mapping built from it,
/// bundled since a mesh job always needs both together and this is what
/// travels as one `Arc` to every worker thread (`crate::mesher::MeshPool`).
///
/// There is deliberately no `fn context(&self) -> MeshContext` here: a
/// `MeshContext` borrows a `Fn(&str) -> u32` by reference, and that closure
/// has to live at the call site (a method can't hand back a reference to a
/// closure it only just created and is about to drop). Build it where it's
/// used:
/// ```ignore
/// let layer_of = |name: &str| assets.layers.layer_of(name);
/// let ctx = MeshContext { registry: &assets.registry, layer_of: &layer_of };
/// ```
pub struct MeshAssets {
    pub registry: BlockRegistry,
    pub layers: TextureLayers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_color_is_deterministic_and_distinguishes_names() {
        assert_eq!(placeholder_color("stone"), placeholder_color("stone"));
        assert_ne!(placeholder_color("stone"), placeholder_color("soil"));
    }

    #[test]
    fn mip_chain_halves_down_to_one_pixel() {
        let chain = mip_chain(solid_tile([10, 20, 30]));
        assert_eq!(chain.len() as u32, MIP_LEVELS);
        for (level, data) in chain.iter().enumerate() {
            let size = TILE_SIZE >> level;
            assert_eq!(
                data.len() as u32,
                size * size * 4,
                "level {level} should be {size}x{size} RGBA"
            );
        }
        assert_eq!(
            chain.last().unwrap().len(),
            4,
            "last level is a single texel"
        );
    }

    #[test]
    fn a_flat_tile_keeps_its_colour_all_the_way_down() {
        // Nothing to average away, so every level must be the same colour --
        // any drift here is the filter losing energy, not detail.
        let chain = mip_chain(solid_tile([200, 100, 50]));
        for (level, data) in chain.iter().enumerate() {
            assert_eq!(
                &data[..4],
                &[200, 100, 50, 255],
                "level {level} shifted colour"
            );
        }
    }

    #[test]
    fn downsampling_averages_in_linear_light_not_srgb() {
        // The whole reason `downsample` decodes and re-encodes. A 2x2 of two
        // black and two white texels is half the light, and half of full
        // brightness encodes to sRGB ~188 -- not 127. Averaging the *encoded*
        // bytes directly gives 127, a mip visibly darker than the surface it
        // stands in for, which reads in-game as terrain dimming with distance.
        let black_white = [
            0, 0, 0, 255, 255, 255, 255, 255, // row 0
            255, 255, 255, 255, 0, 0, 0, 255, // row 1
        ];
        let out = downsample(&black_white, 2);
        assert_eq!(out.len(), 4, "2x2 downsamples to a single texel");
        let grey = out[0];
        assert!(
            (186..=190).contains(&grey),
            "expected the linear-light average (~188), got {grey}              ({} is what averaging sRGB bytes gives)",
            127
        );
        assert_eq!(out[3], 255, "alpha is averaged as-is and stays opaque");
    }

    #[test]
    fn srgb_round_trips_through_linear() {
        for b in [0u8, 1, 37, 128, 200, 254, 255] {
            assert_eq!(
                linear_to_srgb(srgb_to_linear(b)),
                b,
                "byte {b} did not survive"
            );
        }
    }
}
