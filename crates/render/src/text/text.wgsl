// Screen-space bitmap text: pixel-positioned quads sampling a font atlas.

struct Screen {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
// Item icons, 16x16 RGBA cells side by side (see `icon_atlas`).
@group(0) @binding(3) var icons: texture_2d<f32>;

struct VsIn {
    @location(0) pos: vec2<f32>,   // screen pixels, origin top-left
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: f32,        // 0 font atlas, 1 icon atlas
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) mode: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    // Pixels (y down) → clip space (y up).
    let ndc = vec2<f32>(
        in.pos.x / screen.size.x * 2.0 - 1.0,
        1.0 - in.pos.y / screen.size.y * 2.0,
    );
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.mode = in.mode;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Both sampled unconditionally, at level 0: neither atlas has mips, and
    // sampling inside the branch would put a texture read in non-uniform
    // control flow, which WGSL rejects for derivative-based sampling.
    let glyph = textureSampleLevel(atlas, atlas_sampler, in.uv, 0.0).r;
    let icon = textureSampleLevel(icons, atlas_sampler, in.uv, 0.0);
    if in.mode > 0.5 {
        // Pixel art with hard edges: a pixel is in the icon or it is not.
        if icon.a < 0.5 {
            discard;
        }
        return vec4<f32>(icon.rgb * in.color.rgb, in.color.a);
    }
    // The atlas is 1 where a glyph pixel is set. Crisp (no blending): keep set
    // pixels, drop the rest.
    if glyph < 0.5 {
        discard;
    }
    return in.color;
}
