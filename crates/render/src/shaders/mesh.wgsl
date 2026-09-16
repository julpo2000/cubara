// Packed node-local geometry, textured. Vertices carry a lattice position
// local to their node plus baked AO, a texture-array layer, a face
// direction, this corner's own tile coordinate, and the node index itself --
// see docs/PHASE1_ARCHITECTURE.md §5.2 for the bit layout. World placement is
// a per-node origin add (scaled by the node's own lattice step) here, not a
// CPU-side translate.

// Only the camera matrix -- vs_main is all this shader reads `frame` for.
// Lighting and fog used to live in this same uniform (still true for
// `figure.wgsl`, which shares the buffer), but moved to `override` pipeline
// constants below: the fragment stage merely being *configured* to see a
// uniform buffer measured as a real cost on Apple's tile-based GPU (an
// occupancy cliff, not a gradual one -- see BENCHMARKS.md's package-4
// footnote), and every value below changes far less often than once a frame
// -- a lighting update, not a per-frame read. `render.rs`'s
// `mesh_pipeline_constants` is the one place these are derived from
// `Lighting`; `SceneRenderer::set_lighting` rebuilds this pipeline in the
// background whenever they actually change, not every frame.
struct Frame {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;

override ambient_low: f32 = 0.28;
override ambient_high: f32 = 0.42;
override ao_floor: f32 = 0.4;
override diffuse_weight: f32 = 0.75;
override sun_dir_x: f32 = 0.37115407;
override sun_dir_y: f32 = 0.92788516;
override sun_dir_z: f32 = 0.27836555;
override sun_color_r: f32 = 1.0;
override sun_color_g: f32 = 1.0;
override sun_color_b: f32 = 1.0;
override fog_color_r: f32 = 0.45;
override fog_color_g: f32 = 0.62;
override fog_color_b: f32 = 0.80;
// Real-distance units (blocks), same convention `Lighting` documents:
// fog_end <= fog_start means fog is off.
override fog_start: f32 = 0.0;
override fog_end: f32 = 0.0;
// (A, B) such that view_depth = A / (clip_pos.z + B) -- see the derivation
// on `render.rs`'s `reverse_z_depth_constants`, which this is fed from.
override depth_a: f32 = 0.1000050002500125;
override depth_b: f32 = 5.000250012500625e-05;
// The active render target's size, in pixels -- converts
// `@builtin(position).xy` (a framebuffer pixel) to NDC below, for radial
// fog's per-fragment ray direction. Rebuilt on resize
// (`SceneRenderer::resize`), same as the depth buffer.
override viewport_width: f32 = 1920.0;
override viewport_height: f32 = 1080.0;
override aspect: f32 = 1.7777777777777777;
// tan(30 deg): half of the fixed 60 deg vertical FOV `CameraUniform::look_view_proj`
// always builds. A `const`, not an `override` -- this engine has never
// varied FOV, so there is nothing to rebuild for.
const TAN_HALF_FOV: f32 = 0.5773502691896257;

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
    // *fragment* in fs_main: greedy-meshed faces never bend across a
    // triangle, so there is nothing to interpolate either way, but a
    // per-fragment dynamic array index measurably costs more than a flat
    // varying carrying the already-resolved vector (isolated A/B/C
    // diagnostic in the package-4 research).
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

    // Directional sun: clear sun-side / shadow-side split. `sun_dir` is
    // normalized on the CPU (`Lighting`'s doc comment); not renormalized here.
    let sun_dir = vec3<f32>(sun_dir_x, sun_dir_y, sun_dir_z);
    let diffuse = max(dot(n, sun_dir), 0.0) * diffuse_weight;

    // Hemispheric ambient: a touch brighter facing up (sky) than down (ground).
    let ambient = mix(ambient_low, ambient_high, n.y * 0.5 + 0.5);

    // Baked ambient occlusion darkens crevices; keep a floor so nothing is pure black.
    let ao = mix(ao_floor, 1.0, in.ao);

    let tex = textureSample(block_textures, block_sampler, in.uv, in.layer);
    let sun_color = vec3<f32>(sun_color_r, sun_color_g, sun_color_b);
    let lit = tex.rgb * sun_color * (ambient + diffuse) * ao;

    // Radial distance fog, fading toward `fog_color` (the sky colour, by
    // default `Lighting::default`) rather than a hard render-radius edge.
    // Radial (true distance from the eye), not planar (view-axis depth) --
    // planar fog shipped first because it was reachable with zero varyings,
    // but it has a real, player-visible artifact: the same mountain reads
    // foggier dead ahead than at the screen edge, since off-axis geometry at
    // equal depth is actually farther away. Radial fixes that, and turned
    // out to cost nothing extra once fog was already override-based rather
    // than a uniform read (measured -- see BENCHMARKS.md's package-4
    // footnote, variant I).
    //
    // view_depth recovers from `@builtin(position).z` exactly as before:
    // `reverse_z` only flips the z output row of an otherwise-standard
    // perspective projection, so `view_depth = depth_a / (clip_pos.z +
    // depth_b)` (`render.rs`'s `reverse_z_depth_constants`, pinned against
    // the real projection matrix by a unit test there).
    //
    // Radial distance from view_depth needs the ray direction too, which
    // comes from `@builtin(position).xy` -- the framebuffer pixel, converted
    // to NDC with the viewport size -- rather than `@builtin(position).w`:
    // wgpu 24's DX12 backend has a real naga/HLSL bug reading `.w` in the
    // fragment stage (see mesh_fog_from_clip_pos_z's own history), so this
    // shader stays clear of that builtin's `.w`/`.z` components beyond the
    // `.z` already established safe, and never touches `.w` at all. For a
    // symmetric perspective frustum, radial = view_depth * sqrt(1 +
    // (ndc_x*TAN_HALF_FOV*aspect)^2 + (ndc_y*TAN_HALF_FOV)^2) -- the same
    // relationship the projection matrix itself encodes, evaluated per
    // fragment instead of carried across as a varying.
    //
    // `fog_end <= fog_start` is "fog off" ([`Lighting`]'s documented
    // convention), checked explicitly rather than relied on via a huge
    // sentinel distance: `select` here means a disabled fog never evaluates
    // `smoothstep` on an equal-edges range, whose result WGSL leaves
    // unspecified.
    let view_depth = depth_a / (in.clip_pos.z + depth_b);
    let ndc_x = (in.clip_pos.x / viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.clip_pos.y / viewport_height) * 2.0;
    let off_axis_x = ndc_x * TAN_HALF_FOV * aspect;
    let off_axis_y = ndc_y * TAN_HALF_FOV;
    let radial = view_depth * sqrt(1.0 + off_axis_x * off_axis_x + off_axis_y * off_axis_y);
    let fog_amount = select(0.0, smoothstep(fog_start, fog_end, radial), fog_end > fog_start);
    let fog_color = vec3<f32>(fog_color_r, fog_color_g, fog_color_b);
    let color = mix(lit, fog_color, fog_amount);
    return vec4<f32>(color, 1.0);
}
