// Forward shading pass.
//
// Per-frame data lives in a uniform buffer; per-object data goes through push
// constants, which stay within the 128-byte minimum Vulkan guarantees.

struct FrameData {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // xyz = direction toward the sun, w = time in seconds
    sun: vec4<f32>,
    // rgb = fog and sky colour, a = fog density
    fog: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameData;

struct Push {
    model: mat4x4<f32>,
    tint: vec4<f32>,
    // x: 0 = car body, 1 = track surface. y: emissive boost. z: metallic.
    params: vec4<f32>,
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
    // Uniform scale only, so the model matrix is safe for normals.
    out.normal = (pc.model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = uv;
    out.clip = frame.view_proj * world;
    return out;
}

// Lane markings and edge glow, derived from the tube's ring/arc-length UVs.
fn track_pattern(uv: vec2<f32>) -> vec3<f32> {
    // uv.x runs around the ring, uv.y is metres along the track.
    let ring = fract(uv.x);
    let along = uv.y;

    // Dashed centre line down the middle of the floor.
    let centre = 1.0 - smoothstep(0.0, 0.012, abs(ring - 0.5));
    let dash = step(0.5, fract(along * 0.08));
    var glow = vec3<f32>(0.15, 0.85, 1.0) * centre * dash;

    // Continuous strips up the walls, which give a strong sense of speed.
    let strip_a = 1.0 - smoothstep(0.0, 0.010, abs(ring - 0.25));
    let strip_b = 1.0 - smoothstep(0.0, 0.010, abs(ring - 0.75));
    glow += vec3<f32>(1.0, 0.35, 0.1) * (strip_a + strip_b);

    // Rungs across the tube, spaced in metres, to read closing speed.
    let rung = 1.0 - smoothstep(0.0, 0.06, abs(fract(along * 0.04) - 0.5));
    glow += vec3<f32>(0.3, 0.4, 0.9) * rung * 0.35;

    return glow;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let view_vec = frame.camera_pos.xyz - in.world;
    let dist = length(view_vec);
    let v = view_vec / dist;
    let l = normalize(frame.sun.xyz);

    var albedo = pc.tint.rgb;
    var emissive = vec3<f32>(0.0);

    if (pc.params.x > 0.5) {
        let pattern = track_pattern(in.uv);
        emissive += pattern * (1.2 + pc.params.y);
        // Darken the base surface where markings sit so they read as light.
        albedo = mix(albedo, albedo * 0.55, clamp(length(pattern), 0.0, 1.0));
    }

    // Half-Lambert keeps unlit faces readable rather than crushing them to black,
    // which matters in a tube where much of the surface faces away from the sun.
    let ndl = dot(n, l);
    let diffuse = ndl * 0.5 + 0.5;

    let h = normalize(l + v);
    let spec_power = mix(24.0, 96.0, pc.params.z);
    let spec = pow(max(dot(n, h), 0.0), spec_power) * mix(0.15, 0.9, pc.params.z);

    // Cheap stand-in for the environment term the ray-traced reflection pass
    // will replace: brighten toward grazing angles.
    let fresnel = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 4.0);
    let ambient = mix(vec3<f32>(0.16, 0.18, 0.24), frame.fog.rgb, 0.35);

    var colour = albedo * (ambient + vec3<f32>(1.0, 0.96, 0.88) * diffuse * 0.85);
    colour += vec3<f32>(1.0, 0.97, 0.9) * spec;
    colour += frame.fog.rgb * fresnel * mix(0.25, 0.8, pc.params.z);
    colour += emissive;

    // Exponential distance fog, which also hides the far end of the tube.
    let fog_amount = 1.0 - exp(-dist * frame.fog.a);
    colour = mix(colour, frame.fog.rgb, clamp(fog_amount, 0.0, 1.0));

    return vec4<f32>(colour, 1.0);
}
