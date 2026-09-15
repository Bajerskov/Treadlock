// Skid marks, blended by multiplication.
//
// The colour that arrives is not a colour but a multiplier on whatever is
// already on the track: 1 leaves it untouched, 0 paints it black. That is what
// rubber does - it darkens the road and its markings alike - and it means a
// mark that has worn away fades to white and simply stops mattering, with no
// pop when it is finally recycled.

struct FrameData {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    sun: vec4<f32>,
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameData;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) factor: vec4<f32>,
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
    out.factor = color;
    out.uv = uv;
    out.world = pos;
    out.clip = frame.view_proj * vec4<f32>(pos, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Soften across the width of the strip, so a mark has the edge of a tyre
    // rather than of a ruler. uv.x runs across, uv.y along.
    let across = abs(in.uv.x - 0.5) * 2.0;
    let edge = 1.0 - smoothstep(0.55, 1.0, across);

    // Marks fade out with distance as well as with age: at the far end of the
    // tube they would otherwise read as dirt on the lens.
    let dist = length(frame.camera_pos.xyz - in.world);
    let haze = exp(-dist * frame.fog.a * 0.8);

    // 1 is "leave the track alone", so every reason to weaken the mark pulls
    // the multiplier back toward 1.
    let strength = edge * haze;
    let factor = mix(vec3<f32>(1.0), in.factor.rgb, strength);
    return vec4<f32>(factor, 1.0);
}
