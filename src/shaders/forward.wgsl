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

// Set 1: the material's base colour map. Draws without one bind a single white
// pixel, so this is always valid and the shader needs no branch.
@group(1) @binding(0) var base_color_map: texture_2d<f32>;
@group(1) @binding(1) var base_color_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) gravity_blend: f32,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) gravity_blend: f32,
) -> VsOut {
    var out: VsOut;
    let world = pc.model * vec4<f32>(pos, 1.0);
    out.world = world.xyz;
    // Uniform scale only, so the model matrix is safe for normals.
    out.normal = (pc.model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = uv;
    out.gravity_blend = gravity_blend;
    out.clip = frame.view_proj * world;
    return out;
}

// Markings for a tube stretch: rings all the way round, because every surface
// is drivable and the player needs to read rotation as well as speed.
fn tube_pattern(uv: vec2<f32>) -> vec3<f32> {
    let ring = fract(uv.x);
    let along = uv.y;

    let centre = 1.0 - smoothstep(0.0, 0.012, abs(ring - 0.5));
    let dash = step(0.5, fract(along * 0.08));
    var glow = vec3<f32>(0.15, 0.85, 1.0) * centre * dash;

    let strip_a = 1.0 - smoothstep(0.0, 0.010, abs(ring - 0.25));
    let strip_b = 1.0 - smoothstep(0.0, 0.010, abs(ring - 0.75));
    glow += vec3<f32>(1.0, 0.35, 0.1) * (strip_a + strip_b);

    let rung = 1.0 - smoothstep(0.0, 0.06, abs(fract(along * 0.04) - 0.5));
    glow += vec3<f32>(0.3, 0.4, 0.9) * rung * 0.35;
    return glow;
}

// Markings for an open stretch: a road along the floor with edge lines where
// the drivable surface runs out, so it reads as a carriageway rather than a
// pipe. Gravity holds the car below those lines.
fn road_pattern(uv: vec2<f32>, n: vec3<f32>) -> vec3<f32> {
    let ring = fract(uv.x);
    let along = uv.y;

    // The floor is where the surface normal points up.
    let floorness = clamp(n.y, 0.0, 1.0);

    let centre = 1.0 - smoothstep(0.0, 0.010, abs(ring - 0.5));
    let dash = step(0.45, fract(along * 0.12));
    var glow = vec3<f32>(1.0, 0.85, 0.3) * centre * dash * floorness;

    // Edge lines mark the limit of the flat road, at the point where the tube
    // wall starts to rise away.
    let edge_a = 1.0 - smoothstep(0.0, 0.014, abs(ring - 0.36));
    let edge_b = 1.0 - smoothstep(0.0, 0.014, abs(ring - 0.64));
    glow += vec3<f32>(1.0, 0.55, 0.15) * (edge_a + edge_b);

    // Chevrons on the banking above the edge lines, warning it is a wall now.
    let banking = smoothstep(0.30, 0.10, abs(ring - 0.5) * -1.0 + 0.5);
    let chevron = step(0.7, fract(along * 0.05 + abs(ring - 0.5) * 2.0));
    glow += vec3<f32>(0.9, 0.2, 0.1) * chevron * banking * 0.25;
    return glow;
}

// Hash for per-prop variation, fed from the vertex's spare slot.
fn hash11(x: f32) -> f32 {
    return fract(sin(x * 127.1 + 311.7) * 43758.5453);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let view_vec = frame.camera_pos.xyz - in.world;
    let dist = length(view_vec);
    let v = view_vec / dist;
    let l = normalize(frame.sun.xyz);

    // Tint multiplies the map, so an untextured draw keeps its flat colour and
    // a textured one is modulated rather than replaced.
    let sampled = textureSample(base_color_map, base_color_sampler, in.uv);
    var albedo = pc.tint.rgb * sampled.rgb;
    var emissive = vec3<f32>(0.0);

    if (pc.params.x > 4.5) {
        // Weapon props: crates, rockets, mines. Lit flatly and mostly emissive,
        // because they have to be picked out against a busy track at speed and
        // read the same whether they are on the floor, the wall or the ceiling.
        let ndl = dot(n, l) * 0.5 + 0.5;
        let pulse = 0.75 + 0.25 * sin(frame.sun.w * 6.0 + pc.params.y * 10.0);
        let rim = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 2.0);
        var colour = albedo * (0.35 + 0.45 * ndl) + albedo * (pulse * 1.6 + rim * 1.2);
        let fade = 1.0 - exp(-dist * frame.fog.a);
        return vec4<f32>(mix(colour, frame.fog.rgb, clamp(fade, 0.0, 1.0)), 1.0);
    }

    if (pc.params.x > 3.5) {
        // Sky dome. Unlit and unfogged: it is the thing the fog fades into, so
        // fogging it would wash the whole backdrop to a flat colour.
        return vec4<f32>(sampled.rgb * pc.tint.rgb, 1.0);
    }

    if (pc.params.x > 2.5) {
        // Background structure. Lit flatly and faded hard into the fog, so the
        // skyline reads as distance rather than competing with the track.
        let seed = in.gravity_blend;
        let tone = 0.35 + 0.30 * hash11(seed);
        albedo = frame.fog.rgb * tone;

        // Window lights, denser toward the base, on a grid that varies per
        // structure. Only on the sides, which is where the uv.x band lands.
        let grid = step(0.55, fract(in.uv.x * (7.0 + floor(hash11(seed + 3.0) * 9.0))))
            * step(0.62, fract(in.uv.y * (13.0 + floor(hash11(seed + 9.0) * 14.0))));
        let lit = step(0.45, hash11(seed + floor(in.uv.y * 20.0) + floor(in.uv.x * 20.0) * 31.0));
        let warm = mix(vec3<f32>(1.0, 0.72, 0.35), vec3<f32>(0.4, 0.85, 1.0), hash11(seed + 5.0));
        emissive += warm * grid * lit * 1.1;

        // A hazard light at the top of the taller things, blinking out of step
        // with its neighbours.
        let beacon = step(0.96, in.uv.y) * step(0.5, fract(frame.sun.w * 0.6 + seed));
        emissive += vec3<f32>(1.4, 0.2, 0.15) * beacon;

        let ndl = dot(n, l) * 0.5 + 0.5;
        var colour = albedo * (0.30 + 0.70 * ndl) + emissive;
        // Denser fog than the track gets, so scenery sits behind it.
        let fade = 1.0 - exp(-dist * frame.fog.a * 1.6);
        return vec4<f32>(mix(colour, frame.fog.rgb, clamp(fade, 0.0, 1.0)), 1.0);
    }

    if (pc.params.x > 1.5) {

        // Boost pad: chevrons racing forward along it, so it reads as a
        // direction to take rather than just a bright patch of floor.
        let travel = fract(in.uv.y * 3.0 - frame.sun.w * 2.5);
        let across = abs(in.uv.x - 0.5) * 2.0;
        let chevron = smoothstep(0.55, 0.95, 1.0 - abs(travel - across * 0.4 - 0.3) * 3.0);
        let rim = smoothstep(0.86, 1.0, across);
        emissive += (vec3<f32>(0.35, 1.5, 2.4) * chevron + vec3<f32>(0.2, 0.9, 1.6) * rim) * 2.0;
        albedo *= 0.35;
    } else if (pc.params.x > 0.5) {
        let blend = clamp(in.gravity_blend, 0.0, 1.0);
        let pattern = mix(road_pattern(in.uv, n), tube_pattern(in.uv), blend);
        emissive += pattern * (1.2 + pc.params.y);
        albedo = mix(albedo, albedo * 0.55, clamp(length(pattern), 0.0, 1.0));
        // Open stretches read warmer, tube stretches colder, so the change in
        // gravity is visible from a distance rather than felt by surprise.
        albedo *= mix(vec3<f32>(1.15, 1.02, 0.85), vec3<f32>(0.9, 0.96, 1.15), blend);
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
