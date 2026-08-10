//! Texture array + per-block texture-layer resolution.
//!
//! Builds a `texture_2d_array` from the registry's texture names — 16×16
//! tiles, one layer per name, nearest-filtered (pixel art), matching
//! `docs/PHASE1_ARCHITECTURE.md` §11. The real texture art is block 1.4c;
//! until then each layer is a flat, deterministically-derived placeholder
//! colour, so distinct materials are visibly distinct without depending on
//! art that doesn't exist yet.

use std::collections::HashMap;

use cubara_voxel::BlockRegistry;

const TILE_SIZE: u32 = 16;
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

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

    /// An empty mapping -- every name falls through to layer 0 via
    /// [`layer_of`](Self::layer_of)'s default. For tests that need a
    /// [`MeshAssets`] but don't care about texturing (geometry/pooling logic
    /// tests, which shouldn't need a GPU device just to build a real texture
    /// array via [`build`]).
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            layer_of: HashMap::new(),
        }
    }
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

/// Build the texture array (view + sampler) and the block-id -> layer table
/// from `registry`. Every texture name the registry references gets its own
/// layer, in the same sorted order [`BlockRegistry::texture_names`] returns —
/// deterministic, so the mapping doesn't depend on `HashMap` iteration order
/// anywhere upstream.
pub fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    registry: &BlockRegistry,
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
        // TODO(#55): mip levels arrive with real art in 1.4c -- a flat
        // placeholder colour has nothing to minify into.
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let write_layer = |layer: u32, pixels: &[u8]| {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE * 4),
                rows_per_image: Some(TILE_SIZE),
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    };

    if names.is_empty() {
        write_layer(0, &solid_tile([255, 255, 255]));
    }
    for (layer, &name) in names.iter().enumerate() {
        write_layer(layer as u32, &solid_tile(placeholder_color(name)));
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
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let layer_of: HashMap<String, u32> = names
        .iter()
        .enumerate()
        .map(|(i, &n)| (n.to_string(), i as u32))
        .collect();

    (view, sampler, TextureLayers { layer_of })
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
}
