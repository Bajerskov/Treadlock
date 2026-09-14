// Screen-space UI pass.
//
// Positions arrive in pixels from the top left, which is how the layout code
// thinks, and are converted to clip space here using the window size. One atlas
// serves both the bitmap font and solid shapes, so the whole HUD is one draw.

struct Push {
    model: mat4x4<f32>,
    tint: vec4<f32>,
    // xy = window size in pixels
    params: vec4<f32>,
};
var<push_constant> pc: Push;

@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    let ndc = pos / max(pc.params.xy, vec2<f32>(1.0, 1.0)) * 2.0 - vec2<f32>(1.0, 1.0);
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // The atlas is white with coverage in alpha, so colour comes entirely from
    // the vertex and one atlas serves every tint.
    let coverage = textureSample(atlas, atlas_sampler, in.uv).a;
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
