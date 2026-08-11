// A thin wireframe box around one targeted voxel (issue #52). Local geometry
// is a static unit-cube edge list, uploaded once (see OUTLINE_CUBE_EDGES in
// render.rs); only the box's world-space origin changes per frame, via a
// tiny uniform -- no per-block state on the mesh vertices, no second draw of
// the chunk (both would cost far more than this).

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct Outline {
    // .xyz is the targeted voxel's world-space min corner; .w is unused,
    // padding for WGSL's 16-byte uniform alignment on a vec3.
    origin: vec4<f32>,
};

@group(1) @binding(0) var<uniform> outline: Outline;

@vertex
fn vs_main(@location(0) local_pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    let world_pos = local_pos + outline.origin.xyz;
    return camera.view_proj * vec4<f32>(world_pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
