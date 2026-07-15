// Camera-transformed, diffuse-lit geometry. The camera uniform carries the
// combined view*projection plus the model matrix so normals can be transformed
// for lighting.

struct Camera {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = camera.model * vec4<f32>(in.position, 1.0);
    out.clip_pos = camera.view_proj * world;
    // model is rotation-only here, so this is a valid normal transform.
    out.world_normal = (camera.model * vec4<f32>(in.normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.5, 1.0, 0.3));
    let n = normalize(in.world_normal);
    let diffuse = max(dot(n, light_dir), 0.0);
    let ambient = 0.25;
    let base = vec3<f32>(0.55, 0.75, 0.95);
    let color = base * (ambient + diffuse * 0.85);
    return vec4<f32>(color, 1.0);
}
