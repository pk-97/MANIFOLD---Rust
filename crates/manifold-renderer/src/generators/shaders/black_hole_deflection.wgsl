// Black Hole — Kerr Deflection Map
//
// Traces null geodesics in Kerr spacetime (spinning black hole).
// At spin=0, reduces exactly to Schwarzschild.
//
// Physics:
//   - Schwarzschild gravitational acceleration: -1.5 h²/r⁵ pos
//   - Kerr frame-dragging: gravitomagnetic force a/r³ cross(ŷ, vel)
//   - Boyer-Lindquist radius for horizon check (oblate geometry)
//   - Event horizon: r_H = 0.5(1 + √(1 - a²)) in our units (r_s = 1)
//
// Output 1: (final_r, disk1_r, cos_angle1, sin_angle1)
// Output 2: (vol_accum, disk2_r, cos_angle2, sin_angle2)
// Output 3: (sky_dir.xyz, escaped_flag)

struct Uniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    uv_scale: f32,
    spin: f32,
    disk_inner: f32,
    disk_outer: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output1: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var output3: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output1);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // ── Camera physics ──
    // Schwarzschild radius is 1.0 in our units (M = 0.5).
    // Event horizon for Kerr: r_H = M + √(M² - a²M²) = 0.5(1 + √(1 - a²)).
    let a = u.spin;
    let a2 = a * a;
    let r_horizon = 0.5 * (1.0 + sqrt(max(1.0 - a2, 0.0)));

    // Clamp camera distance just outside the horizon — physical observers
    // cannot hover inside the event horizon. A small epsilon prevents the
    // ZAMO factor from going to zero (which would collapse the FOV).
    let safe_cam_dist = max(u.cam_dist, r_horizon + 0.05);

    // ZAMO (Zero Angular Momentum Observer) frame correction.
    // Near the horizon, the proper-frame solid angle subtended by infinity
    // shrinks by ≈ √(1 − r_s/r). The aperture must widen to compensate so
    // that distant features stay roughly the same angular size in the image.
    let metric_factor = sqrt(max(1.0 - 1.0 / safe_cam_dist, 0.05));
    let fov_factor = 1.2 / metric_factor;

    // Gravitational redshift between observer at safe_cam_dist and infinity.
    // Stored multiplied into final_r so the display pass can dim escaping
    // light by the right amount when the camera is deep in the well.
    let redshift_factor = metric_factor;

    let cos_tilt = cos(u.tilt_rad);
    let sin_tilt = sin(u.tilt_rad);
    let cos_rot = cos(u.rotate_rad);
    let sin_rot = sin(u.rotate_rad);

    let cam_pos = vec3<f32>(
        safe_cam_dist * cos_tilt * cos_rot,
        safe_cam_dist * sin_tilt,
        safe_cam_dist * cos_tilt * sin_rot,
    );
    let fwd = normalize(-cam_pos);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    let ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    var pos = cam_pos;
    var vel = ray_dir;
    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    // ── Kerr spin axis (a, a2, r_horizon already computed above) ──
    let spin_axis = vec3<f32>(0.0, 1.0, 0.0);

    // ── Disk region bounds ──
    // Two separate bands: a wide one for crossings, a tight one for volumetrics.
    //
    // Crossings (cross_*) must extend BEYOND the visible disk fade range. The
    // display shader's 5×5 Gaussian blur samples neighboring deflection texels
    // and bilinear-interpolates between "ray crossed at r" and "ray didn't
    // cross" pixels — that interpolation produces fake intermediate c1_r
    // values which would render as half-opaque disk fragments if they fell
    // inside the visible fade range. Keeping the cutoff well past the fade
    // (≥ 25, scaled with disk_outer) ensures every fake intermediate lands at
    // an r where disk_opacity_from_r is already zero, hiding the artifact.
    //
    // Volumetric (vol_*) is gated tight to the user's actual disk range so
    // the orange glow matches the visible disk. vol_accum varies geometrically
    // and continuously with ray geometry (it's a path integral, not a binary
    // event) so the bilinear-blur boundary issue doesn't bite the same way.
    let cross_inner = 0.5;
    let cross_outer = max(25.0, u.disk_outer * 2.0);
    let vol_inner = max(u.disk_inner * 0.25, 0.5);
    let vol_outer = u.disk_outer * 1.5;

    // ── Adaptive step budget by impact parameter ──
    // For unit |vel|, |h| = b (impact parameter). Photon sphere is at b ≈ 2.6.
    //   b > 10 → essentially straight ray, 40 steps is plenty
    //   b > 5  → moderate bending, 80 steps
    //   b > 3  → strong bending, 120 steps
    //   b ≤ 3  → near-photon-sphere, full user-requested step budget
    let b_param = sqrt(h2);
    let user_steps = i32(u.steps);
    var step_budget = user_steps;
    if b_param > 10.0 {
        step_budget = min(40, user_steps);
    } else if b_param > 5.0 {
        step_budget = min(80, user_steps);
    } else if b_param > 3.0 {
        step_budget = min(120, user_steps);
    }
    let max_steps = step_budget;

    let escape_r = max(safe_cam_dist * 3.0, 40.0);
    let base_step = max(safe_cam_dist * 0.02, 0.15);

    var prev_y = pos.y;
    var final_r = 0.0;
    var absorbed = false;

    var c1_r = 0.0; var c1_ca = 0.0; var c1_sa = 0.0;
    var c2_r = 0.0; var c2_ca = 0.0; var c2_sa = 0.0;
    var crossing_count = 0;
    var vol_accum = 0.0;

    // ── Convergence tracking ──
    // Snapshot the velocity every 8 steps after step 20. If the direction
    // hasn't changed by more than ~0.5° over an interval AND we're past the
    // camera shell, the ray has stopped bending and can be terminated as escaped.
    var vel_prev = vel;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        // Boyer-Lindquist radius (accounts for oblate Kerr horizon geometry)
        let w2 = r * r - a2;
        let r_bl = sqrt(max(0.5 * w2 + sqrt(max(0.25 * w2 * w2
            + a2 * pos.y * pos.y, 0.0)), 1e-8));

        if r_bl < r_horizon { final_r = 0.0; absorbed = true; break; }
        if r > escape_r { final_r = r; break; }
        // Early escape: ray well beyond camera and moving outward — negligible bending
        if r > max(safe_cam_dist * 2.0, 15.0) && dot(pos, vel) > 0.0 {
            final_r = r; break;
        }

        let step = base_step * clamp(r * 0.1, 0.005, 2.0);

        // ── Schwarzschild gravitational acceleration ──
        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel_grav = -1.5 * h2 / r5 * pos;

        // ── Kerr frame-dragging (gravitomagnetic force) ──
        // Photons are dragged in the direction of black hole rotation.
        // Force ∝ a/r³, direction = cross(spin_axis, vel).
        // At a=0 this is zero → identical to Schwarzschild.
        let r3 = r2 * r;
        let accel_drag = a / r3 * cross(spin_axis, vel);

        let accel = accel_grav + accel_drag;

        // Verlet first half-step
        vel += accel * step * 0.5;
        pos += vel * step;

        // Verlet second half-step (recompute at new position)
        let r_new = length(pos);
        let r2_new = r_new * r_new;
        let r5_new = r2_new * r2_new * r_new;
        let accel_grav2 = -1.5 * h2 / r5_new * pos;
        let r3_new = r2_new * r_new;
        let accel_drag2 = a / r3_new * cross(spin_axis, vel);
        vel += (accel_grav2 + accel_drag2) * step * 0.5;

        // ── Volumetric density accumulation ──
        // Gaussian vertical profile: rays near the disk plane accumulate density.
        // Path integral gives volumetric thickness for edge-on viewing. Gated
        // to the user's disk range (vol_inner/vol_outer) so the orange
        // volumetric glow only shows where the disk actually is.
        let disk_r_xz = length(vec2<f32>(pos.x, pos.z));
        if disk_r_xz > vol_inner && disk_r_xz < vol_outer {
            let half_thick = 0.12 * disk_r_xz;
            let y_norm = pos.y / half_thick;
            vol_accum += exp(-y_norm * y_norm * 2.0) * step;
        }

        // ── Disk crossing detection ──
        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 {
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(cross_pos);

            if cross_r > cross_inner && cross_r < cross_outer {
                let angle = atan2(cross_pos.z, cross_pos.x);
                let ca = cos(angle);
                let sa = sin(angle);

                if crossing_count == 0 {
                    c1_r = cross_r; c1_ca = ca; c1_sa = sa;
                } else if crossing_count == 1 {
                    c2_r = cross_r; c2_ca = ca; c2_sa = sa;
                }
                crossing_count++;
            }
        }
        prev_y = cur_y;
        final_r = r;

        // ── Convergence early-out ──
        // Sample velocity direction every 8 steps after step 20. If the ray
        // hasn't bent in this interval and we're past the camera shell, it's
        // going straight — no point integrating further.
        if i >= 20 && (i & 7) == 7 {
            let cos_change = dot(normalize(vel), normalize(vel_prev));
            if cos_change > 0.99996 && r > safe_cam_dist {
                break;
            }
            vel_prev = vel;
        }
    }

    // Apply gravitational redshift to escaping rays — far-field intensity
    // dims by the metric factor when the camera is deep in the well.
    let final_r_redshifted = final_r * redshift_factor;
    textureStore(output1, gid.xy, vec4<f32>(final_r_redshifted, c1_r, c1_ca, c1_sa));
    textureStore(output2, gid.xy, vec4<f32>(vol_accum, c2_r, c2_ca, c2_sa));

    if absorbed {
        textureStore(output3, gid.xy, vec4<f32>(0.0, 0.0, 0.0, 0.0));
    } else {
        let sky_dir = normalize(vel);
        textureStore(output3, gid.xy, vec4<f32>(sky_dir, 1.0));
    }
}
