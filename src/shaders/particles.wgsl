// Additively blended billboard particles.
//
// Quads arrive already facing the camera, built on the CPU from the camera
// basis, so this stays a pass-through with a soft radial falloff.

struct FrameData {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    sun: vec4<f32>,
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameData;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    out.color = color;
    out.uv = uv;
    out.world = pos;
    out.clip = frame.view_proj * vec4<f32>(pos, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Soft round sprite, so no texture is needed and nothing has to be loaded
    // over the target's slow storage.
    let d = length(in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
    let falloff = clamp(1.0 - d, 0.0, 1.0);
    // Squared edge plus a tight core reads as a glowing ember rather than a
    // flat disc.
    let intensity = falloff * falloff + pow(falloff, 8.0) * 1.5;

    var colour = in.color.rgb * intensity * in.color.a;

    // Fade into the fog so particles do not float as bright dots in the haze.
    let dist = length(frame.camera_pos.xyz - in.world);
    colour *= exp(-dist * frame.fog.a * 0.6);

    return vec4<f32>(colour, in.color.a * intensity);
}
