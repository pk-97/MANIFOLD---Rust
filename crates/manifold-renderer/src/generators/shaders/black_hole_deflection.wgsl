// Black Hole — Volumetric Deflection Map
//
// Traces geodesics through a THICK disk slab (not a thin plane).
// Accumulates density as the ray passes through the disk volume.
//
// Disk volume: |y| < thickness(r), where thickness increases with r.
// Density profile: gaussian in y, concentrated at midplane.
//
// Output 1: (final_r, weighted_avg_r, weighted_avg_angle, total_density)
// Output 2: reserved for future use

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

// Disk half-thickness as function of radius
fn disk_height(r: f32) -> f32 {
    let t = clamp((r - u.disk_inner) / (u.disk_outer - u.disk_inner), 0.0, 1.0);
    // Inner edge thin (compressed by gravity), outer edge puffy
    return 0.15 + 0.6 * t;
}

// Density at a point (gaussian in y, smooth radial profile)
fn disk_density(r: f32, y: f32) -> f32 {
    if r < u.disk_inner * 0.7 || r > u.disk_outer * 1.3 {
        return 0.0;
    }

    let h = disk_height(r);
    // Gaussian vertical profile
    let y_density = exp(-(y * y) / (h * h));

    // Soft radial edges
    let inner_fade = smoothstep(u.disk_inner * 0.7, u.disk_inner * 1.05, r);
    let outer_fade = 1.0 - smoothstep(u.disk_outer * 0.9, u.disk_outer * 1.3, r);

    // Radial density: denser near inner edge (matter piles up)
    let r_norm = (r - u.disk_inner) / (u.disk_outer - u.disk_inner);
    let radial_density = 1.0 / (0.3 + r_norm * r_norm);

    return y_density * inner_fade * outer_fade * radial_density;
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

    let max_steps = i32(u.steps);
    let escape_r = max(u.cam_dist * 3.0, 150.0);
    let base_step = max(u.cam_dist * 0.02, 0.3);

    var final_r = 0.0;

    // Volumetric accumulation
    var total_density = 0.0;
    var weighted_r = 0.0;
    var weighted_angle = 0.0;
    // For angle averaging, accumulate sin/cos to avoid wrap issues
    var weighted_cos_a = 0.0;
    var weighted_sin_a = 0.0;

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

        let step = base_step * clamp(r * 0.08, 0.005, 1.0);

        // Verlet integration
        let r2 = r * r;
        let r5 = r2 * r2 * r;
        let accel = -1.5 * h2 / r5 * pos;
        vel += accel * step * 0.5;
        pos += vel * step;
        let r_new = length(pos);
        let r2_new = r_new * r_new;
        let r5_new = r2_new * r2_new * r_new;
        vel += -1.5 * h2 / r5_new * pos * step * 0.5;

        // ── Volumetric disk sampling ──
        let r_xz = length(vec2<f32>(pos.x, pos.z));
        let density = disk_density(r_xz, pos.y);

        if density > 0.001 {
            let w = density * step;
            let angle = atan2(pos.z, pos.x);

            total_density += w;
            weighted_r += r_xz * w;
            weighted_cos_a += cos(angle) * w;
            weighted_sin_a += sin(angle) * w;
        }

        final_r = r;
    }

    // Compute weighted averages
    var avg_r = 0.0;
    var avg_angle = 0.0;
    if total_density > 0.001 {
        avg_r = weighted_r / total_density;
        avg_angle = atan2(weighted_sin_a, weighted_cos_a);
        // Normalize density to useful range
        total_density = min(total_density, 8.0);
    }

    textureStore(output1, gid.xy, vec4<f32>(final_r, avg_r, avg_angle, total_density));
    textureStore(output2, gid.xy, vec4<f32>(0.0, 0.0, 0.0, 0.0));
}
