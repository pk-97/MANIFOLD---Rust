// Black Hole — Single-pass Verlet integration (50 steps, every frame)
//
// Short geodesic integration per pixel. No deflection map, no rebaking.
// All params instant. 50 steps with aggressive adaptive stepping gives
// good visual quality at ~25M iterations/frame.

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
    steps: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
// Steps controlled by uniform — user trades quality vs perf

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

    // Doppler
    let v_orb = 0.45 * inverseSqrt(r_norm);
    let doppler = pow(max(1.0 + v_orb * cos(angle), 0.05), 3.5);

    // Concentric rings
    let az1 = ca * 0.2 + sa * 0.15;
    let az2 = ca * 0.1 - sa * 0.08;
    let az3 = ca * 0.3 + sa * 0.2;

    let ring1 = noise2d(vec2<f32>(ring_r * 50.0, az1 + 10.0));
    let ring2 = noise2d(vec2<f32>(ring_r * 100.0 + 5.0, az2 + 20.0));
    let ring3 = noise2d(vec2<f32>(ring_r * 25.0, az3));

    let rings = ring1 * 0.4 + ring2 * 0.35 + ring3 * 0.25;
    let ring_mod = smoothstep(0.25, 0.6, rings);

    // Clumps
    let clump1 = smoothstep(0.5, 0.9, noise2d(vec2<f32>(
        ca * 1.5 + sa * 0.8 + ring_r * 3.0,
        sa * 1.2 + ca * 0.5 + 15.0,
    )));
    let clump_brightness = 1.0 + clump1 * 0.6;

    // Inner glow
    let inner_glow = exp(-(t * t) * 6.0) * 0.8;

    var emission = base_col * r_falloff * doppler
        * (ring_mod * 0.6 + 0.4)
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

    // Camera
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

    // Geodesic integration (Verlet, 50 steps with adaptive sizing)
    var pos = cam_pos;
    var vel = ray_dir;
    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    let escape_r = max(u.cam_dist * 3.0, 100.0);
    // Aggressive step size for 50-step budget
    let base_step = max(u.cam_dist * 0.04, 0.5);

    var prev_y = pos.y;
    var color = vec3<f32>(0.0);
    var total_opacity = 0.0;
    var crossing_count = 0;
    var absorbed = false;

    let max_steps = i32(u.steps);
    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        if r < 1.0 {
            absorbed = true;
            break;
        }
        if r > escape_r {
            break;
        }

        // Adaptive: tiny near horizon, large far away
        let step = base_step * clamp(r * 0.1, 0.005, 1.5);

        // Verlet
        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;
        vel += accel * step * 0.5;
        pos += vel * step;
        let r_new = length(pos);
        let r5_new = r_new * r_new * r_new * r_new * r_new;
        vel += -1.5 * h2 / r5_new * pos * step * 0.5;

        // Disk crossing
        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 && crossing_count < 2 {
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(vec2<f32>(cross_pos.x, cross_pos.z));

            if cross_r > u.disk_inner * 0.8 && cross_r < u.disk_outer * 1.2 {
                let angle = atan2(cross_pos.z, cross_pos.x);
                let op = disk_opacity(cross_r);
                let is_secondary = crossing_count > 0;

                if is_secondary {
                    let remaining = max(1.0 - total_opacity * 0.6, 0.0);
                    color += shade_disk(cross_r, angle, true) * op * remaining;
                    total_opacity = clamp(total_opacity + op * 0.5, 0.0, 1.0);
                } else {
                    color += shade_disk(cross_r, angle, false) * op;
                    total_opacity = op;
                }
                crossing_count++;
            }
        }
        prev_y = cur_y;
    }

    // Absorbed
    if absorbed {
        color = vec3<f32>(0.0);
    }

    // Photon ring
    let b = length(cross(cam_pos, ray_dir));
    if b > 2.4 && b < 3.2 && total_opacity < 0.5 && !absorbed {
        let ring = exp(-(b - 2.6) * (b - 2.6) * 30.0) * 0.12;
        color += vec3<f32>(0.8, 0.85, 1.0) * ring;
    }

    // ACES
    let a = 2.51; let b2 = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b2)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
