// Diffuse-lit geometry. Vertices arrive already in world space, so the camera
// uniform is just the combined view*projection.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) ao: f32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) ao: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.world_normal = in.normal;
    out.ao = in.ao;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);

    // Directional sun: clear sun-side / shadow-side split.
    let light_dir = normalize(vec3<f32>(0.4, 1.0, 0.3));
    let diffuse = max(dot(n, light_dir), 0.0);

    // Hemispheric ambient: a touch brighter facing up (sky) than down (ground).
    let ambient = mix(0.28, 0.42, n.y * 0.5 + 0.5);

    // Baked ambient occlusion darkens crevices; keep a floor so nothing is pure black.
    let ao = mix(0.4, 1.0, in.ao);

    let base = vec3<f32>(0.42, 0.60, 0.32);
    let color = base * (ambient + diffuse * 0.75) * ao;
    return vec4<f32>(color, 1.0);
}
