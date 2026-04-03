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
// Output 2: (unused, disk2_r, cos_angle2, sin_angle2)
// Output 3: (sky_dir.xyz, escaped_flag)

struct Uniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    uv_scale: f32,
    spin: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output1: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba32float, write>;
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

    var pos = cam_pos;
    var vel = ray_dir;
    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    // ── Kerr spin setup ──
    let a = u.spin;
    let a2 = a * a;
    // Event horizon: r_H = M + √(M² - a²M²) where M=0.5 (r_s = 1)
    // = 0.5(1 + √(1 - a²))
    let r_horizon = 0.5 * (1.0 + sqrt(max(1.0 - a2, 0.0)));
    let spin_axis = vec3<f32>(0.0, 1.0, 0.0);

    let max_steps = i32(u.steps);
    let escape_r = max(u.cam_dist * 3.0, 150.0);
    let base_step = max(u.cam_dist * 0.02, 0.3);

    var prev_y = pos.y;
    var final_r = 0.0;
    var absorbed = false;

    var c1_r = 0.0; var c1_ca = 0.0; var c1_sa = 0.0;
    var c2_r = 0.0; var c2_ca = 0.0; var c2_sa = 0.0;
    var crossing_count = 0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        // Boyer-Lindquist radius (accounts for oblate Kerr horizon geometry)
        let w2 = r * r - a2;
        let r_bl = sqrt(max(0.5 * w2 + sqrt(max(0.25 * w2 * w2
            + a2 * pos.y * pos.y, 0.0)), 1e-8));

        if r_bl < r_horizon { final_r = 0.0; absorbed = true; break; }
        if r > escape_r { final_r = r; break; }

        let step = base_step * clamp(r * 0.08, 0.005, 1.0);

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

    textureStore(output1, gid.xy, vec4<f32>(final_r, c1_r, c1_ca, c1_sa));
    textureStore(output2, gid.xy, vec4<f32>(0.0, c2_r, c2_ca, c2_sa));

    if absorbed {
        textureStore(output3, gid.xy, vec4<f32>(0.0, 0.0, 0.0, 0.0));
    } else {
        let sky_dir = normalize(vel);
        textureStore(output3, gid.xy, vec4<f32>(sky_dir, 1.0));
    }
}
