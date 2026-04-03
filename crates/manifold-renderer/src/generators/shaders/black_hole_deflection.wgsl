// Black Hole — Deflection Map (dual crossing, cos/sin angle storage)
//
// Traces geodesics, records up to 2 disk crossings per ray.
// Stores cos/sin of angle instead of raw atan2 to avoid seam artifacts.
//
// Output 1: (final_r, disk1_r, cos_angle1, sin_angle1)
// Output 2: (disk1_opacity, disk2_r, cos_angle2, sin_angle2)
// Remaining: disk2_opacity stored in output2.r as combined with disk1_opacity

struct Uniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    uv_scale: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output1: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba32float, write>;

fn disk_opacity(r: f32) -> f32 {
    let inner_fade = smoothstep(u.disk_inner * 0.8, u.disk_inner * 1.1, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.85, u.disk_outer * 1.2, r);
    return inner_fade * outer_fade;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output1);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // Tilt = elevation above disk plane (0° = edge-on, 90° = face-on from above)
    // Low tilt (10-30°) gives the dramatic Interstellar lensing halos.
    // Rotate = orbit around y-axis (which side of the disk you see)
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

    let max_steps = i32(u.steps);
    let escape_r = max(u.cam_dist * 3.0, 150.0);
    let base_step = max(u.cam_dist * 0.02, 0.3);

    var prev_y = pos.y;
    var final_r = 0.0;

    var c1_r = 0.0; var c1_ca = 0.0; var c1_sa = 0.0; var c1_op = 0.0;
    var c2_r = 0.0; var c2_ca = 0.0; var c2_sa = 0.0; var c2_op = 0.0;
    var crossing_count = 0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        if r < 1.0 { final_r = 0.0; break; }
        if r > escape_r { final_r = r; break; }

        let step = base_step * clamp(r * 0.08, 0.005, 1.0);

        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;
        vel += accel * step * 0.5;
        pos += vel * step;
        let r_new = length(pos);
        let r5_new = r_new * r_new * r_new * r_new * r_new;
        vel += -1.5 * h2 / r5_new * pos * step * 0.5;

        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 {
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(cross_pos);

            if cross_r > u.disk_inner * 0.8 && cross_r < u.disk_outer * 1.2 {
                let angle = atan2(cross_pos.z, cross_pos.x);
                let op = disk_opacity(cross_r);
                let ca = cos(angle);
                let sa = sin(angle);

                if crossing_count == 0 {
                    c1_r = cross_r; c1_ca = ca; c1_sa = sa; c1_op = op;
                } else if crossing_count == 1 {
                    c2_r = cross_r; c2_ca = ca; c2_sa = sa; c2_op = op;
                }
                crossing_count++;
            }
        }
        prev_y = cur_y;
        final_r = r;
    }

    // Pack: output1 = (final_r, disk1_r, cos1, sin1)
    //       output2 = (disk1_opacity | disk2_opacity<<16, disk2_r, cos2, sin2)
    textureStore(output1, gid.xy, vec4<f32>(final_r, c1_r, c1_ca, c1_sa));
    textureStore(output2, gid.xy, vec4<f32>(c1_op, c2_r, c2_ca, c2_sa));
}
