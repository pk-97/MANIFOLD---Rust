// Black Hole — Cinematic Volumetric Display
//
// Reads volumetric density accumulated by the deflection pass.
// Deflection map: (final_r, avg_disk_r, avg_disk_angle, total_density)

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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));

    let d1 = textureSampleLevel(deflection1, s_linear, uv, 0.0);

    let final_r = d1.r;
    let avg_r = d1.g;
    let avg_angle = d1.b + u.orbit_angle;
    let density = d1.a;

    var color = vec3<f32>(0.0);

    if density > 0.01 && avg_r > 0.1 {
        let disk_range = u.disk_outer - u.disk_inner;
        let t = clamp((avg_r - u.disk_inner) / disk_range, 0.0, 1.0);

        // ── Temperature gradient ──
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

        // ── Radial intensity ──
        let r_norm = avg_r / u.disk_inner;
        let r_falloff = u.disk_glow / (r_norm * r_norm);

        // ── Doppler beaming ──
        let v_orbital = 0.5 * inverseSqrt(r_norm);
        let doppler = pow(max(1.0 + v_orbital * cos(avg_angle), 0.05), 4.0);

        // ── Concentric ring structure (seamless angle) ──
        let ring_r = (avg_r - u.disk_inner) / disk_range;
        let ca = cos(avg_angle);
        let sa = sin(avg_angle);

        let ring1 = noise2d(vec2<f32>(ring_r * 50.0, ca * 0.2 + sa * 0.15 + 10.0));
        let ring2 = noise2d(vec2<f32>(ring_r * 100.0 + 5.0, ca * 0.1 - sa * 0.08 + 20.0));
        let ring3 = noise2d(vec2<f32>(ring_r * 25.0 - u.time_val * 0.06, ca * 0.3 + sa * 0.2));
        let ring4 = noise2d(vec2<f32>(ring_r * 200.0 + 2.0, ca * 0.05 + sa * 0.04 + 40.0));

        let rings = ring1 * 0.35 + ring2 * 0.25 + ring3 * 0.2 + ring4 * 0.2;
        let ring_mod = smoothstep(0.25, 0.6, rings);

        // ── Turbulent wisps ──
        let wisp_az = cos(avg_angle * 4.0 + u.time_val * 0.12)
            + sin(avg_angle * 3.0 + u.time_val * 0.1) * 0.5;
        let wisp = noise2d(vec2<f32>(wisp_az + 30.0, ring_r * 6.0 + u.time_val * 0.04));
        let wisp_mod = 0.7 + 0.3 * wisp;

        // ── Inner edge brightening ──
        let inner_glow = exp(-(t * t) * 8.0) * 1.5;

        // ── Volumetric density → emission ──
        // density comes from ray accumulation through thick slab
        // Higher density = more material along line of sight = brighter
        let vol_emission = 1.0 - exp(-density * 1.5);

        color = base_col * r_falloff * doppler
            * (ring_mod * 0.7 + 0.3)
            * wisp_mod
            * (1.0 + inner_glow)
            * vol_emission;
    }

    // Stars
    let star_alpha = max(1.0 - density * 0.5, 0.0);
    if final_r > 1.0 {
        color += star_field(final_r * 0.01, d1.b + uv.x * 50.0) * star_alpha;
    }

    // Photon ring
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.2;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring * star_alpha;
    }

    // ACES
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
