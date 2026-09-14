struct Push {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    camera_pos: vec4<f32>,
    tint: vec4<f32>,
};
var<push_constant> pc: Push;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    let world = pc.model * vec4<f32>(pos, 1.0);
    out.world = world.xyz;
    out.normal = (pc.model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = uv;
    out.clip = pc.view_proj * world;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(vec3<f32>(0.4, 0.8, 0.3));
    let diffuse = max(dot(n, l), 0.0);
    return vec4<f32>(pc.tint.rgb * (0.25 + 0.75 * diffuse), 1.0);
}
