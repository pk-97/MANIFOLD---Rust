// Black Hole — Deflection Map Bake (dual crossing)
//
// Traces geodesics, records up to 2 disk crossings per ray.
// Output 1 (first crossing):  R=final_r, G=disk_r, B=disk_angle, A=opacity
// Output 2 (second crossing): R=0,       G=disk_r, B=disk_angle, A=opacity
//
// Photon acceleration: a = -1.5 * h² / r⁵ * pos (rs=1, c=1)

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
@group(0) @binding(1) var output1: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var output2: texture_storage_2d<rgba32float, write>;

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

    let cam_pos = vec3<f32>(u.cam_dist * cos_tilt, u.cam_dist * sin_tilt, 0.0);
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

    let base_step = 0.3;
    let max_steps = i32(u.steps);
    let escape_r = max(u.cam_dist * 3.0, 150.0);

    var prev_y = pos.y;
    var final_r = 0.0;

    // Store up to 2 crossings
    var cross1_r = 0.0;
    var cross1_angle = 0.0;
    var cross1_opacity = 0.0;
    var cross2_r = 0.0;
    var cross2_angle = 0.0;
    var cross2_opacity = 0.0;
    var crossing_count = 0;

    for (var i = 0; i < max_steps; i++) {
        let r = length(pos);

        if r < 1.0 {
            final_r = 0.0;
            break;
        }

        if r > escape_r {
            final_r = r;
            break;
        }

        let step = base_step * clamp(r * 0.05, 0.01, 0.5);

        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;

        vel += accel * step * 0.5;
        pos += vel * step;
        let r_new = length(pos);
        let r2_new = r_new * r_new;
        let r5_new = r2_new * r2_new * r_new;
        vel += -1.5 * h2 / r5_new * pos * step * 0.5;

        let cur_y = pos.y;
        if prev_y * cur_y < 0.0 {
            let frac = abs(prev_y) / (abs(prev_y) + abs(cur_y) + 1e-8);
            let cross_pos = pos - vel * step * (1.0 - frac);
            let cross_r = length(cross_pos);

            if cross_r > u.disk_inner && cross_r < u.disk_outer {
                let angle = atan2(cross_pos.z, cross_pos.x);
                let opacity = 0.5 + 0.4 * (1.0 - (cross_r - u.disk_inner)
                    / (u.disk_outer - u.disk_inner));

                if crossing_count == 0 {
                    cross1_r = cross_r;
                    cross1_angle = angle;
                    cross1_opacity = opacity;
                } else if crossing_count == 1 {
                    cross2_r = cross_r;
                    cross2_angle = angle;
                    cross2_opacity = opacity;
                }
                crossing_count++;
            }
        }
        prev_y = cur_y;
        final_r = r;
    }

    textureStore(output1, gid.xy, vec4<f32>(final_r, cross1_r, cross1_angle, cross1_opacity));
    textureStore(output2, gid.xy, vec4<f32>(0.0, cross2_r, cross2_angle, cross2_opacity));
}
