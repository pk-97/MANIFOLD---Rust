// Black Hole — Cached Display
//
// Reads 4 pre-baked deflection neighbors, bilinearly blends them with
// rotation + zoom UV transforms, then renders the accretion disk and stars
// exactly like the live display shader. Replaces the geodesic compute pass
// entirely — there is no more deflection bake at runtime.
//
// 4 neighbors × 3 textures = 12 input textures, indexed:
//   slot 0 = TL (cam_dist_lo, tilt_lo)
//   slot 1 = TR (cam_dist_lo, tilt_hi)
//   slot 2 = BL (cam_dist_hi, tilt_lo)
//   slot 3 = BR (cam_dist_hi, tilt_hi)
//
// Bake space: 4096×4096 (or whatever the bake used), aspect=1.0, uv_scale=1.0,
// rotate=0. NDC range ±1 in the bake corresponds to ±1 in screen space.
// Aspect, zoom, and rotation are applied here as a UV transform.

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    stars_brightness: f32,
    spin: f32,
    rotate_rad: f32,
    uv_scale: f32,
    aspect: f32,
    tilt_mirror: f32,    // 1.0 normal, -1.0 if tilt > 90°
    w_tl: f32,
    w_tr: f32,
    w_bl: f32,
    w_br: f32,
    /// Half-extent of the screen-space ray field that the bake covers.
    /// Bake screen.x and screen.y both range over [-bake_fov_half, +bake_fov_half].
    /// Display divides the lookup NDC by this so the bake covers the visible
    /// rectangle even at wide aspect ratios and zoomed-out scales.
    bake_fov_half: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tl_defl1: texture_2d<f32>;
@group(0) @binding(2) var tl_defl2: texture_2d<f32>;
@group(0) @binding(3) var tl_sky: texture_2d<f32>;
@group(0) @binding(4) var tr_defl1: texture_2d<f32>;
@group(0) @binding(5) var tr_defl2: texture_2d<f32>;
@group(0) @binding(6) var tr_sky: texture_2d<f32>;
@group(0) @binding(7) var bl_defl1: texture_2d<f32>;
@group(0) @binding(8) var bl_defl2: texture_2d<f32>;
@group(0) @binding(9) var bl_sky: texture_2d<f32>;
@group(0) @binding(10) var br_defl1: texture_2d<f32>;
@group(0) @binding(11) var br_defl2: texture_2d<f32>;
@group(0) @binding(12) var br_sky: texture_2d<f32>;
@group(0) @binding(13) var s_linear: sampler;
@group(0) @binding(14) var output: texture_storage_2d<rgba16float, write>;

// ── Hash functions (identical to live display shader) ──

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

fn fbm(p: vec2<f32>) -> f32 {
    var val = noise2d(p) * 0.5;
    val += noise2d(p * 2.03 + vec2<f32>(1.7, -1.3)) * 0.25;
    val += noise2d(p * 4.07 + vec2<f32>(3.4, -2.6)) * 0.125;
    return val;
}

// ── Procedural star field (identical to live display shader) ──

fn star_layer(theta: f32, phi: f32, scale: f32, threshold: f32,
              intensity_mult: f32, seed: f32) -> vec3<f32> {
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
                let star_intensity = pow(norm_bright, 1.5) * intensity_mult;

                let core = exp(-dist2 * 6000.0);
                let halo = exp(-dist2 * 800.0) * norm_bright * norm_bright * 0.06;

                let temp = hash21(neighbor * 3.46 + seed + 27.0);
                var star_col: vec3<f32>;
                if temp > 0.82 {
                    star_col = vec3<f32>(0.88, 0.92, 1.15);
                } else if temp > 0.55 {
                    star_col = vec3<f32>(0.97, 0.98, 1.05);
                } else if temp > 0.25 {
                    star_col = vec3<f32>(1.0, 0.97, 0.93);
                } else {
                    star_col = vec3<f32>(1.05, 0.92, 0.82);
                }

                light += star_col * star_intensity * (core + halo);
            }
        }
    }
    return light;
}

fn nebula(dir: vec3<f32>) -> vec3<f32> {
    let n1 = fbm(dir.xz * 1.5 + dir.y * 0.5);
    let n2 = noise2d(dir.xz * 3.0 + vec2<f32>(10.0, 20.0));
    let density = max(n1 * 0.7 + n2 * 0.3 - 0.35, 0.0);

    let tint = noise2d(dir.xz * 0.8 + vec2<f32>(50.0, 60.0));
    let warm = vec3<f32>(0.15, 0.06, 0.03);
    let cool = vec3<f32>(0.04, 0.06, 0.12);
    return mix(cool, warm, tint) * density;
}

fn star_field(dir: vec3<f32>, brightness: f32) -> vec3<f32> {
    if brightness < 0.001 { return vec3<f32>(0.0); }

    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x) + 3.14159265;

    var stars = vec3<f32>(0.0);
    stars += star_layer(theta, phi, 20.0, 0.82, 3.0, 0.0);
    stars += star_layer(theta, phi, 50.0, 0.80, 1.2, 100.0);
    stars += star_layer(theta, phi, 100.0, 0.84, 0.5, 200.0);
    stars += star_layer(theta, phi, 180.0, 0.88, 0.15, 300.0);
    stars += nebula(dir) * brightness;

    return stars * brightness;
}

// ── Accretion disk shading (identical to live display shader) ──

fn disk_opacity_from_r(r: f32) -> f32 {
    let inner_fade = smoothstep(u.disk_inner * 0.25, u.disk_inner * 1.3, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.7, u.disk_outer * 1.5, r);
    return inner_fade * outer_fade;
}

fn shade_disk(disk_r: f32, cos_a: f32, sin_a: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);
    let ring_r = t;
    let r_norm = disk_r / u.disk_inner;

    let base_angle = atan2(sin_a, cos_a);
    let shear = u.time_val * 0.05 / sqrt(r_norm);
    let angle_struct = base_angle + shear + u.orbit_angle;
    let cs = cos(angle_struct);
    let ss = sin(angle_struct);

    let fd_boost = u.spin * 0.3 / (r_norm * r_norm * r_norm);
    let v_orb = 0.45 * inverseSqrt(r_norm) + u.spin * 0.12 / (r_norm * r_norm);
    let raw_doppler = pow(max(1.0 + v_orb * cos(base_angle), 0.05), 3.0);
    let doppler = mix(1.0, raw_doppler, 0.6);

    let t_warp = fbm(vec2<f32>(cs * 2.0 + ring_r * 1.5, ss * 1.5 + 50.0));
    let tc = clamp(t + (t_warp - 0.4) * 0.25, 0.0, 1.0);

    let inner_col = vec3<f32>(1.0, 0.95, 0.9);
    let mid1_col = vec3<f32>(1.0, 0.65, 0.3);
    let mid2_col = vec3<f32>(0.85, 0.3, 0.06);
    let outer_col = vec3<f32>(0.35, 0.04, 0.0);

    var base_col: vec3<f32>;
    if tc < 0.15 {
        base_col = mix(inner_col, mid1_col, tc / 0.15);
    } else if tc < 0.45 {
        base_col = mix(mid1_col, mid2_col, (tc - 0.15) / 0.3);
    } else {
        base_col = mix(mid2_col, outer_col, (tc - 0.45) / 0.55);
    }

    let r_falloff = u.disk_glow * 0.5 / (r_norm * r_norm);

    let warp_x = noise2d(vec2<f32>(cs * 2.0 + ring_r + 10.0, ss * 2.0 + 20.0)) - 0.5;
    let warp_y = noise2d(vec2<f32>(cs * 2.0 + ring_r + 30.0, ss * 2.0 + 40.0)) - 0.5;
    let wcs = cs + warp_x * 1.5;
    let wss = ss + warp_y * 1.5;

    let density_large = fbm(vec2<f32>(
        wcs * 4.0 + wss * 3.0 + ring_r * 1.5,
        wss * 3.5 - wcs * 2.0 + 5.0,
    ));
    let density_med = fbm(vec2<f32>(
        wcs * 8.0 + wss * 5.0 + ring_r * 2.0 + 15.0,
        wss * 6.0 + wcs * 3.0 + 25.0,
    ));

    let density = density_large * 0.55 + density_med * 0.45;
    let density_mod = smoothstep(0.15, 0.6, density);

    let inner_glow = exp(-(t * t) * 4.0) * 1.2;

    let plunge = max(1.0 - r_norm, 0.0);
    let redshift = exp(-plunge * 3.0);
    let plunge_col = vec3<f32>(0.8, 0.25, 0.03) * plunge * redshift * 2.0;

    var emission = base_col * r_falloff * doppler
        * (density_mod * 0.75 + 0.25)
        * (1.0 + inner_glow)
        + plunge_col;

    if is_secondary {
        emission *= 0.4;
    }

    return emission;
}

// ── Bilinear blend of 4 neighbor samples ──

fn blend4(
    a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32>,
) -> vec4<f32> {
    return a * u.w_tl + b * u.w_tr + c * u.w_bl + d * u.w_br;
}

// ── Main ──

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let out_uv = (vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));

    // Output pixel → bake-space UV.
    // Bake was rendered with aspect=1.0, rotate=0, and uv_scale=bake_fov_half
    // (so the bake covers screen-space rays in [-bake_fov_half, +bake_fov_half]).
    // We map current params (aspect, uv_scale, rotate) into the same square.
    let ndc = out_uv * 2.0 - 1.0;
    let screen = vec2<f32>(ndc.x * u.aspect * u.uv_scale, ndc.y * u.uv_scale);

    // Apply rotation in screen space, then convert to bake UV.
    let cos_r = cos(u.rotate_rad);
    let sin_r = sin(u.rotate_rad);
    let rotated = vec2<f32>(
        screen.x * cos_r - screen.y * sin_r,
        screen.x * sin_r + screen.y * cos_r,
    );

    // Bake covers screen-space [-bake_fov_half, +bake_fov_half] → texture [0, 1].
    let bake_uv = rotated * (0.5 / u.bake_fov_half) + 0.5;

    // Sample all 4 neighbors with bilinear filtering.
    let d1_tl = textureSampleLevel(tl_defl1, s_linear, bake_uv, 0.0);
    let d1_tr = textureSampleLevel(tr_defl1, s_linear, bake_uv, 0.0);
    let d1_bl = textureSampleLevel(bl_defl1, s_linear, bake_uv, 0.0);
    let d1_br = textureSampleLevel(br_defl1, s_linear, bake_uv, 0.0);

    let d2_tl = textureSampleLevel(tl_defl2, s_linear, bake_uv, 0.0);
    let d2_tr = textureSampleLevel(tr_defl2, s_linear, bake_uv, 0.0);
    let d2_bl = textureSampleLevel(bl_defl2, s_linear, bake_uv, 0.0);
    let d2_br = textureSampleLevel(br_defl2, s_linear, bake_uv, 0.0);

    let sky_tl = textureSampleLevel(tl_sky, s_linear, bake_uv, 0.0);
    let sky_tr = textureSampleLevel(tr_sky, s_linear, bake_uv, 0.0);
    let sky_bl = textureSampleLevel(bl_sky, s_linear, bake_uv, 0.0);
    let sky_br = textureSampleLevel(br_sky, s_linear, bake_uv, 0.0);

    var d1 = blend4(d1_tl, d1_tr, d1_bl, d1_br);
    var d2 = blend4(d2_tl, d2_tr, d2_bl, d2_br);
    var sky = blend4(sky_tl, sky_tr, sky_bl, sky_br);

    // Tilt mirroring: flip Y component of sky direction and sin_angle of crossings.
    if u.tilt_mirror < 0.0 {
        d1 = vec4<f32>(d1.x, d1.y, d1.z, -d1.w);
        d2 = vec4<f32>(d2.x, d2.y, d2.z, -d2.w);
        sky = vec4<f32>(sky.x, -sky.y, sky.z, sky.w);
    }

    // Rotate disk crossing angles (cos/sin pair rotation).
    let c1_ca_raw = d1.z;
    let c1_sa_raw = d1.w;
    let c1_ca = c1_ca_raw * cos_r - c1_sa_raw * sin_r;
    let c1_sa = c1_ca_raw * sin_r + c1_sa_raw * cos_r;

    let c2_ca_raw = d2.z;
    let c2_sa_raw = d2.w;
    let c2_ca = c2_ca_raw * cos_r - c2_sa_raw * sin_r;
    let c2_sa = c2_ca_raw * sin_r + c2_sa_raw * cos_r;

    // Rotate sky direction around the Y axis.
    let sky_x = sky.x * cos_r - sky.z * sin_r;
    let sky_z = sky.x * sin_r + sky.z * cos_r;
    let sky_dir = vec3<f32>(sky_x, sky.y, sky_z);

    let final_r = d1.x;
    let c1_r = d1.y;
    let c1_op = disk_opacity_from_r(c1_r);
    let c2_r = d2.y;

    // ── Star field background ──
    var color = vec3<f32>(0.0);
    if sky.w > 0.3 {
        // Reconstruct unit vector after blending — bilinear blend can shrink it.
        let sky_n = normalize(sky_dir);
        color = star_field(sky_n, u.stars_brightness) * sky.w;
    }
    var total_opacity = 0.0;

    // ── First crossing (front disk) ──
    let c1_mag2 = c1_ca * c1_ca + c1_sa * c1_sa;
    if c1_r > 0.1 && c1_mag2 > 0.25 {
        let inv_mag = inverseSqrt(c1_mag2);
        let disk_col = shade_disk(c1_r, c1_ca * inv_mag, c1_sa * inv_mag, false) * c1_op;
        color = color * (1.0 - c1_op * 0.85) + disk_col;
        total_opacity = c1_op;
    }

    // ── Second crossing (lensed back) ──
    let c2_mag2 = c2_ca * c2_ca + c2_sa * c2_sa;
    if c2_r > 0.1 && c2_mag2 > 0.25 {
        let inv_mag = inverseSqrt(c2_mag2);
        let c2_op = disk_opacity_from_r(c2_r);
        let remaining = max(1.0 - total_opacity * 0.6, 0.0);
        let disk_col = shade_disk(c2_r, c2_ca * inv_mag, c2_sa * inv_mag, true)
            * c2_op * remaining;
        color = color * (1.0 - c2_op * remaining * 0.5) + disk_col;
        total_opacity = clamp(total_opacity + c2_op * 0.5, 0.0, 1.0);
    }

    // ── Photon ring ──
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 6.0);
        let ring_vis = ring * mix(0.6, 0.15, clamp(total_opacity, 0.0, 1.0));
        color += vec3<f32>(0.9, 0.85, 0.7) * ring_vis;
    }

    // ── Volumetric emission ──
    let vol = d2.x;
    if vol > 0.01 {
        let vol_opacity = 1.0 - exp(-vol * 0.3);
        let vol_r = select(c1_r, (u.disk_inner + u.disk_outer) * 0.5, c1_r < 0.1);
        let vol_t = clamp(
            (vol_r - u.disk_inner) / (u.disk_outer - u.disk_inner), 0.0, 1.0);
        let vol_col = mix(
            vec3<f32>(1.0, 0.85, 0.6),
            vec3<f32>(0.5, 0.12, 0.02),
            vol_t,
        );
        let vol_fade = smoothstep(0.0, 0.15, vol);
        color += vol_col * vol_opacity * vol_fade * u.disk_glow * 0.15;
    }

    // ── Soft knee compression ──
    let peak = max(max(color.r, color.g), max(color.b, 0.001));
    if peak > 0.8 {
        let compressed = 0.8 * (1.0 + log(peak / 0.8));
        color = color * (compressed / peak);
    }

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
