// Minimal pipeline validation: a single hard-coded, vertex-colored triangle.
// No vertex buffer — positions and colors are indexed by the vertex id.

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>( 0.0,  0.5),
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
    );
    var colors = array<vec3<f32>, 3>(
        vec3<f32>(1.0, 0.25, 0.35),
        vec3<f32>(0.25, 1.0, 0.45),
        vec3<f32>(0.35, 0.45, 1.0),
    );

    var out: VsOut;
    out.clip_pos = vec4<f32>(positions[idx], 0.0, 1.0);
    out.color = colors[idx];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
