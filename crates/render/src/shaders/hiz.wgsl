// The depth pyramid occlusion culling tests against (`occlusion.rs`).
//
// Each texel holds the *farthest* depth of the pixels it covers. Depth is
// reversed-Z (`render.rs`, `reverse_z`): near is 1, far and cleared sky are 0,
// so the farthest is the minimum. A box whose nearest point is farther than
// that everywhere it covers cannot show a single pixel.
//
// `BASE_SHIFT` is prepended by `occlusion.rs`. Level 0 is the screen divided by
// `2^BASE_SHIFT`, rounded up, so texel `t` of level `k` covers the pixels
// `t << (k + BASE_SHIFT)` up to the next texel's. A level is half the one below
// it rounded *down* (that is how mip sizes work), so the last texel of a row or
// column also takes in the odd texel left over below it: it covers everything
// to the edge. Whatever lies past the screen covers no pixel and reads as 1.0,
// which a minimum ignores.

@group(0) @binding(0) var depth: texture_depth_2d;
@group(0) @binding(1) var finer: texture_2d<f32>;
@group(0) @binding(2) var level: texture_storage_2d<r32float, write>;
@group(0) @binding(3) var nearest: sampler;

// Level 0, from the depth buffer: `2^BASE_SHIFT` pixels a side per texel, read
// two by two with `textureGather` -- a quarter of the calls one load per pixel
// would take, and building the pyramid is nearly all of occlusion's cost.
//
// A gather at the corner shared by pixels `p` and `p + 1` returns exactly
// those four (the corner is half a pixel from either rounding). Past the
// screen's edge the clamping sampler repeats the edge pixel, which is inside
// the same texel's block, so the minimum is unchanged.
@compute @workgroup_size(8, 8)
fn from_depth(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(level);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let pixels = vec2<f32>(textureDimensions(depth));
    let side = 1u << BASE_SHIFT;
    let start = id.xy << vec2<u32>(BASE_SHIFT);
    var farthest = 1.0;
    for (var dy = 0u; dy < side; dy += 2u) {
        for (var dx = 0u; dx < side; dx += 2u) {
            let corner = vec2<f32>(start + vec2<u32>(dx + 1u, dy + 1u)) / pixels;
            let four = textureGather(depth, nearest, corner);
            farthest = min(farthest, min(min(four.x, four.y), min(four.z, four.w)));
        }
    }
    textureStore(level, id.xy, vec4<f32>(farthest, 0.0, 0.0, 0.0));
}

// Every later level, from the one below it.
@compute @workgroup_size(8, 8)
fn from_finer(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(level);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let below = textureDimensions(finer);
    let start = id.xy * 2u;
    // Two texels a side; the last row and column take the rest too.
    let end = select(start + vec2<u32>(2u), below, id.xy == size - vec2<u32>(1u));
    var farthest = 1.0;
    for (var y = start.y; y < min(end.y, below.y); y++) {
        for (var x = start.x; x < min(end.x, below.x); x++) {
            farthest = min(farthest, textureLoad(finer, vec2<u32>(x, y), 0).r);
        }
    }
    textureStore(level, id.xy, vec4<f32>(farthest, 0.0, 0.0, 0.0));
}
