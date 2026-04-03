// Black Hole — Cinematic Display (dual crossing + gravitationally lensed star field)
//
// Deflection map layout:
//   output1: (final_r, disk1_r, cos_angle1, sin_angle1)
//   output2: (unused, disk2_r, cos_angle2, sin_angle2)
//   output3: (sky_dir.xyz, escaped_flag)

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    stars_brightness: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var deflection1: texture_2d<f32>;
@group(0) @binding(2) var deflection2: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var sky_dir_tex: texture_2d<f32>;

// ── Hash functions ──

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

// ── Procedural star field ──

fn star_layer(theta: f32, phi: f32, scale: f32, threshold: f32,
              intensity_mult: f32, seed: f32) -> vec3<f32> {
    // Map spherical coords to grid. phi/2pi and theta/pi give [0,1] range.
    let uv = vec2<f32>(phi * scale * 0.15915, theta * scale * 0.31831);
    let cell = floor(uv);
    let f = fract(uv);

    var light = vec3<f32>(0.0);

    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let neighbor = cell + vec2<f32>(f32(i), f32(j));
            let h = hash21(neighbor + seed);
            if h > threshold {
                let sx = hash21(neighbor * 1.273 + seed + 7.0);
                let sy = hash21(neighbor * 2.178 + seed + 13.0);
                let d = f - vec2<f32>(f32(i), f32(j)) - vec2<f32>(sx, sy);
                let dist2 = dot(d, d);

                let norm_bright = (h - threshold) / (1.0 - threshold);
                let star_intensity = pow(norm_bright, 0.35) * intensity_mult;

                // Point spread: sharp core + soft halo on bright stars
                let core = exp(-dist2 * 900.0);
                let halo = exp(-dist2 * 90.0) * norm_bright * 0.15;

                // Spectral class color
                let temp = hash21(neighbor * 3.46 + seed + 27.0);
                var star_col: vec3<f32>;
                if temp > 0.82 {
                    star_col = vec3<f32>(0.7, 0.85, 1.4);   // O/B hot blue
                } else if temp > 0.65 {
                    star_col = vec3<f32>(0.95, 0.97, 1.15);  // A white-blue
                } else if temp > 0.4 {
                    star_col = vec3<f32>(1.0, 0.98, 0.9);    // F/G solar
                } else if temp > 0.2 {
                    star_col = vec3<f32>(1.1, 0.88, 0.65);   // K orange
                } else {
                    star_col = vec3<f32>(1.15, 0.7, 0.45);   // M red-orange
                }

                light += star_col * star_intensity * (core + halo);
            }
        }
    }
    return light;
}

fn star_field(dir: vec3<f32>, brightness: f32) -> vec3<f32> {
    if brightness < 0.001 { return vec3<f32>(0.0); }

    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x) + 3.14159265;

    var stars = vec3<f32>(0.0);

    // Layer 1: bright stars
    stars += star_layer(theta, phi, 20.0, 0.85, 2.5, 0.0);

    // Layer 2: medium density
    stars += star_layer(theta, phi, 45.0, 0.85, 1.0, 100.0);

    // Layer 3: dense field
    stars += star_layer(theta, phi, 90.0, 0.88, 0.4, 200.0);

    // Layer 4: very dense faint background
    stars += star_layer(theta, phi, 160.0, 0.90, 0.15, 300.0);

    return stars * brightness;
}

// ── Accretion disk shading ──

fn disk_opacity_from_r(r: f32) -> f32 {
    let inner_fade = smoothstep(u.disk_inner * 0.8, u.disk_inner * 1.1, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.85, u.disk_outer * 1.2, r);
    return inner_fade * outer_fade;
}

fn shade_disk(disk_r: f32, cos_a: f32, sin_a: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);
    let ring_r = t;
    let r_norm = disk_r / u.disk_inner;

    // Reconstruct angle, add rotate offset + orbital animation
    let base_angle = atan2(sin_a, cos_a);

    // Keplerian orbital motion: inner orbits faster
    let orbital_speed = u.time_val * 0.4 * pow(r_norm, -1.5);
    let angle = base_angle + orbital_speed + u.orbit_angle;

    // Seamless angle coordinates for noise
    let ca = cos(angle);
    let sa = sin(angle);

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

    // Radial intensity
    let r_falloff = u.disk_glow * 0.5 / (r_norm * r_norm);

    // Doppler beaming
    let v_orb = 0.45 * inverseSqrt(r_norm);
    let doppler = pow(max(1.0 + v_orb * cos(angle), 0.05), 3.5);

    // Concentric rings (seamless)
    let az1 = ca * 0.2 + sa * 0.15;
    let az2 = ca * 0.1 - sa * 0.08;
    let az3 = ca * 0.3 + sa * 0.2;
    let az4 = ca * 0.05 + sa * 0.04;

    let ring1 = noise2d(vec2<f32>(ring_r * 50.0, az1 + 10.0));
    let ring2 = noise2d(vec2<f32>(ring_r * 100.0 + 5.0, az2 + 20.0));
    let ring3 = noise2d(vec2<f32>(ring_r * 25.0, az3));
    let ring4 = noise2d(vec2<f32>(ring_r * 200.0, az4 + 40.0));

    let rings = ring1 * 0.35 + ring2 * 0.25 + ring3 * 0.2 + ring4 * 0.2;
    let ring_mod = smoothstep(0.25, 0.6, rings);

    // Orbiting clumps
    let clump1 = smoothstep(0.5, 0.9, noise2d(vec2<f32>(
        ca * 1.5 + sa * 0.8 + ring_r * 3.0,
        sa * 1.2 + ca * 0.5 + 15.0,
    )));
    let clump2 = smoothstep(0.45, 0.85, noise2d(vec2<f32>(
        ca * 1.2 + ring_r * 4.0 + 30.0,
        sa * 1.0 + 25.0,
    )));
    let clump_brightness = 1.0 + (clump1 + clump2) * 0.5;

    // Turbulent wisps
    let wisp_az = cos(angle * 5.0) + sin(angle * 3.5) * 0.5;
    let wisp = noise2d(vec2<f32>(wisp_az + 30.0, ring_r * 6.0));
    let wisp_mod = 0.75 + 0.25 * wisp;

    // Inner edge glow
    let inner_glow = exp(-(t * t) * 6.0) * 0.8;

    var emission = base_col * r_falloff * doppler
        * (ring_mod * 0.6 + 0.4)
        * wisp_mod
        * clump_brightness
        * (1.0 + inner_glow);

    if is_secondary {
        emission *= 0.4;
    }

    return emission;
}

// ── Main ──

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
    let sky = textureSampleLevel(sky_dir_tex, s_linear, uv, 0.0);

    let final_r = d1.r;
    let c1_r = d1.g;
    let c1_ca = d1.b;
    let c1_sa = d1.a;
    let c1_op = disk_opacity_from_r(c1_r);
    let c2_r = d2.g;
    let c2_ca = d2.b;
    let c2_sa = d2.a;

    // ── Star field background (gravitationally lensed) ──
    var color = vec3<f32>(0.0);
    if sky.w > 0.5 {
        color = star_field(normalize(sky.xyz), u.stars_brightness);
    }
    var total_opacity = 0.0;

    // ── First crossing (front disk) — composited over stars ──
    // Opacity threshold filters half-res interpolation artifacts where
    // "no crossing" (c1_r=0) bleeds into real crossings via bilinear.
    if c1_r > 0.1 && c1_op > 0.02 {
        let disk_col = shade_disk(c1_r, c1_ca, c1_sa, false) * c1_op;
        // Disk gas absorbs background starlight proportional to opacity
        color = color * (1.0 - c1_op * 0.85) + disk_col;
        total_opacity = c1_op;
    }

    // ── Second crossing (lensed back) ──
    if c2_r > 0.1 {
        let c2_op = disk_opacity_from_r(c2_r);
        if c2_op > 0.02 {
            let remaining = max(1.0 - total_opacity * 0.6, 0.0);
            let disk_col = shade_disk(c2_r, c2_ca, c2_sa, true) * c2_op * remaining;
            color = color * (1.0 - c2_op * remaining * 0.5) + disk_col;
            total_opacity = clamp(total_opacity + c2_op * 0.5, 0.0, 1.0);
        }
    }

    // ── Photon ring ──
    if final_r > 1.0 && final_r < 5.0 && total_opacity < 0.5 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.15;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring;
    }

    // ── ACES tonemapping ──
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
