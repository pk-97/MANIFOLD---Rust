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
    cam_velocity: f32,
    cam_freefall: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output1: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var output3: texture_storage_2d<rgba16float, write>;

// Fixed generous crossing bounds — covers full param ranges
const CROSS_INNER: f32 = 0.5;
const CROSS_OUTER: f32 = 25.0;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output1);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // ── Kerr spin setup (needed up front for camera physics horizon clamp) ──
    let a = u.spin;
    let a2 = a * a;
    // Event horizon: r_H = M + √(M² - a²M²) where M=0.5 (r_s = 1)
    // = 0.5(1 + √(1 - a²))
    let r_horizon = 0.5 * (1.0 + sqrt(max(1.0 - a2, 0.0)));
    let spin_axis = vec3<f32>(0.0, 1.0, 0.0);

    // ── Camera physics ──
    // The camera is allowed to fly through the event horizon for the trippy
    // "fall into a black hole" effect. The clamp here is purely numerical
    // safety to keep ratios finite very near the singularity — physical
    // correctness is not enforced inside the horizon (real observers can't
    // hover there, but this is a visual tool).
    let safe_cam_dist = max(u.cam_dist, 0.05);
    let camera_inside_horizon = safe_cam_dist < r_horizon;

    // ZAMO (Zero Angular Momentum Observer) frame correction.
    // Outside the horizon: the proper-frame solid angle subtended by infinity
    // shrinks by ≈ √(1 − r_s/r), so the aperture widens by 1/that to keep
    // distant features at a consistent angular size as the camera descends.
    // Inside the horizon: 1 − r_s/r is negative, the max() floor keeps the
    // metric factor at its minimum (0.05), giving a fully-saturated wide
    // aperture (~5×) for the "fallen in" view.
    let metric_factor = sqrt(max(1.0 - 1.0 / safe_cam_dist, 0.05));
    let fov_factor = 1.2 / metric_factor;

    // Gravitational redshift between observer at safe_cam_dist and infinity.
    // Stored multiplied into final_r so the display pass can dim escaping
    // light by the correct amount when the camera is deep in the well.
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

    var ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    // ── Relativistic aberration ──
    // Compose ZAMO → freefall-observer frame with a pure SR boost.
    // The observer's velocity is radial-inward (v̂ = fwd). In the
    // moving observer's frame, rays are pulled toward the forward
    // direction, so to convert a moving-frame ray to its ZAMO-frame
    // equivalent we use the INVERSE aberration transform:
    //
    //     cos θ = (cos θ' − β) / (1 − β cos θ')
    //
    // where θ is the angle between the ray and v̂ in the ZAMO frame
    // and θ' is the same angle in the moving frame.
    //
    // Cinematic mode: β = user slider (cam_velocity).
    // Freefall mode: β = √(r_s/r) (radial infall from rest at infinity),
    // clamped to 0.99 to keep the formula well-defined.
    var beta = u.cam_velocity;
    if u.cam_freefall > 0.5 {
        beta = clamp(sqrt(1.0 / safe_cam_dist), 0.0, 0.99);
    }
    if beta > 1e-4 {
        let v_dir = fwd; // radial-inward at cam_pos
        let cos_tp = dot(ray_dir, v_dir);
        let denom = 1.0 - beta * cos_tp;
        let cos_t = (cos_tp - beta) / denom;
        // Perpendicular component scaling: sin θ / sin θ'. Derivable
        // from requiring |ray_dir_static| = 1.
        let sin_tp2 = max(1.0 - cos_tp * cos_tp, 0.0);
        let sin_t2 = max(1.0 - cos_t * cos_t, 0.0);
        let perp_scale = select(
            1.0,
            sqrt(sin_t2 / sin_tp2),
            sin_tp2 > 1e-8,
        );
        let ray_perp = ray_dir - v_dir * cos_tp;
        ray_dir = normalize(v_dir * cos_t + ray_perp * perp_scale);
    }

    var pos = cam_pos;
    var vel = ray_dir;
    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    let max_steps = i32(u.steps);
    let escape_r = max(safe_cam_dist * 3.0, 40.0);
    let base_step = max(safe_cam_dist * 0.02, 0.15);

    var prev_y = pos.y;
    var final_r = 0.0;
    var absorbed = false;

    var c1_r = 0.0; var c1_ca = 0.0; var c1_sa = 0.0;
    var c2_r = 0.0; var c2_ca = 0.0; var c2_sa = 0.0;
    var crossing_count = 0;
    var vol_accum = 0.0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        // Boyer-Lindquist radius (accounts for oblate Kerr horizon geometry)
        let w2 = r * r - a2;
        let r_bl = sqrt(max(0.5 * w2 + sqrt(max(0.25 * w2 * w2
            + a2 * pos.y * pos.y, 0.0)), 1e-8));

        // Singularity check — terminate any ray that reaches the center.
        // This also prevents 1/r⁵ NaNs in the gravitational acceleration.
        if r < 0.1 { final_r = 0.0; absorbed = true; break; }

        // Horizon absorption — only applied when the camera is OUTSIDE the
        // horizon. If the camera is inside, every ray starts inside the
        // horizon by definition, and we don't want them all self-absorbing
        // at step 0; let them fall to the singularity instead.
        if !camera_inside_horizon && r_bl < r_horizon {
            final_r = 0.0; absorbed = true; break;
        }

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
        // Path integral gives volumetric thickness for edge-on viewing.
        let disk_r_xz = length(vec2<f32>(pos.x, pos.z));
        if disk_r_xz > CROSS_INNER && disk_r_xz < CROSS_OUTER {
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

            if cross_r > CROSS_INNER && cross_r < CROSS_OUTER {
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
    }

    // Apply gravitational redshift to the escape radius — the photon ring
    // and far-field intensity dim by metric_factor when the camera is deep
    // in the well.
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
