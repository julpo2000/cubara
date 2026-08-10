// Packed node-local geometry, textured. Vertices carry a lattice position
// local to their chunk ("node") plus baked AO, a texture-array layer, a face
// direction, and this corner's own tile coordinate -- see
// docs/PHASE1_ARCHITECTURE.md §5.2 for the bit layout. World placement is a
// per-node origin add here, not a CPU-side translate.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// One world-space origin per resident chunk ("node"), indexed by
// @builtin(instance_index) -- each draw's first_instance is the node index
// (see crates/render/src/arena.rs). xyz used; w is spare.
@group(1) @binding(0) var<storage, read> node_origins: array<vec4<f32>>;

@group(2) @binding(0) var block_textures: texture_2d_array<f32>;
@group(2) @binding(1) var block_sampler: sampler;

struct VsIn {
    @builtin(instance_index) node_index: u32,
    @location(0) packed0: u32,
    @location(1) packed1: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
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

    let local_pos = vec3<f32>(x, y, z);
    let origin = node_origins[in.node_index].xyz;
    let world_pos = origin + local_pos;

    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.normal = FACE_NORMALS[face];
    out.ao = f32(ao_raw) / 3.0;
    out.uv = vec2<f32>(u, v);
    out.layer = tex_layer;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);

    // Directional sun: clear sun-side / shadow-side split.
    let light_dir = normalize(vec3<f32>(0.4, 1.0, 0.3));
    let diffuse = max(dot(n, light_dir), 0.0);

    // Hemispheric ambient: a touch brighter facing up (sky) than down (ground).
    let ambient = mix(0.28, 0.42, n.y * 0.5 + 0.5);

    // Baked ambient occlusion darkens crevices; keep a floor so nothing is pure black.
    let ao = mix(0.4, 1.0, in.ao);

    let tex = textureSample(block_textures, block_sampler, in.uv, in.layer);
    let color = tex.rgb * (ambient + diffuse * 0.75) * ao;
    return vec4<f32>(color, 1.0);
}
