// The far terrain (`docs/PROPOSAL_FAR_VIEW.md` §3.2): beyond the voxel rings,
// the land as a height field.
//
// There is no vertex buffer. Every patch is the same 35 x 35 grid of vertex
// indices, drawn once per patch as an instance: the inner 33 x 33 are the
// patch's heights, read from a storage buffer, and the outer ring repeats the
// edge vertex dropped by the patch's skirt depth -- so the skirts that hide the
// seam against a coarser neighbour cost no geometry of their own. One draw for
// the whole far terrain, whatever its size.

// Same `Frame` the other pipelines bind (`render.rs`'s `FrameUniform`): one sun,
// one fog, one camera.
struct Frame {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    fog_color: vec4<f32>,
    fog: vec4<f32>,
};

// One patch: its lowest corner, the width of one quad, and how far its skirt
// drops, all in blocks.
struct Patch {
    origin: vec2<f32>,
    quad: f32,
    skirt: f32,
};

struct Params {
    // The box the voxel rings draw themselves: nothing here is drawn inside it.
    hole_min: vec4<f32>,
    hole_max: vec4<f32>,
    // The average colour of the ground's top material, in linear light.
    top_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<storage, read> heights: array<f32>;
@group(1) @binding(1) var<storage, read> patches: array<Patch>;
@group(1) @binding(2) var<storage, read> draw_slots: array<u32>;
@group(1) @binding(3) var<uniform> params: Params;

const QUADS: i32 = 32;
const VERTS: u32 = 33u;
const GRID: u32 = 35u;

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

fn height(base: u32, i: i32, j: i32) -> f32 {
    let ci = u32(clamp(i, 0, QUADS));
    let cj = u32(clamp(j, 0, QUADS));
    return heights[base + cj * VERTS + ci];
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @builtin(instance_index) instance: u32,
) -> VertexOut {
    let slot = draw_slots[instance];
    let tile = patches[slot];
    let base = slot * VERTS * VERTS;
    let gx = vertex % GRID;
    let gz = vertex / GRID;
    // Grid 0 and 34 are the skirt ring; 1..=33 are the patch's own vertices.
    let i = clamp(i32(gx) - 1, 0, QUADS);
    let j = clamp(i32(gz) - 1, 0, QUADS);
    let skirt = gx == 0u || gx == GRID - 1u || gz == 0u || gz == GRID - 1u;
    var y = height(base, i, j);
    if skirt {
        y = y - tile.skirt;
    }
    let world = vec3<f32>(
        tile.origin.x + f32(i) * tile.quad,
        y,
        tile.origin.y + f32(j) * tile.quad,
    );
    // The surface's slope from the neighbouring heights, one-sided at the edge.
    let il = max(i - 1, 0);
    let ir = min(i + 1, QUADS);
    let jl = max(j - 1, 0);
    let jr = min(j + 1, QUADS);
    let dx = (height(base, ir, j) - height(base, il, j)) / (f32(ir - il) * tile.quad);
    let dz = (height(base, i, jr) - height(base, i, jl)) / (f32(jr - jl) * tile.quad);

    var out: VertexOut;
    out.clip_pos = frame.view_proj * vec4<f32>(world, 1.0);
    out.world_pos = world;
    out.normal = normalize(vec3<f32>(-dx, 1.0, -dz));
    return out;
}

// Most patches lie wholly outside the hole, and are drawn with this: no
// discard, so the GPU may treat them as plain opaque geometry (on a
// tile-based GPU, a discard turns off hidden-surface removal for the whole
// pipeline).
@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return shade(in);
}

// The few patches that cross the hole's edge: the part inside is the voxels'.
@fragment
fn fs_cut(in: VertexOut) -> @location(0) vec4<f32> {
    let p = in.world_pos;
    if all(p >= params.hole_min.xyz) && all(p <= params.hole_max.xyz) {
        discard;
    }
    return shade(in);
}

fn shade(in: VertexOut) -> vec4<f32> {
    let p = in.world_pos;
    let n = normalize(in.normal);
    // The terrain's own lighting terms (`mesh.wgsl`): hemispheric ambient and
    // one sun. No ambient occlusion -- open ground from kilometres away has
    // nothing near enough to occlude it.
    let ambient = mix(frame.ambient.x, frame.ambient.y, n.y * 0.5 + 0.5);
    let diffuse = max(dot(n, frame.sun_dir.xyz), 0.0) * frame.sun_color.w;
    let lit = params.top_color.rgb * frame.sun_color.rgb * (ambient + diffuse);

    let dist = distance(p, frame.eye.xyz);
    let fog_amount = select(0.0, smoothstep(frame.fog.x, frame.fog.y, dist), frame.fog.y > frame.fog.x);
    return vec4<f32>(mix(lit, frame.fog_color.rgb, fog_amount), 1.0);
}
