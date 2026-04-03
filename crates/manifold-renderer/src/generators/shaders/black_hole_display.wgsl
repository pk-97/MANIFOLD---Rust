// Black Hole — Cinematic Display (Interstellar-style)
//
// Key visual elements from reference:
//   - Tight CONCENTRIC rings (not radial), following orbital paths
//   - Smooth luminosity gradient with subtle banding
//   - Doppler beaming (one side brighter)
//   - Soft feathered edges
//   - Dual disk crossings (front + lensed back)

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

// ── Noise ──

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

fn star_field(seed1: f32, seed2: f32) -> vec3<f32> {
    let p = vec3<f32>(seed1 * 400.0, seed2 * 400.0, seed1 * seed2 * 200.0);
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    return vec3<f32>(h * h * star * 0.25) * vec3<f32>(
        0.8 + 0.2 * fract(h * 13.7),
        0.85 + 0.15 * fract(h * 27.3),
        0.9 + 0.1 * fract(h * 41.1),
    );
}

// ── Disk shading ──

fn shade_disk(disk_r: f32, disk_angle: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);

    // ── Temperature gradient (reference: bright white center → orange → dark red) ──
    let inner_col = vec3<f32>(1.0, 0.95, 0.9);
    let mid1_col = vec3<f32>(1.0, 0.7, 0.35);
    let mid2_col = vec3<f32>(0.9, 0.35, 0.08);
    let outer_col = vec3<f32>(0.4, 0.05, 0.0);

    var base_col: vec3<f32>;
    if t < 0.15 {
        base_col = mix(inner_col, mid1_col, t / 0.15);
    } else if t < 0.45 {
        base_col = mix(mid1_col, mid2_col, (t - 0.15) / 0.3);
    } else {
        base_col = mix(mid2_col, outer_col, (t - 0.45) / 0.55);
    }

    // ── Radial intensity: steep inverse-r falloff ──
    let r_norm = disk_r / u.disk_inner;
    let r_falloff = u.disk_glow / (r_norm * r_norm);

    // ── Doppler beaming: (1 + v·cos(θ))³ ──
    let v_orbital = 0.35 * inverseSqrt(r_norm);
    let doppler = pow(1.0 + v_orbital * cos(disk_angle), 3.0);

    // ── Concentric ring structure (KEY: noise mapped along radius, not angle) ──
    // Primary rings: high frequency in r, very low in angle
    let ring_r = (disk_r - u.disk_inner) / disk_range;
    let ring1 = noise2d(vec2<f32>(ring_r * 40.0, disk_angle * 0.3 + 10.0));
    let ring2 = noise2d(vec2<f32>(ring_r * 80.0 + 5.0, disk_angle * 0.15 + 20.0));
    let ring3 = noise2d(vec2<f32>(ring_r * 20.0 - u.time_val * 0.05, disk_angle * 0.5));

    // Combine: mostly concentric bands with subtle azimuthal variation
    let rings = ring1 * 0.5 + ring2 * 0.3 + ring3 * 0.2;
    // Map to luminosity variation (subtle, not splotchy)
    let ring_modulation = 0.6 + 0.4 * smoothstep(0.3, 0.7, rings);

    // ── Fine turbulent wisps (very subtle) ──
    let wisp = noise2d(vec2<f32>(
        disk_angle * 6.0 + u.time_val * 0.1,
        ring_r * 8.0 + u.time_val * 0.03,
    ));
    let wisp_mod = 0.9 + 0.1 * wisp;

    // ── Combine ──
    var emission = base_col * r_falloff * doppler * ring_modulation * wisp_mod;

    if is_secondary {
        emission *= 0.45;
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

    // First crossing (front disk)
    if disk1_r > 0.1 {
        color += shade_disk(disk1_r, disk1_angle, false) * disk1_opacity;
        total_opacity = disk1_opacity;
    }

    // Second crossing (lensed back)
    if disk2_r > 0.1 {
        let remaining = max(1.0 - total_opacity * 0.6, 0.0);
        color += shade_disk(disk2_r, disk2_angle, true) * disk2_opacity * remaining;
        total_opacity = clamp(total_opacity + disk2_opacity * 0.5, 0.0, 1.0);
    }

    // Stars
    if final_r > 1.0 {
        color += star_field(final_r * 0.01, d1.b + uv.x * 50.0)
            * max(1.0 - total_opacity, 0.0);
    }

    // Photon ring
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.2;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring * max(1.0 - total_opacity, 0.0);
    }

    // ACES
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
