// Black Hole — Schwarzschild geodesic raytracer (3D Cartesian)
//
// All distances in units of Schwarzschild radius (rs = 1).
// Integrates light geodesics in full 3D Cartesian coordinates.
// Accretion disk on the y=0 plane, detected via sign-change crossing.
//
// The effective gravitational acceleration for a photon in Schwarzschild geometry:
//   a = -1.5 * rs * h² / r⁵ * pos
// where h = |pos × vel| is the conserved angular momentum magnitude.

struct Uniforms {
    time_val: f32,
    aspect: f32,
    speed: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    uv_scale: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

// ── Accretion disk color ──
fn disk_color(r: f32, pos: vec3<f32>) -> vec3<f32> {
    let t = (r - u.disk_inner) / (u.disk_outer - u.disk_inner);
    let t_clamped = clamp(t, 0.0, 1.0);

    // Temperature gradient: white-hot inner → orange → deep red outer
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.6, 0.2);
    let outer_col = vec3<f32>(0.7, 0.15, 0.03);

    var col: vec3<f32>;
    if t_clamped < 0.5 {
        col = mix(inner_col, mid_col, t_clamped * 2.0);
    } else {
        col = mix(mid_col, outer_col, (t_clamped - 0.5) * 2.0);
    }

    // Radial intensity falloff (inverse square from inner edge)
    let intensity = u.disk_glow * (u.disk_inner * u.disk_inner) / (r * r);

    // Procedural swirl texture using world-space angle
    let angle = atan2(pos.z, pos.x);
    let noise1 = 0.7 + 0.3 * sin(angle * 8.0 + r * 1.5 - u.time_val * 0.4);
    let noise2 = 0.85 + 0.15 * sin(angle * 20.0 - r * 3.0 + u.time_val * 0.7);

    col *= intensity * noise1 * noise2;
    return col;
}

// ── Star field background ──
fn star_field(dir: vec3<f32>) -> vec3<f32> {
    let p = dir * 400.0;
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    let brightness = h * h * star * 0.4;
    // Slight color variation
    let tint = vec3<f32>(
        0.8 + 0.2 * fract(h * 13.7),
        0.8 + 0.2 * fract(h * 27.3),
        0.9 + 0.1 * fract(h * 41.1),
    );
    return tint * brightness;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    // UV: centered, aspect-corrected, scaled
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // ── Camera setup ──
    let orbit_angle = u.time_val * u.speed * 0.3;
    let cos_tilt = cos(u.tilt_rad);
    let sin_tilt = sin(u.tilt_rad);
    let cos_orbit = cos(orbit_angle);
    let sin_orbit = sin(orbit_angle);

    // Camera position: orbit at cam_dist, tilted above disk plane (y = up)
    let cam_pos = vec3<f32>(
        u.cam_dist * cos_tilt * cos_orbit,
        u.cam_dist * sin_tilt,
        u.cam_dist * cos_tilt * sin_orbit,
    );

    // Look-at matrix (target = origin)
    let fwd = normalize(-cam_pos);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    // Ray direction (FOV ~50°)
    let fov_factor = 1.2;
    let ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    // ── 3D Cartesian geodesic integration ──
    // For a Schwarzschild black hole, the effective acceleration on a photon is:
    //   a = -1.5 * h² / r⁵ * pos
    // where h = |pos × vel| is conserved angular momentum per unit mass,
    // and rs = 1 in our units.
    //
    // This is the standard form from Weiskopf (2000) / James et al. (2015).

    var pos = cam_pos;
    var vel = ray_dir; // c = 1, photon moves at unit speed

    // Angular momentum (conserved quantity)
    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    // Adaptive step size based on distance
    let base_step = 0.15;
    let max_steps = i32(u.steps);

    var color = vec3<f32>(0.0);
    var hit = false;
    var prev_y = pos.y;
    var disk_alpha = 0.0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        // Event horizon check
        if r < 1.0 {
            color = vec3<f32>(0.0);
            hit = true;
            break;
        }

        // Escape check
        if r > 100.0 {
            color += star_field(normalize(vel)) * (1.0 - disk_alpha);
            hit = true;
            break;
        }

        // Adaptive step: smaller near the hole for accuracy
        let step = base_step * clamp(r * 0.1, 0.01, 1.0);

        // ── Verlet / leapfrog integration (symplectic, energy-conserving) ──
        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;

        // Update velocity (half step)
        vel += accel * step * 0.5;
        // Update position
        pos += vel * step;
        // Recompute acceleration at new position
        let r_new = length(pos);
        let r2_new = r_new * r_new;
        let r5_new = r2_new * r2_new * r_new;
        let accel_new = -1.5 * h2 / r5_new * pos;
        // Update velocity (second half step)
        vel += accel_new * step * 0.5;

        // ── Accretion disk crossing (y = 0 plane) ──
        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 {
            // Crossed the disk plane — interpolate exact crossing point
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(cross_pos);

            if cross_r > u.disk_inner && cross_r < u.disk_outer {
                let dc = disk_color(cross_r, cross_pos);

                // Semi-transparent disk: accumulate color, allow rays to pass through
                // Opacity based on proximity to inner edge (denser near BH)
                let opacity = 0.6 + 0.3 * (1.0 - (cross_r - u.disk_inner)
                    / (u.disk_outer - u.disk_inner));
                color += dc * (1.0 - disk_alpha);
                disk_alpha = clamp(disk_alpha + opacity * 0.7, 0.0, 1.0);

                // If disk is nearly opaque, stop tracing
                if disk_alpha > 0.95 {
                    hit = true;
                    break;
                }
            }
        }
        prev_y = cur_y;
    }

    // Rays that didn't terminate — faint background
    if !hit {
        color += star_field(normalize(vel)) * (1.0 - disk_alpha) * 0.5;
    }

    // Photon ring glow (r ≈ 1.5 rs — the photon sphere)
    // Only add if the ray got close but escaped
    let final_r = length(pos);
    if final_r > 1.0 && final_r < 5.0 {
        let ring_glow = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.3;
        color += vec3<f32>(0.7, 0.8, 1.0) * ring_glow * (1.0 - disk_alpha);
    }

    // Tone map (ACES-ish filmic)
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
