// Occlusion test for every node in this frame's draw list (`occlusion.rs`).
//
// A node is hidden when the nearest point of its bounding box is farther than
// the farthest depth already drawn anywhere its box covers on screen. Its own
// triangles lie inside that box, so none of them could pass the depth test.
// Whenever that cannot be decided safely -- a box reaching behind the camera or
// through the near plane -- the node counts as visible.

struct Params {
    view_proj: mat4x4<f32>,
    // The screen, in pixels.
    screen: vec2<f32>,
    // Levels in the depth pyramid.
    levels: u32,
    // Entries before this were drawn already, in the first pass; from here on
    // they are candidates, and this decides whether the second pass draws them.
    first_candidate: u32,
    count: u32,
};

struct Bounds {
    lo: vec3<f32>,
    _pad0: f32,
    hi: vec3<f32>,
    _pad1: f32,
};

// `wgpu`'s `DrawIndexedIndirectArgs`, the indirect draw list itself.
struct Draw {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> bounds: array<Bounds>;
@group(0) @binding(2) var<storage, read_write> draws: array<Draw>;
@group(0) @binding(3) var<storage, read_write> seen: array<u32>;
@group(0) @binding(4) var pyramid: texture_2d<f32>;

fn hidden(b: Bounds) -> bool {
    var lo = vec2<f32>(1.0e30);
    var hi = vec2<f32>(-1.0e30);
    var nearest = 0.0;
    for (var c = 0u; c < 8u; c++) {
        let corner = vec3<f32>(
            select(b.lo.x, b.hi.x, (c & 1u) != 0u),
            select(b.lo.y, b.hi.y, (c & 2u) != 0u),
            select(b.lo.z, b.hi.z, (c & 4u) != 0u),
        );
        let clip = params.view_proj * vec4<f32>(corner, 1.0);
        if (clip.w <= 1.0e-5) {
            return false;
        }
        let ndc = clip.xyz / clip.w;
        lo = min(lo, ndc.xy);
        hi = max(hi, ndc.xy);
        nearest = max(nearest, ndc.z);
    }
    if (nearest >= 1.0) {
        return false;
    }
    // Off screen entirely: the frustum test let it through, so do not argue.
    if (hi.x < -1.0 || lo.x > 1.0 || hi.y < -1.0 || lo.y > 1.0) {
        return false;
    }

    // The pixels the box covers, inclusive. Row 0 is the top of the screen.
    let size = params.screen;
    let x0 = u32(clamp((lo.x * 0.5 + 0.5) * size.x, 0.0, size.x - 1.0));
    let x1 = u32(clamp((hi.x * 0.5 + 0.5) * size.x, 0.0, size.x - 1.0));
    let y0 = u32(clamp((0.5 - hi.y * 0.5) * size.y, 0.0, size.y - 1.0));
    let y1 = u32(clamp((0.5 - lo.y * 0.5) * size.y, 0.0, size.y - 1.0));

    // The finest level at which those pixels fall in at most two texels a side.
    // A pixel's texel is its position shifted down, or the last one, which
    // covers everything to the edge (`hiz.wgsl`).
    var level = 0u;
    loop {
        let shift = level + BASE_SHIFT;
        let fits = (x1 >> shift) - (x0 >> shift) <= 1u && (y1 >> shift) - (y0 >> shift) <= 1u;
        if (fits || level + 1u >= params.levels) {
            break;
        }
        level++;
    }
    let shift = level + BASE_SHIFT;
    let texels = textureDimensions(pyramid, level);
    var farthest = 1.0;
    let first = min(vec2<u32>(x0, y0) >> vec2<u32>(shift), texels - vec2<u32>(1u));
    let last = min(vec2<u32>(x1, y1) >> vec2<u32>(shift), texels - vec2<u32>(1u));
    for (var y = first.y; y <= last.y; y++) {
        for (var x = first.x; x <= last.x; x++) {
            farthest = min(farthest, textureLoad(pyramid, vec2<u32>(x, y), i32(level)).r);
        }
    }
    // A hair of margin, so a box touching the drawn surface is never called
    // hidden over the rounding of two different transforms of the same point.
    return nearest * 1.0001 < farthest;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.count) {
        return;
    }
    let visible = !hidden(bounds[i]);
    seen[i] = select(0u, 1u, visible);
    if (i >= params.first_candidate) {
        draws[i].instance_count = select(0u, 1u, visible);
    }
}
