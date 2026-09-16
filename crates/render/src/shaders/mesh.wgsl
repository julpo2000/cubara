// Packed node-local geometry, textured. Vertices carry a lattice position
// local to their node plus baked AO, a texture-array layer, a face
// direction, this corner's own tile coordinate, and the node index itself --
// see docs/PHASE1_ARCHITECTURE.md §5.2 for the bit layout. World placement is
// a per-node origin add (scaled by the node's own lattice step) here, not a
// CPU-side translate.

// Everything about the frame beyond the geometry that still varies at
// runtime: camera, sun direction/colour, fog. One binding shared with
// `figure.wgsl` (`render.rs`'s `FrameUniform`/`Lighting`), so the two can't
// disagree about where the sun is the way they used to.
//
// Ambient (ground/sky/AO floor) and the diffuse weight are *not* here --
// every call site that builds a `Lighting` leaves them at `Lighting::default`
// (only fog varies), so they are `override` pipeline constants below
// instead of uniform reads. `.w` on `sun_color` is unused (was the diffuse
// weight); kept as a full `vec4` for std140 alignment and so a future
// non-white sun has a ready runtime-varying field.
struct Frame {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    fog_color: vec4<f32>,
    // .x fog start, .y fog end, .z time of day.
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;

// Pipeline-compile-time, not frame-time: `render.rs`'s `mesh_pipeline_constants`
// feeds these from the same `Lighting::default()` that used to be packed into
// this uniform every frame. Where `mix(0.28, 0.42, t)` and `* 0.75` were once
// hard `mesh.wgsl` literals (before `Lighting` existed), then became uniform
// reads (before this), they are pipeline overrides now -- the compiler folds
// them the way it folded the original literals, since a pipeline recompile
// (not a per-frame write) is the only way they can change. If a feature ever
// needs one of these to vary within a run (day/night dimming ambient, a
// coloured sun), it moves back into `Frame` above, and its cost is measured
// again on arrival -- the same way moving it out was.
override ambient_low: f32 = 0.28;
override ambient_high: f32 = 0.42;
override ao_floor: f32 = 0.4;
override diffuse_weight: f32 = 0.75;
// Folds away the `sun_color.rgb` multiply entirely when true (the only value
// `Lighting::default` has ever produced) -- `tex.rgb * vec3(1.0)` is exactly
// `tex.rgb`, so the multiply was pure cost for a shader that never used it.
override sun_is_white: bool = true;

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
    // 1/w_clip, for the fog below -- an ordinary linearly-interpolated
    // varying, not read back off `clip_pos.w`. See the comment at its use
    // site: reading the position builtin's `w` in the fragment stage is not
    // portable on wgpu 24's DX12 backend.
    @location(4) @interpolate(linear) inv_w: f32,
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
    // 1/w here, in the vertex shader, is unambiguous -- `clip_pos.w` is a
    // plain value this shader just computed, not a value read back off a
    // position-semantic builtin. Marked `@interpolate(linear)` (screen-space
    // linear, not perspective-correct) deliberately: 1/w is exactly affine
    // in screen space, which is the standard identity perspective-correct
    // interpolation itself is built on, so a plain linear interpolation of
    // this already-reciprocal value recovers the true per-fragment 1/w_clip.
    out.inv_w = 1.0 / out.clip_pos.w;
    out.face = face;
    out.ao = f32(ao_raw) / 3.0;
    out.uv = vec2<f32>(u, v);
    out.layer = tex_layer;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = FACE_NORMALS[in.face];

    // Directional sun: clear sun-side / shadow-side split. `sun_dir` arrives
    // already normalized (`Lighting`'s doc comment); the shader does not
    // renormalize it.
    let diffuse = max(dot(n, frame.sun_dir.xyz), 0.0) * diffuse_weight;

    // Hemispheric ambient: a touch brighter facing up (sky) than down (ground).
    let ambient = mix(ambient_low, ambient_high, n.y * 0.5 + 0.5);

    // Baked ambient occlusion darkens crevices; keep a floor so nothing is pure black.
    let ao = mix(ao_floor, 1.0, in.ao);

    let tex = textureSample(block_textures, block_sampler, in.uv, in.layer);
    // `sun_is_white` folds this branch to just `tex.rgb * (...)` at pipeline
    // compile time in the only configuration this engine has ever shipped --
    // `frame.sun_color.rgb` is read at all only the day a non-white sun
    // exists, and even then only by the pipeline built for it.
    var lit: vec3<f32>;
    if sun_is_white {
        lit = tex.rgb * (ambient + diffuse) * ao;
    } else {
        lit = tex.rgb * frame.sun_color.rgb * (ambient + diffuse) * ao;
    }

    // Depth fog, fading toward `fog_color` (the sky colour, by default --
    // `Lighting::default`) rather than a hard render-radius edge. Planar
    // (view-space depth), not radial (distance from the eye) -- measured on
    // a tile-based GPU (Apple M3) that a world-space `distance()` needs a
    // `world_pos` varying, and on this scene (~480k triangles, real
    // overdraw) that varying alone cost ~0.3ms/frame in interpolation
    // traffic, on top of the extra ALU cost. View-space depth for this
    // projection is exactly `w_clip`, i.e. `1/frag_coord.w` -- a WGSL/WebGPU
    // guarantee, true regardless of `reverse_z`'s flip (that only touches
    // the z output row, not w) -- so it looks free: no extra varying, just
    // reading the position builtin's `w` back in the fragment stage.
    //
    // It is not actually free, on every backend: wgpu 24's DX12 target hit a
    // known naga/HLSL quirk here (CI caught it on the Windows runner, where
    // this golden came back ~23% different at up to 220/255 per channel --
    // not driver noise, a wrong value). Direct3D's SV_Position.w in a pixel
    // shader is the raw interpolated `w`, not `1/w` like Vulkan and Metal;
    // naga's HLSL backend does not correct for the difference, so
    // `in.clip_pos.w` on that backend was not `1/w_clip` at all. `inv_w` above
    // sidesteps it entirely by never reading the position builtin's `w` in
    // this stage -- it is a plain vertex-shader value, computed the same way
    // on every backend, carried across as an ordinary linear-interpolated
    // varying. One extra `f32` (the cheapest interpolation mode there is),
    // for a fog that is actually cross-backend rather than only
    // cross-backend on the machines this branch happened to be measured on.
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
    let view_depth = 1.0 / in.inv_w;
    let fog_amount = select(0.0, smoothstep(frame.fog.x, frame.fog.y, view_depth), frame.fog.y > frame.fog.x);
    let color = mix(lit, frame.fog_color.rgb, fog_amount);
    return vec4<f32>(color, 1.0);
}
