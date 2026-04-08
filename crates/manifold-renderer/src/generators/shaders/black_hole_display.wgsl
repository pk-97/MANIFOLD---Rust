// Black Hole — Cinematic Display (dual crossing + gravitationally lensed star field)
//
// Deflection map layout:
//   output1: (final_r, disk1_r, cos_angle1, sin_angle1)
//   output2: (vol_accum, disk2_r, cos_angle2, sin_angle2)
//   output3: (sky_dir.xyz, escaped_flag)

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    stars_brightness: f32,
    spin: f32,
    particle_strength: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var deflection1: texture_2d<f32>;
@group(0) @binding(2) var deflection2: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var sky_dir_tex: texture_2d<f32>;
@group(0) @binding(6) var particle_density_top: texture_2d<f32>;
@group(0) @binding(7) var particle_density_bottom: texture_2d<f32>;

// Sample particle density at a disk hit point. Polar coords:
//   X = angle / 2π (wrapped)
//   Y = (r - inner) / (outer - inner) (clamped)
//
// `near_bias` selects which side of the disk plane this hit favors:
//   1.0 = top-biased, 0.0 = bottom-biased. The other side still
//   contributes (at 0.6×) so the disk feels volumetric instead of
//   showing only one face.
//
// The polar texture uses uniform angular bins, so at small radii a
// single cell covers a tiny arc length but maps to a wide screen-
// space wedge. A bright inner cell projects to a long radial streak
// across many screen pixels. To kill the streak artifact we do TWO
// things: (a) area-correct the density so inner cells contribute
// less per pixel, AND (b) at small r, average multiple adjacent
// angular cells so no single cell can dominate a wedge. The
// angular blur radius scales as 1/r — wide at the inner edge,
// vanishing at the outer edge.
fn sample_one(uv: vec2<f32>, near_bias: f32) -> f32 {
    let d_top = textureSampleLevel(particle_density_top, s_linear, uv, 0.0).r;
    let d_bot = textureSampleLevel(particle_density_bottom, s_linear, uv, 0.0).r;
    let near_w = mix(0.6, 1.0, near_bias);
    let far_w  = mix(1.0, 0.6, near_bias);
    return d_top * near_w + d_bot * far_w;
}

fn sample_particle_density(
    disk_r: f32, cos_a: f32, sin_a: f32, near_bias: f32,
) -> f32 {
    let angle = atan2(sin_a, cos_a);
    let ang_norm = fract(angle / 6.28318530 + 1.0);
    let r_norm = clamp(
        (disk_r - u.disk_inner) / (u.disk_outer - u.disk_inner),
        0.0, 1.0,
    );

    // Angular blur radius (in normalized texture coordinates) scales
    // inverse-linearly with r_norm. At the inner edge we average over
    // ~3% of the angular axis (~60 cells at width 2048), at the outer
    // edge it collapses to a single sample.
    let blur_radius = mix(0.030, 0.0, r_norm);

    // 7-tap blur — symmetric, weights normalize automatically.
    var raw = 0.0;
    var wsum = 0.0;
    for (var i: i32 = -3; i <= 3; i = i + 1) {
        let off = f32(i) * (blur_radius / 3.0);
        let w = exp(-f32(i * i) * 0.5);
        let uv = vec2<f32>(fract(ang_norm + off + 1.0), r_norm);
        raw = raw + sample_one(uv, near_bias) * w;
        wsum = wsum + w;
    }
    raw = raw / wsum;

    // Area correction: inner cells are still over-represented per
    // screen pixel even after the blur. Stronger than before — fades
    // toward 0 at the inner edge so the very innermost ring can't
    // produce streaks at all.
    let area_w = smoothstep(0.0, 0.4, r_norm);
    return raw * area_w;
}

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

fn fbm(p: vec2<f32>) -> f32 {
    var val = noise2d(p) * 0.5;
    val += noise2d(p * 2.03 + vec2<f32>(1.7, -1.3)) * 0.25;
    val += noise2d(p * 4.07 + vec2<f32>(3.4, -2.6)) * 0.125;
    return val;
}

// ── Procedural star field ──

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

                // Steeper power law — many faint, very few bright
                let norm_bright = (h - threshold) / (1.0 - threshold);
                let star_intensity = pow(norm_bright, 1.5) * intensity_mult;

                // Tight point spread — sharp pinpoints, not blobs
                let core = exp(-dist2 * 6000.0);
                let halo = exp(-dist2 * 800.0) * norm_bright * norm_bright * 0.06;

                // Desaturated spectral colors — subtle tint, mostly white
                let temp = hash21(neighbor * 3.46 + seed + 27.0);
                var star_col: vec3<f32>;
                if temp > 0.82 {
                    star_col = vec3<f32>(0.88, 0.92, 1.15);  // O/B cool blue tint
                } else if temp > 0.55 {
                    star_col = vec3<f32>(0.97, 0.98, 1.05);  // A/F near-white
                } else if temp > 0.25 {
                    star_col = vec3<f32>(1.0, 0.97, 0.93);   // G solar
                } else {
                    star_col = vec3<f32>(1.05, 0.92, 0.82);  // K/M warm tint
                }

                light += star_col * star_intensity * (core + halo);
            }
        }
    }
    return light;
}

// Faint nebulosity — large-scale dust/gas clouds for depth
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

    // Layer 1: bright stars
    stars += star_layer(theta, phi, 20.0, 0.82, 3.0, 0.0);

    // Layer 2: medium density
    stars += star_layer(theta, phi, 50.0, 0.80, 1.2, 100.0);

    // Layer 3: dense field
    stars += star_layer(theta, phi, 100.0, 0.84, 0.5, 200.0);

    // Layer 4: faint background dust
    stars += star_layer(theta, phi, 180.0, 0.88, 0.15, 300.0);

    // Background nebulosity
    stars += nebula(dir) * brightness;

    return stars * brightness;
}

// ── Accretion disk shading ──

fn disk_opacity_from_r(r: f32) -> f32 {
    // Wide, soft transitions — volumetric torus appearance.
    // Inner edge extends close to horizon for plunging-region glow.
    let inner_fade = smoothstep(u.disk_inner * 0.25, u.disk_inner * 1.3, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.7, u.disk_outer * 1.5, r);
    return inner_fade * outer_fade;
}

fn shade_disk(disk_r: f32, cos_a: f32, sin_a: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);
    let ring_r = t;
    let r_norm = disk_r / u.disk_inner;

    // Reconstruct angle
    let base_angle = atan2(sin_a, cos_a);

    // ── Structure angle: gentle differential shear for noise ──
    let shear = u.time_val * 0.05 / sqrt(r_norm);
    let angle_struct = base_angle + shear + u.orbit_angle;
    let cs = cos(angle_struct);
    let ss = sin(angle_struct);

    // ── Doppler beaming (fixed in camera frame) ──
    let fd_boost = u.spin * 0.3 / (r_norm * r_norm * r_norm);
    let v_orb = 0.45 * inverseSqrt(r_norm) + u.spin * 0.12 / (r_norm * r_norm);
    let raw_doppler = pow(max(1.0 + v_orb * cos(base_angle), 0.05), 3.0);
    let doppler = mix(1.0, raw_doppler, 0.6);

    // ── Temperature gradient (noise-perturbed for organic color variation) ──
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

    // Radial intensity
    let r_falloff = u.disk_glow * 0.5 / (r_norm * r_norm);

    // ── Domain-warped FBM density ──
    let warp_x = noise2d(vec2<f32>(cs * 2.0 + ring_r + 10.0, ss * 2.0 + 20.0)) - 0.5;
    let warp_y = noise2d(vec2<f32>(cs * 2.0 + ring_r + 30.0, ss * 2.0 + 40.0)) - 0.5;
    let wcs = cs + warp_x * 1.5;
    let wss = ss + warp_y * 1.5;

    // Large-scale: spiral arms, hot spots
    let density_large = fbm(vec2<f32>(
        wcs * 4.0 + wss * 3.0 + ring_r * 1.5,
        wss * 3.5 - wcs * 2.0 + 5.0,
    ));
    // Medium-scale: turbulent eddies, dark lanes
    let density_med = fbm(vec2<f32>(
        wcs * 8.0 + wss * 5.0 + ring_r * 2.0 + 15.0,
        wss * 6.0 + wcs * 3.0 + 25.0,
    ));

    let density = density_large * 0.55 + density_med * 0.45;
    let density_mod = smoothstep(0.15, 0.6, density);

    // Inner edge glow — wider spread for volumetric feel
    let inner_glow = exp(-(t * t) * 4.0) * 1.2;

    // Plunging region: gas inside ISCO spiraling into the hole.
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

// ── Main ──

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));

    // ── Gaussian-blurred upscale of all deflection data ──
    // 13×13 Gaussian blur at deflection texel scale, sigma ≈ 3.0. The
    // deflection bake is at eighth resolution, so a single deflection
    // texel covers ~8 full-res pixels — the wider kernel is needed to
    // hide the underlying eighth-res grid. Sigma=3 in deflection-texel
    // units corresponds to ~24 full-res pixels of softening, well
    // above the 8-pixel texel size.
    let dp = 1.0 / vec2<f32>(textureDimensions(deflection1));
    var d1 = vec4<f32>(0.0);
    var d2 = vec4<f32>(0.0);
    var sky = vec4<f32>(0.0);
    var w_total = 0.0;
    for (var dy = -6; dy <= 6; dy++) {
        for (var dx = -6; dx <= 6; dx++) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * dp;
            let dist2 = f32(dx * dx + dy * dy);
            // sigma = 3.0 → divisor = 2σ² = 18
            let w = exp(-dist2 / 18.0);
            d1 += textureSampleLevel(deflection1, s_linear, uv + offset, 0.0) * w;
            d2 += textureSampleLevel(deflection2, s_linear, uv + offset, 0.0) * w;
            sky += textureSampleLevel(sky_dir_tex, s_linear, uv + offset, 0.0) * w;
            w_total += w;
        }
    }
    d1 /= w_total;
    d2 /= w_total;
    sky /= w_total;

    let final_r = d1.r;
    let c1_r = d1.g;
    let c1_ca = d1.b;
    let c1_sa = d1.a;
    let c1_op = disk_opacity_from_r(c1_r);
    let c2_r = d2.g;
    let c2_ca = d2.b;
    let c2_sa = d2.a;

    // ── Star field background ──
    // sky.w is the escaped/absorbed flag from the deflection bake — 1.0
    // for rays that escaped, 0.0 for rays absorbed by the horizon. The
    // 5×5 gaussian above produces fractional values across the shadow
    // boundary; smoothstep gives a soft shadow edge instead of a hard
    // pixelated cutoff. The previous `if sky.w > 0.3` was the source
    // of the "blocky shadow" — the gaussian was working, we were just
    // re-thresholding it back into a binary signal.
    var color = vec3<f32>(0.0);
    let escape_w = smoothstep(0.15, 0.65, sky.w);
    if escape_w > 0.001 {
        color = star_field(normalize(sky.xyz), u.stars_brightness) * escape_w;
    }
    var total_opacity = 0.0;

    // ── First crossing (front disk) ──
    // c1_mag2 = squared magnitude of (cos_a, sin_a) — ~1.0 for a real
    // crossing, 0 for a non-crossing pixel. The 5×5 gaussian blur over
    // the quarter-res deflection bake produces fractional values at
    // boundaries; smoothstep gives a soft silhouette edge so the disk
    // fades into the star field instead of cutting hard.
    let c1_mag2 = c1_ca * c1_ca + c1_sa * c1_sa;
    let c1_visible = smoothstep(0.25, 0.85, c1_mag2);
    let c1_op_eff = c1_op * c1_visible;
    if c1_r > 0.1 && c1_op_eff > 0.001 {
        var disk_col = shade_disk(c1_r, c1_ca, c1_sa, false) * c1_op_eff;
        if u.particle_strength > 0.001 {
            // First crossing is the front disk — bias toward the top layer.
            let d1 = sample_particle_density(c1_r, c1_ca, c1_sa, 1.0);
            disk_col = disk_col * (1.0 + d1 * u.particle_strength * 2.5);
        }
        color = color * (1.0 - c1_op_eff * 0.85) + disk_col;
        total_opacity = c1_op_eff;
    }

    // ── Second crossing (lensed back) ──
    let c2_mag2 = c2_ca * c2_ca + c2_sa * c2_sa;
    let c2_visible = smoothstep(0.25, 0.85, c2_mag2);
    if c2_r > 0.1 && c2_visible > 0.001 {
        let c2_op = disk_opacity_from_r(c2_r) * c2_visible;
        let remaining = max(1.0 - total_opacity * 0.6, 0.0);
        var disk_col = shade_disk(c2_r, c2_ca, c2_sa, true) * c2_op * remaining;
        if u.particle_strength > 0.001 {
            // Second crossing is the lensed back of the disk — bias toward bottom.
            let d2v = sample_particle_density(c2_r, c2_ca, c2_sa, 0.0);
            disk_col = disk_col * (1.0 + d2v * u.particle_strength * 2.5);
        }
        color = color * (1.0 - c2_op * remaining * 0.5) + disk_col;
        total_opacity = clamp(total_opacity + c2_op * 0.5, 0.0, 1.0);
    }

    // ── Photon ring (visible even over disk, softens shadow boundary) ──
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 6.0);
        let ring_vis = ring * mix(0.6, 0.15, clamp(total_opacity, 0.0, 1.0));
        color += vec3<f32>(0.9, 0.85, 0.7) * ring_vis;
    }

    // ── Volumetric emission (path integral through disk atmosphere) ──
    // The deflection bake stores `vol_accum` — a Gaussian-weighted line
    // integral of the disk's vertical thickness profile along the ray.
    // Multiplying by polar particle density at the volumetric radius
    // turns flat clumps into truly volumetric thickness modulation:
    // where particles are clustered, the path integral *and* the
    // azimuthal density both spike, so the disk visibly bulges.
    let vol = d2.r;
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
        var vol_emit = vol_col * vol_opacity * vol_fade * u.disk_glow * 0.15;
        if u.particle_strength > 0.001 && c1_r > 0.1 && c1_mag2 > 0.25 {
            // Sample both layers symmetrically — volumetric emission spans
            // the full disk thickness, so neither side is "near" or "far".
            let dv = sample_particle_density(c1_r, c1_ca, c1_sa, 0.5);
            vol_emit = vol_emit * (1.0 + dv * u.particle_strength * 2.0);
        }
        color += vol_emit;
    }

    // ── Soft knee compression (HDR-preserving) ──
    let peak = max(max(color.r, color.g), max(color.b, 0.001));
    if peak > 0.8 {
        let compressed = 0.8 * (1.0 + log(peak / 0.8));
        color = color * (compressed / peak);
    }

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
