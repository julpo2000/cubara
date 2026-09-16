// Other players, as blocky figures (block 2.12b's goal: see the person on the
// other laptop).
//
// The vertices arrive already in world space and already coloured -- a figure
// is six boxes and there are a handful of players, so building the triangles on
// the CPU costs nothing and buys a pure function that can be tested with no GPU
// (see `figure.rs`). That is why there is no model uniform here: nothing to
// bind, nothing to keep in step with the vertex buffer.
//
// The shading is a fixed directional term rather than the terrain's ambient
// occlusion. A figure has no neighbours to be occluded by, and flat-lit boxes
// read as a single silhouette at distance, which is the opposite of what makes
// a person visible across a field.

// Same `Frame` `mesh.wgsl` binds (`render.rs`'s `FrameUniform`/`Lighting`):
// figures used to light themselves from a *different* hard-coded sun
// (`(0.4, 0.9, 0.25)` here, `(0.4, 1.0, 0.3)` in `mesh.wgsl`) -- one binding
// means one sun. No `ambient` field -- it moved to `mesh.wgsl`'s `override`
// pipeline constants (this shader never read it), so the struct shrank to
// match the buffer `mesh.wgsl`'s pipeline now writes.
struct Frame {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    fog_color: vec4<f32>,
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) world_pos: vec3<f32>,
    @location(1) color: vec3<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.clip_pos = frame.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = color;
    out.world_pos = world_pos;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // A face normal from screen-space derivatives, so the shading needs no
    // per-vertex normal and cannot disagree with the geometry it is shading.
    let normal = normalize(cross(dpdx(in.world_pos), dpdy(in.world_pos)));
    let lambert = clamp(dot(normal, frame.sun_dir.xyz), 0.0, 1.0);
    // Never fully dark: an unlit side of a person should still read as that
    // person's colour rather than as a hole.
    let shade = 0.55 + 0.45 * lambert;
    let lit = in.color * shade;

    // Same fog as the terrain (`mesh.wgsl`): a player at the render-distance
    // ring fades the way the ground under them does, rather than staying a
    // crisp silhouette against faded terrain.
    let dist = distance(in.world_pos, frame.eye.xyz);
    let fog_amount = select(0.0, smoothstep(frame.fog.x, frame.fog.y, dist), frame.fog.y > frame.fog.x);
    let color = mix(lit, frame.fog_color.rgb, fog_amount);
    return vec4<f32>(color, 1.0);
}
