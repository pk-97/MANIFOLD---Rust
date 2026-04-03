// Black Hole — Deflection Map Bake
//
// Traces geodesics once per camera configuration, storing results
// in a deflection texture. This eliminates per-frame ray integration.
//
// Output (Rgba32Float):
//   R: final radius at termination (0 = absorbed by horizon)
//   G: disk crossing radius (0 = no disk hit)
//   B: disk crossing angle (atan2 in world XZ plane)
//   A: accumulated disk opacity (0-1)
//
// The photon acceleration in Schwarzschild geometry (rs = 1, c = 1):
//   a = -1.5 * h² / r⁵ * pos

struct Uniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    uv_scale: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // Camera setup — always baked at orbit_angle=0 (rotational symmetry)
    let cos_tilt = cos(u.tilt_rad);
    let sin_tilt = sin(u.tilt_rad);

    let cam_pos = vec3<f32>(
        u.cam_dist * cos_tilt,
        u.cam_dist * sin_tilt,
        0.0,
    );

    let fwd = normalize(-cam_pos);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    let fov_factor = 1.2;
    let ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    // 3D Cartesian geodesic integration
    var pos = cam_pos;
    var vel = ray_dir;

    let h_vec = cross(pos, vel);
    let h2 = dot(h_vec, h_vec);

    let base_step = 0.15;
    let max_steps = i32(u.steps);

    var prev_y = pos.y;
    var best_disk_r = 0.0;
    var best_disk_angle = 0.0;
    var disk_alpha = 0.0;
    var final_r = 0.0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        // Event horizon
        if r < 1.0 {
            final_r = 0.0;
            break;
        }

        // Escape
        if r > 100.0 {
            final_r = r;
            break;
        }

        let step = base_step * clamp(r * 0.1, 0.01, 1.0);

        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;

        vel += accel * step * 0.5;
        pos += vel * step;

        let r_new = length(pos);
        let r2_new = r_new * r_new;
        let r5_new = r2_new * r2_new * r_new;
        let accel_new = -1.5 * h2 / r5_new * pos;
        vel += accel_new * step * 0.5;

        // Disk crossing
        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 {
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(cross_pos);

            if cross_r > u.disk_inner && cross_r < u.disk_outer {
                let angle = atan2(cross_pos.z, cross_pos.x);
                let opacity = 0.6 + 0.3 * (1.0 - (cross_r - u.disk_inner)
                    / (u.disk_outer - u.disk_inner));

                // Store the strongest (closest to inner) disk hit
                if disk_alpha < 0.01 || cross_r < best_disk_r {
                    best_disk_r = cross_r;
                    best_disk_angle = angle;
                }
                disk_alpha = clamp(disk_alpha + opacity * 0.7, 0.0, 1.0);

                if disk_alpha > 0.95 {
                    final_r = cross_r;
                    break;
                }
            }
        }
        prev_y = cur_y;
        final_r = r;
    }

    textureStore(output, gid.xy, vec4<f32>(final_r, best_disk_r, best_disk_angle, disk_alpha));
}
