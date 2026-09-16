// Packed node-local geometry, textured. Vertices carry a lattice position
// local to their node plus baked AO, a texture-array layer, a face
// direction, this corner's own tile coordinate, and the node index itself --
// see docs/PHASE1_ARCHITECTURE.md §5.2 for the bit layout. World placement is
// a per-node origin add (scaled by the node's own lattice step) here, not a
// CPU-side translate.

// Everything about the frame beyond the geometry: camera, sun, ambient, fog.
// One binding shared with `figure.wgsl` (`render.rs`'s `FrameUniform`/
// `Lighting`), so the two can't disagree about where the sun is the way
// they used to.
struct Frame {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    sun_dir: vec4<f32>,
    // .w is the diffuse term's weight.
    sun_color: vec4<f32>,
    // .x ambient (ground-facing), .y ambient (sky-facing), .z AO floor.
    ambient: vec4<f32>,
    fog_color: vec4<f32>,
    // .x fog start, .y fog end, .z time of day.
    fog: vec4<f32>,
    // .x/.y are the (A, B) in `view_depth = A / (clip_pos.z + B)`, which
    // inverts `reverse_z`'s depth -- see the fog comment below for the
    // derivation and why this reads `clip_pos.z`, not a varying.
    depth: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;

// One world-space origin per resident node, indexed by the node_index packed
// into word 2 of each vertex (see crates/render/src/arena.rs). xyz is the
// node's world-space min corner; w is its scale -- world units per lattice
// step (1.0 at level 0, 2^level above it, since a node's mesh is always a
// fixed 16^3 lattice regardless of how many chunks it spans, §6.2).
//
// Not @builtin(instance_index): block 1.4a tried that (first_instance set
// per indirect draw) and found it unreliable in both directions across real
// CI backends -- broken with multi_draw_indexed_indirect on one software
// DX12 adapter, broken in the plain draw_indexed fallback on a virtualized
// Metal adapter, no combination safe everywhere. §5.3 has the full story.
@group(1) @binding(0) var<storage, read> node_origins: array<vec4<f32>>;

@group(2) @binding(0) var block_textures: texture_2d_array<f32>;
@group(2) @binding(1) var block_sampler: sampler;

struct VsIn {
    @location(0) packed0: u32,
    @location(1) packed1: u32,
    @location(2) packed2: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    // The resolved unit normal, not the packed `face` index -- looked up
    // from `FACE_NORMALS` once per *vertex* here instead of once per
    // *fragment* in fs_main. Measured on both machines (an A/B/C throwaway:
    // hardcoding the fragment-stage index away, isolated from everything
    // else): the dynamic array index into a const array is a real fragment
    // cost, worth more than the flat u32 it used to be a "free" alternative
    // to -- greedy-meshed faces never bend across a triangle, so there is
    // nothing for the rasterizer to interpolate either way, and moving the
    // index to the vertex stage amortizes it over one lookup per vertex
    // instead of one per covered pixel.
    @location(0) @interpolate(flat) normal: vec3<f32>,
    @location(1) ao: f32,
    @location(2) uv: vec2<f32>,
    @location(3) @interpolate(flat) layer: u32,
};

// Indexed by the packed `face` field (3 bits) -- always one of the six axis
// directions, since greedy-meshed voxel faces never point anywhere else.
const FACE_NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, -1.0),
);

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // word 0: x:10 y:10 z:10 ao:2
    let x = f32(in.packed0 & 0x3FFu);
    let y = f32((in.packed0 >> 10u) & 0x3FFu);
    let z = f32((in.packed0 >> 20u) & 0x3FFu);
    let ao_raw = (in.packed0 >> 30u) & 0x3u;

    // word 1: tex_layer:12 face:3 u:8 v:8 (1 spare)
    let tex_layer = in.packed1 & 0xFFFu;
    let face = (in.packed1 >> 12u) & 0x7u;
    let u = f32((in.packed1 >> 15u) & 0xFFu);
    let v = f32((in.packed1 >> 23u) & 0xFFu);

    // word 2: node_index:16 (16 spare)
    let node_index = in.packed2 & 0xFFFFu;

    let local_pos = vec3<f32>(x, y, z);
    let node_origin = node_origins[node_index];
    let world_pos = node_origin.xyz + local_pos * node_origin.w;

    var out: VsOut;
    out.clip_pos = frame.view_proj * vec4<f32>(world_pos, 1.0);
    out.normal = FACE_NORMALS[face];
    out.ao = f32(ao_raw) / 3.0;
    out.uv = vec2<f32>(u, v);
    out.layer = tex_layer;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = in.normal;

    // Directional sun: clear sun-side / shadow-side split. `sun_dir` arrives
    // already normalized (`Lighting`'s doc comment); the shader does not
    // renormalize it.
    let diffuse = max(dot(n, frame.sun_dir.xyz), 0.0) * frame.sun_color.w;

    // Hemispheric ambient: a touch brighter facing up (sky) than down (ground).
    let ambient = mix(frame.ambient.x, frame.ambient.y, n.y * 0.5 + 0.5);

    // Baked ambient occlusion darkens crevices; keep a floor so nothing is pure black.
    let ao = mix(frame.ambient.z, 1.0, in.ao);

    let tex = textureSample(block_textures, block_sampler, in.uv, in.layer);
    let lit = tex.rgb * frame.sun_color.rgb * (ambient + diffuse) * ao;

    // Depth fog, fading toward `fog_color` (the sky colour, by default --
    // `Lighting::default`) rather than a hard render-radius edge. Planar
    // (view-space depth), not radial (distance from the eye) -- measured on
    // a tile-based GPU (Apple M3) that a world-space `distance()` needs a
    // `world_pos` varying, and on this scene (~480k triangles, real
    // overdraw) that varying alone cost ~0.3ms/frame in interpolation
    // traffic, on top of the extra ALU cost.
    //
    // Two earlier approaches to view-space depth both turned out to read a
    // value naga's DX12 backend gets wrong (wgpu 24; CI's Windows runner
    // caught both, not this machine or the M3): `@builtin(position).w`,
    // which the WGSL/WebGPU spec says is `1/w_clip` but which Direct3D's
    // pixel-shader `SV_Position.w` gives raw, uninverted, with naga not
    // correcting for the difference; and, as a fix, a plain vertex-shader
    // `1/w_clip` carried across as an ordinary `@interpolate(linear)`
    // varying, which was correct everywhere but still cost an extra f32 of
    // interpolation traffic (measured on the M3: ~80 FPS, ~0.043 ms/frame).
    //
    // This reads `@builtin(position).z` instead -- the rasterizer's actual
    // depth-buffer value, not a value under naga's `.w` handling at all, and
    // recoverable with zero varyings since it's already per-fragment via the
    // hardware depth interpolant every pipeline needs anyway. `reverse_z`
    // only touches the z output row (not w), so starting from
    // `perspective_rh`'s un-reversed NDC (`ndc_z = C - A/d` for view depth
    // `d`) and applying `new_ndc_z = 1 - ndc_z` gives `ndc_z' = A/d - B` --
    // solved for `d`, that's the `A/(z+B)` below. `frame.depth` carries
    // `(A, B)`, computed once on the CPU from the same near/far the
    // projection itself uses (`render.rs`'s `reverse_z_depth_constants`,
    // pinned against the real matrix by a unit test there rather than
    // trusted as algebra alone).
    //
    // The visible difference from the old world_pos-based radial fog is
    // real, not a regression: at the screen edges the plane fades in
    // slightly later than the true radial distance would (`distance` >
    // `depth` off-axis), never earlier, so nothing pops out of fog too soon.
    //
    // `fog.y <= fog.x` is "fog off" ([`Lighting`]'s documented convention),
    // checked explicitly rather than relied on via a huge sentinel distance:
    // `select` here means a disabled fog never evaluates `smoothstep` on an
    // equal-edges range, whose result WGSL leaves unspecified.
    let view_depth = frame.depth.x / (in.clip_pos.z + frame.depth.y);
    let fog_amount = select(0.0, smoothstep(frame.fog.x, frame.fog.y, view_depth), frame.fog.y > frame.fog.x);
    let color = mix(lit, frame.fog_color.rgb, fog_amount);
    return vec4<f32>(color, 1.0);
}
