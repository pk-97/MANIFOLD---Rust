// Black Hole — Cinematic Display (Interstellar-style)
//
// Multi-layered disk shading:
//   - Multi-octave noise for turbulent gas streaks
//   - Doppler beaming (approaching side brighter)
//   - Volumetric depth (disk has thickness, density tapers)
//   - Dual disk crossings (front + lensed back)
//   - Temperature gradient (white-hot inner → orange → deep red outer)

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var deflection1: texture_2d<f32>;
@group(0) @binding(2) var deflection2: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;

// ── Noise functions ──

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn noise2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u2 = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), u2.x),
        mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), u2.x),
        u2.y,
    );
}

// Fractal Brownian Motion — turbulent streaky noise
fn fbm(p_in: vec2<f32>, octaves: i32) -> f32 {
    var p = p_in;
    var val = 0.0;
    var amp = 0.5;
    var freq = 1.0;
    for (var i = 0; i < octaves; i++) {
        val += amp * noise2d(p * freq);
        freq *= 2.1;
        amp *= 0.48;
        // Rotate each octave slightly for organic feel
        let c = cos(0.5);
        let s = sin(0.5);
        p = vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);
    }
    return val;
}

// ── Star field ──

fn star_field(seed1: f32, seed2: f32) -> vec3<f32> {
    let p = vec3<f32>(seed1 * 400.0, seed2 * 400.0, seed1 * seed2 * 200.0);
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    return vec3<f32>(h * h * star * 0.3) * vec3<f32>(
        0.8 + 0.2 * fract(h * 13.7),
        0.8 + 0.2 * fract(h * 27.3),
        0.9 + 0.1 * fract(h * 41.1),
    );
}

// ── Disk shading ──

fn shade_disk(disk_r: f32, disk_angle: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);

    // ── Temperature gradient ──
    let inner_col = vec3<f32>(1.0, 0.97, 0.92);  // White-hot
    let mid_col = vec3<f32>(1.0, 0.6, 0.18);      // Orange
    let outer_col = vec3<f32>(0.5, 0.08, 0.01);   // Deep ember

    var base_col: vec3<f32>;
    if t < 0.3 {
        base_col = mix(inner_col, mid_col, t / 0.3);
    } else {
        base_col = mix(mid_col, outer_col, (t - 0.3) / 0.7);
    }

    // ── Radial intensity falloff (inverse r) ──
    let r_falloff = u.disk_glow * u.disk_inner / disk_r;

    // ── Doppler beaming ──
    // Matter orbits counter-clockwise; approaching side is brighter
    // Relativistic beaming ~ (1 + v*cos(angle))^3 simplified
    let v_orbital = 0.4 * sqrt(u.disk_inner / disk_r); // Keplerian, sub-c
    let doppler = pow(1.0 + v_orbital * cos(disk_angle), 3.0);

    // ── Multi-octave turbulent noise ──
    // Streaky in azimuthal direction, layered radially
    let noise_uv = vec2<f32>(
        disk_angle * 3.0 + u.time_val * 0.15,
        (disk_r - u.disk_inner) * 4.0 / disk_range,
    );

    // Primary large-scale gas structure
    let gas_structure = fbm(noise_uv * 2.0, 5);

    // Fine turbulent detail
    let fine_detail = fbm(
        vec2<f32>(disk_angle * 12.0 - u.time_val * 0.3, disk_r * 1.5) + 100.0,
        4,
    );

    // Streaks — elongated in orbital direction
    let streak_uv = vec2<f32>(disk_angle * 20.0 + u.time_val * 0.2, disk_r * 0.8);
    let streaks = smoothstep(0.35, 0.65, noise2d(streak_uv));

    // Combine noise layers
    let gas = gas_structure * 0.6 + fine_detail * 0.25 + streaks * 0.15;
    let density = smoothstep(0.15, 0.85, gas);

    // ── Volumetric depth (disk thickness) ──
    // Inner edge is thinner (compressed by gravity), outer is puffier
    let thickness_factor = 0.7 + 0.3 * t;

    // ── Combine ──
    var emission = base_col * r_falloff * doppler * density * thickness_factor;

    // Secondary (lensed back) crossing is dimmer — light traveled longer path
    if is_secondary {
        emission *= 0.5;
    }

    return emission;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));

    // Sample both deflection maps
    let d1 = textureSampleLevel(deflection1, s_linear, uv, 0.0);
    let d2 = textureSampleLevel(deflection2, s_linear, uv, 0.0);

    let final_r = d1.r;
    let disk1_r = d1.g;
    let disk1_angle = d1.b + u.orbit_angle;
    let disk1_opacity = d1.a;
    let disk2_r = d2.g;
    let disk2_angle = d2.b + u.orbit_angle;
    let disk2_opacity = d2.a;

    var color = vec3<f32>(0.0);
    var total_opacity = 0.0;

    // ── First disk crossing (usually the front of the disk) ──
    if disk1_r > 0.1 {
        let emit = shade_disk(disk1_r, disk1_angle, false);
        color += emit * disk1_opacity;
        total_opacity = disk1_opacity;
    }

    // ── Second disk crossing (lensed back, visible above/below hole) ──
    if disk2_r > 0.1 {
        let emit = shade_disk(disk2_r, disk2_angle, true);
        color += emit * disk2_opacity * (1.0 - total_opacity * 0.5);
        total_opacity = clamp(total_opacity + disk2_opacity * 0.5, 0.0, 1.0);
    }

    // ── Background stars ──
    if final_r > 1.0 {
        color += star_field(final_r * 0.01, d1.b + uv.x * 50.0) * max(1.0 - total_opacity, 0.0);
    }

    // ── Photon ring ──
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.2;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring * max(1.0 - total_opacity, 0.0);
    }

    // ── ACES tone mapping ──
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
