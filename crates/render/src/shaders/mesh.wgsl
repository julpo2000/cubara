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

// Throwaway diagnostic (variant H): lighting AND fog entirely as pipeline
// overrides, so fs_main never touches `frame` at all. Fog thresholds
// pre-converted to clip-space for --bench 64's actual fog_start=576/
// fog_end=960, same values variant E used.
override h_ambient_low: f32 = 0.28;
override h_ambient_high: f32 = 0.42;
override h_ao_floor: f32 = 0.4;
override h_diffuse_weight: f32 = 0.75;
override h_fog_z_start: f32 = 0.0001236172919757099;
override h_fog_z_end: f32 = 5.4169375135423426e-05;

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
    // The packed `face` index itself, not the unit vector it picks out of
    // `FACE_NORMALS` -- greedy-meshed faces never bend across a triangle, so
    // there is nothing for the rasterizer to interpolate, and shipping one
    // flat u32 instead of an interpolated (and fragment-renormalized) vec3
    // is strictly less varying traffic for the same answer.
    @location(0) @interpolate(flat) face: u32,
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
    out.face = face;
    out.ao = f32(ao_raw) / 3.0;
    out.uv = vec2<f32>(u, v);
    out.layer = tex_layer;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Throwaway diagnostic (variant H): lighting and fog entirely as
    // pipeline overrides -- this function never reads `frame`.
    let n = FACE_NORMALS[in.face];
    let diffuse = max(dot(n, vec3<f32>(0.37115407, 0.92788516, 0.27836555)), 0.0) * h_diffuse_weight;
    let ambient = mix(h_ambient_low, h_ambient_high, n.y * 0.5 + 0.5);
    let ao = mix(h_ao_floor, 1.0, in.ao);
    let tex = textureSample(block_textures, block_sampler, in.uv, in.layer);
    let lit = tex.rgb * (ambient + diffuse) * ao;
    let fog_amount = 1.0 - smoothstep(h_fog_z_end, h_fog_z_start, in.clip_pos.z);
    let color = mix(lit, vec3<f32>(0.45, 0.62, 0.80), fog_amount);
    return vec4<f32>(color, 1.0);
}
