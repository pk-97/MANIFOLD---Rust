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
        let ring_r = (avg_r - u.disk_inner) / disk_range;
        let r_norm = avg_r / u.disk_inner;

        // ── Keplerian orbital velocity: inner orbits faster ──
        // v ∝ r^(-1.5) → angular velocity ∝ r^(-1.5)
        let orbital_speed = u.time_val * 0.4 * pow(r_norm, -1.5);

        // Angle with orbital motion applied
        let orbit_a = avg_angle + orbital_speed;

        // ── Temperature gradient ──
        let inner_col = vec3<f32>(1.0, 0.95, 0.9);
        let mid1_col = vec3<f32>(1.0, 0.65, 0.3);
        let mid2_col = vec3<f32>(0.85, 0.3, 0.06);
        let outer_col = vec3<f32>(0.35, 0.04, 0.0);

        var base_col: vec3<f32>;
        if t < 0.15 {
            base_col = mix(inner_col, mid1_col, t / 0.15);
        } else if t < 0.45 {
            base_col = mix(mid1_col, mid2_col, (t - 0.15) / 0.3);
        } else {
            base_col = mix(mid2_col, outer_col, (t - 0.45) / 0.55);
        }

        // ── Radial intensity (toned down) ──
        let r_falloff = u.disk_glow * 0.4 / (r_norm * r_norm);

        // ── Doppler beaming (uses orbiting angle) ──
        let v_orb = 0.45 * inverseSqrt(r_norm);
        let doppler = pow(max(1.0 + v_orb * cos(orbit_a), 0.05), 3.5);

        // ── Concentric ring structure with orbital motion ──
        let ca = cos(orbit_a);
        let sa = sin(orbit_a);

        let ring1 = noise2d(vec2<f32>(ring_r * 50.0, ca * 0.2 + sa * 0.15 + 10.0));
        let ring2 = noise2d(vec2<f32>(ring_r * 100.0 + 5.0, ca * 0.1 - sa * 0.08 + 20.0));
        let ring3 = noise2d(vec2<f32>(ring_r * 25.0, ca * 0.3 + sa * 0.2));
        let ring4 = noise2d(vec2<f32>(ring_r * 200.0, ca * 0.05 + sa * 0.04 + 40.0));

        let rings = ring1 * 0.35 + ring2 * 0.25 + ring3 * 0.2 + ring4 * 0.2;
        let ring_mod = smoothstep(0.25, 0.6, rings);

        // ── Orbiting clumps — larger structures that visibly sweep around ──
        // 3-5 major clumps at different radii, orbiting at Keplerian rates
        let clump_a = orbit_a; // Already has differential rotation
        let clump1 = smoothstep(0.5, 0.9, noise2d(vec2<f32>(
            cos(clump_a * 2.0) * 1.5 + sin(clump_a * 1.5) * 0.8 + ring_r * 3.0,
            sin(clump_a * 2.0) * 1.2 + cos(clump_a * 3.0) * 0.5 + 15.0,
        )));
        let clump2 = smoothstep(0.45, 0.85, noise2d(vec2<f32>(
            cos(clump_a * 3.0) * 1.2 + ring_r * 4.0 + 30.0,
            sin(clump_a * 2.5) * 1.0 + 25.0,
        )));
        // Combine clumps: bright knots of denser material
        let clump_brightness = 1.0 + (clump1 + clump2) * 0.6;

        // ── Turbulent wisps (orbiting) ──
        let wisp_az = cos(orbit_a * 5.0) + sin(orbit_a * 3.5) * 0.5;
        let wisp = noise2d(vec2<f32>(wisp_az + 30.0, ring_r * 6.0));
        let wisp_mod = 0.75 + 0.25 * wisp;

        // ── Inner edge brightening ──
        let inner_glow = exp(-(t * t) * 6.0) * 0.8;

        // ── Volumetric density → emission (Beer-Lambert) ──
        let vol_emission = 1.0 - exp(-density * 0.8);

        color = base_col * r_falloff * doppler
            * (ring_mod * 0.6 + 0.4)
            * wisp_mod
            * clump_brightness
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
