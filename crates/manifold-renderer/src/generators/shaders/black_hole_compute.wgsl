// Black Hole — Single-pass analytic renderer
//
// Uses a precomputed 1D deflection LUT (impact parameter → deflection angle)
// instead of per-pixel geodesic integration. All camera/disk param changes
// are instant — no rebaking.
//
// Per pixel:
//   1. Camera matrix → ray direction → impact parameter b
//   2. Sample LUT at b → total deflection angle δ
//   3. Trace ray analytically: straight line deflected by δ at closest approach
//   4. Check disk plane intersection (y=0) for up to 2 crossings
//   5. Shade with noise/Doppler/rings

struct Uniforms {
    time_val: f32,
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    uv_scale: f32,
    orbit_angle: f32,
    b_min: f32,
    b_max: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> deflection_lut: array<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
const LUT_SIZE: f32 = 512.0;

// ── LUT sampling with linear interpolation ──
fn sample_deflection(b: f32) -> f32 {
    if b <= u.b_min {
        return PI * 10.0; // Captured
    }
    // Inverse of quadratic mapping: t = sqrt((b - b_min) / (b_max - b_min))
    let t = sqrt(clamp((b - u.b_min) / (u.b_max - u.b_min), 0.0, 1.0));
    let idx_f = t * (LUT_SIZE - 1.0);
    let i0 = u32(floor(idx_f));
    let i1 = min(i0 + 1u, u32(LUT_SIZE) - 1u);
    let frac = idx_f - floor(idx_f);
    return mix(deflection_lut[i0], deflection_lut[i1], frac);
}

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

// ── Disk shading ──
fn shade_disk(disk_r: f32, disk_angle: f32, is_secondary: bool) -> vec3<f32> {
    let disk_range = u.disk_outer - u.disk_inner;
    let t = clamp((disk_r - u.disk_inner) / disk_range, 0.0, 1.0);
    let ring_r = t;
    let r_norm = disk_r / u.disk_inner;

    // Keplerian orbital motion
    let orbital_speed = u.time_val * 0.4 * pow(r_norm, -1.5);
    let angle = disk_angle + orbital_speed + u.orbit_angle;

    let ca = cos(angle);
    let sa = sin(angle);

    // Temperature gradient
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

    let r_falloff = u.disk_glow * 0.5 / (r_norm * r_norm);

    // Doppler beaming
    let v_orb = 0.45 * inverseSqrt(r_norm);
    let doppler = pow(max(1.0 + v_orb * cos(angle), 0.05), 3.5);

    // Concentric rings (seamless via cos/sin)
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

    // Wisps
    let wisp_az = cos(angle * 5.0) + sin(angle * 3.5) * 0.5;
    let wisp = noise2d(vec2<f32>(wisp_az + 30.0, ring_r * 6.0));
    let wisp_mod = 0.75 + 0.25 * wisp;

    // Inner glow
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

fn disk_opacity(r: f32) -> f32 {
    let inner_fade = smoothstep(u.disk_inner * 0.8, u.disk_inner * 1.1, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.85, u.disk_outer * 1.2, r);
    return inner_fade * outer_fade;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let pixel_uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (pixel_uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // ── Camera setup ──
    let cos_tilt = cos(u.tilt_rad);
    let sin_tilt = sin(u.tilt_rad);
    let cos_rot = cos(u.rotate_rad);
    let sin_rot = sin(u.rotate_rad);

    let cam_pos = vec3<f32>(
        u.cam_dist * cos_tilt * cos_rot,
        u.cam_dist * sin_tilt,
        u.cam_dist * cos_tilt * sin_rot,
    );

    let fwd = normalize(-cam_pos);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    let fov_factor = 1.2;
    let ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    // ── Compute impact parameter ──
    // b = |cam_pos × ray_dir| (perpendicular distance from ray to origin)
    let cross_v = cross(cam_pos, ray_dir);
    let b = length(cross_v);

    // ── Sample deflection LUT ──
    let deflection = sample_deflection(b);

    // Captured by black hole
    if deflection > PI * 5.0 {
        textureStore(output, gid.xy, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }

    // ── Analytic ray path ──
    // The ray travels in the plane defined by cam_pos and ray_dir.
    // At closest approach, it deflects by `deflection` radians.
    // We trace the deflected ray and check where it crosses y=0 (disk plane).

    // Orbital plane basis vectors
    let radial = normalize(cam_pos);
    let orbit_normal = normalize(cross_v); // Normal to ray's orbital plane
    let tangent = normalize(cross(orbit_normal, radial));

    // Initial angle of ray in orbital plane
    let ray_radial = dot(ray_dir, radial);
    let ray_tangent = dot(ray_dir, tangent);
    let initial_angle = atan2(ray_tangent, -ray_radial); // Angle from inward radial

    // The deflected outgoing ray direction (in orbital plane):
    // After deflection, the ray exits at angle = initial_angle + deflection
    // (measured from the same reference)
    let exit_angle = initial_angle + deflection;
    let exit_dir = -radial * cos(exit_angle) + tangent * sin(exit_angle);

    // ── Disk crossing check ──
    // Find where the ray path crosses y=0 plane.
    // We check both the incoming leg (before closest approach) and outgoing leg.
    var color = vec3<f32>(0.0);
    var total_opacity = 0.0;

    // Incoming leg: from camera toward closest approach
    // Parameterize: pos(t) = cam_pos + ray_dir * t
    // y = 0 → t = -cam_pos.y / ray_dir.y
    if abs(ray_dir.y) > 0.001 {
        let t_hit = -cam_pos.y / ray_dir.y;
        if t_hit > 0.0 {
            let hit_pos = cam_pos + ray_dir * t_hit;
            let hit_r = length(hit_pos);
            // Only count if the ray hasn't reached closest approach yet
            // (hit is between camera and the hole, not behind camera)
            if hit_r > 1.0 {
                let hit_r_xz = length(vec2<f32>(hit_pos.x, hit_pos.z));
                if hit_r_xz > u.disk_inner * 0.8 && hit_r_xz < u.disk_outer * 1.2 {
                    let angle = atan2(hit_pos.z, hit_pos.x);
                    let op = disk_opacity(hit_r_xz);
                    color += shade_disk(hit_r_xz, angle, false) * op;
                    total_opacity = op;
                }
            }
        }
    }

    // Outgoing leg: from closest approach outward along deflected direction
    // The closest approach point is approximately at distance b from origin,
    // in the orbital plane. We trace from there along exit_dir.
    // Approximate closest approach position:
    let closest_pos = (radial * cos(initial_angle + PI * 0.5)
        + tangent * sin(initial_angle + PI * 0.5)) * b;

    if abs(exit_dir.y) > 0.001 {
        let t_hit2 = -closest_pos.y / exit_dir.y;
        if t_hit2 > 0.0 {
            let hit_pos2 = closest_pos + exit_dir * t_hit2;
            let hit_r2_xz = length(vec2<f32>(hit_pos2.x, hit_pos2.z));
            if hit_r2_xz > u.disk_inner * 0.8 && hit_r2_xz < u.disk_outer * 1.2 {
                let angle2 = atan2(hit_pos2.z, hit_pos2.x);
                let op2 = disk_opacity(hit_r2_xz);
                let remaining = max(1.0 - total_opacity * 0.6, 0.0);
                color += shade_disk(hit_r2_xz, angle2, true) * op2 * remaining;
                total_opacity = clamp(total_opacity + op2 * 0.5, 0.0, 1.0);
            }
        }
    }

    // ── Photon ring ──
    if b > u.b_min && b < u.b_min * 1.3 && total_opacity < 0.5 {
        let ring = exp(-(b - u.b_min) * (b - u.b_min) * 20.0) * 0.15;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring;
    }

    // ── ACES tone mapping ──
    let a = 2.51; let b2 = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b2)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
