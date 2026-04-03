// Black Hole — Deflection Map (dual crossing, cos/sin angle storage)
//
// Traces geodesics, records up to 2 disk crossings per ray.
// Stores cos/sin of angle instead of raw atan2 to avoid seam artifacts.
// Uses fixed generous crossing bounds (1.5–25.0) so disk_inner/disk_outer
// changes don't require rebake — opacity is computed in the display pass.
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
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output1: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var output3: texture_storage_2d<rgba16float, write>;

// Fixed generous crossing bounds — covers full param ranges
// (disk_inner min=2.0, disk_outer max=20.0)
const CROSS_INNER: f32 = 1.5;
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

        if r < 1.0 { final_r = 0.0; absorbed = true; break; }
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
