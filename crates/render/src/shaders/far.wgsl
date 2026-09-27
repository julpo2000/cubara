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
    // What the ground is made of, in linear light (`aggregate::Materials`):
    // the top of the surface block, its side, the soil and the stone beneath.
    top_color: vec4<f32>,
    // `w`: how many blocks of soil lie between the surface block and stone.
    side_color: vec4<f32>,
    soil_color: vec4<f32>,
    stone_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<storage, read> heights: array<f32>;
@group(1) @binding(1) var<storage, read> patches: array<Patch>;
@group(1) @binding(2) var<storage, read> draw_slots: array<u32>;
@group(1) @binding(3) var<uniform> params: Params;
// `aggregate::MaskingTable`: 16 x 16 x (16 x 16), its last two axes stacked.
@group(1) @binding(4) var masking: texture_3d<f32>;
@group(1) @binding(5) var masking_sampler: sampler;

const QUADS: i32 = 32;
const VERTS: u32 = 33u;
const GRID: u32 = 35u;

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    // The shaded, fogged colour, worked out per vertex (see `shade`).
    @location(0) color: vec3<f32>,
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
    var world = vec3<f32>(
        tile.origin.x + f32(i) * tile.quad,
        y,
        tile.origin.y + f32(j) * tile.quad,
    );
    // Strictly inside the voxels' box, the ground is theirs: sink it to the
    // box's floor, under everything they draw. The box's edges fall on
    // multiples of 128 blocks and every quad edge near them does too, so no
    // triangle straddles the edge -- the ones touching it from inside slope
    // down from it, and that slope is a wall closing the seam between the
    // two. No fragment is ever discarded (on a tile-based GPU a discard turns
    // off hidden-surface removal for the whole pipeline).
    if all(world > params.hole_min.xyz) && all(world < params.hole_max.xyz) {
        world.y = params.hole_min.y - 1.0;
    }
    // The surface's slope from the neighbouring heights, one-sided at the edge.
    let il = max(i - 1, 0);
    let ir = min(i + 1, QUADS);
    let jl = max(j - 1, 0);
    let jr = min(j + 1, QUADS);
    let dx = (height(base, ir, j) - height(base, il, j)) / (f32(ir - il) * tile.quad);
    let dz = (height(base, i, jr) - height(base, i, jl)) / (f32(jr - jl) * tile.quad);

    var out: VertexOut;
    out.clip_pos = frame.view_proj * vec4<f32>(world, 1.0);
    out.color = shade(world, vec2<f32>(dx, dz));
    return out;
}

// ---------------------------------------------------------------------------
// Block-aggregate shading (`docs/PROPOSAL_FAR_VIEW.md` §3.3). The definition is
// `crates/render/src/aggregate.rs`; every function below is the one of the
// same name there, and its tests are what say this is right.
// ---------------------------------------------------------------------------

const TABLE_LAST: f32 = 15.0;
// `aggregate::TABLE_MAX_SLOPE`, as steepness `s / (1 + s)`.
const TABLE_MAX_STEEPNESS: f32 = 0.8501038;

// `MaskingTable::top_fraction`: the top fraction when the risers along one
// axis (A) face the eye and those along the other (B) hide what is behind
// them. Slopes and view components are magnitudes.
fn masked_top(g_a: f32, g_b: f32, v_a: f32, v_b: f32, v_y: f32) -> f32 {
    let slope = length(vec2<f32>(g_a, g_b));
    let seen = v_y + g_a * v_a;
    let at = clamp(
        vec4<f32>(
            sqrt(slope / (1.0 + slope) / TABLE_MAX_STEEPNESS),
            g_b / (g_a + g_b),
            g_a * v_a / seen,
            1.0 - sqrt(1.0 - min(g_b * v_b / seen, 1.0)),
        ) * TABLE_LAST,
        vec4<f32>(0.0),
        vec4<f32>(TABLE_LAST),
    );
    // The fourth axis by hand, between two stacks the filter never mixes.
    let stack = min(floor(at.w), TABLE_LAST - 1.0);
    let blend = at.w - stack;
    let xy = (at.xy + 0.5) / 16.0;
    let near = textureSampleLevel(masking, masking_sampler, vec3<f32>(xy, (at.z + 16.0 * stack + 0.5) / 256.0), 0.0).r;
    let far = textureSampleLevel(masking, masking_sampler, vec3<f32>(xy, (at.z + 16.0 * (stack + 1.0) + 0.5) / 256.0), 0.0).r;
    return mix(near, far, blend);
}

// `visible_weights`: how much of the pixel tops, x-risers and z-risers cover,
// for a slope of gradient `g` seen along `v` (unit, toward the eye).
fn visible_weights(g: vec2<f32>, v: vec3<f32>) -> vec3<f32> {
    // A riser faces the eye when the eye is on its downhill side.
    let x_faces = g.x * v.x < 0.0;
    let z_faces = g.y * v.z < 0.0;
    let x_hides = g.x * v.x > 0.0;
    let z_hides = g.y * v.z > 0.0;
    if x_faces && z_hides {
        let top = masked_top(abs(g.x), abs(g.y), abs(v.x), abs(v.z), v.y);
        return vec3<f32>(top, 1.0 - top, 0.0);
    }
    if z_faces && x_hides {
        let top = masked_top(abs(g.y), abs(g.x), abs(v.z), abs(v.x), v.y);
        return vec3<f32>(top, 0.0, 1.0 - top);
    }
    // Nothing hides: each face's share of the projected area.
    let top = max(v.y, 0.0);
    let x = select(0.0, abs(g.x * v.x), x_faces);
    let z = select(0.0, abs(g.y * v.z), z_faces);
    let sum = top + x + z;
    // A fragment of a slope facing away (the CPU's `None`): the smooth
    // surface's own silhouette. Call it top.
    if sum <= 1e-6 {
        return vec3<f32>(1.0, 0.0, 0.0);
    }
    return vec3<f32>(top, x, z) / sum;
}

// `face_light`: `mesh.wgsl`'s lighting for a face of normal `n`, before
// texture and ambient occlusion -- open ground from kilometres away has
// nothing near enough to occlude it.
fn face_light(n: vec3<f32>) -> vec3<f32> {
    let ambient = mix(frame.ambient.x, frame.ambient.y, n.y * 0.5 + 0.5);
    let diffuse = max(dot(n, frame.sun_dir.xyz), 0.0) * frame.sun_color.w;
    return frame.sun_color.rgb * (ambient + diffuse);
}

// `riser_colour`: the mean side colour of the risers on a slope of `slope`
// blocks per block -- the surface block's side at the top of each, then
// soil, then stone.
fn riser_colour(slope: f32) -> vec3<f32> {
    let s = abs(slope);
    if s <= 0.0 {
        return params.side_color.rgb;
    }
    let depth = params.side_color.w;
    let surface = min(s, 1.0);
    let soil = clamp(s - 1.0, 0.0, depth);
    let stone = max(s - 1.0 - depth, 0.0);
    return (params.side_color.rgb * surface + params.soil_color.rgb * soil
        + params.stone_color.rgb * stone) / s;
}

// The colour of the ground at `p`, whose slope is `g` blocks per block: the
// faces a staircase of that slope really has, as much of each as the eye
// sees, each lit as `mesh.wgsl` lights a block face; then the fog.
//
// Per vertex, not per fragment. Quads are at most about 16 px across (the
// split rule), so the view direction barely turns across one, and the slope
// is interpolated either way. Per fragment this cost 0.37 ms at the flight
// eye on a GTX 1060; per vertex it runs about a quarter as often.
fn shade(p: vec3<f32>, g: vec2<f32>) -> vec3<f32> {
    let v = normalize(frame.eye.xyz - p);
    let w = visible_weights(g, v);
    let riser_x = vec3<f32>(select(1.0, -1.0, g.x > 0.0), 0.0, 0.0);
    let riser_z = vec3<f32>(0.0, 0.0, select(1.0, -1.0, g.y > 0.0));
    let lit = params.top_color.rgb * face_light(vec3<f32>(0.0, 1.0, 0.0)) * w.x
        + riser_colour(g.x) * face_light(riser_x) * w.y
        + riser_colour(g.y) * face_light(riser_z) * w.z;

    let dist = distance(p, frame.eye.xyz);
    let fog_amount = select(0.0, smoothstep(frame.fog.x, frame.fog.y, dist), frame.fog.y > frame.fog.x);
    return mix(lit, frame.fog_color.rgb, fog_amount);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
