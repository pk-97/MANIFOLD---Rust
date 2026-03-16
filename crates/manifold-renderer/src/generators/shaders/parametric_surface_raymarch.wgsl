struct Uniforms {
    time_val: f32,
    speed: f32,
    aspect_ratio: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume_tex: texture_3d<f32>;
@group(0) @binding(2) var volume_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

const HALF_EXTENT: f32 = 4.0;
const MAX_STEPS: i32 = 96;
const SURFACE_THRESH: f32 = 0.02;
const NORMAL_EPS: f32 = 0.005;

// Sample volume texture, converting world-space to [0,1]^3 UVW
fn sample_volume(p: vec3<f32>) -> f32 {
    let uvw = p / (HALF_EXTENT * 2.0 * 0.7) + 0.5;
    if any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0)) {
        return 10.0; // Outside volume
    }
    return textureSampleLevel(volume_tex, volume_sampler, uvw, 0.0).r;
}

// Ray-box intersection for [-extent, +extent]^3
fn ray_box(ro: vec3<f32>, rd: vec3<f32>, extent: f32) -> vec2<f32> {
    let inv_rd = 1.0 / rd;
    let t1 = (-extent - ro) * inv_rd;
    let t2 = (extent - ro) * inv_rd;
    let tmin = min(t1, t2);
    let tmax = max(t1, t2);
    let near = max(max(tmin.x, tmin.y), tmin.z);
    let far = min(min(tmax.x, tmax.y), tmax.z);
    return vec2<f32>(max(near, 0.0), far);
}

// 6-tap central difference normal
fn calc_normal(p: vec3<f32>) -> vec3<f32> {
    let e = NORMAL_EPS;
    let n = vec3<f32>(
        sample_volume(p + vec3<f32>(e, 0.0, 0.0)) - sample_volume(p - vec3<f32>(e, 0.0, 0.0)),
        sample_volume(p + vec3<f32>(0.0, e, 0.0)) - sample_volume(p - vec3<f32>(0.0, e, 0.0)),
        sample_volume(p + vec3<f32>(0.0, 0.0, e)) - sample_volume(p - vec3<f32>(0.0, 0.0, e))
    );
    return normalize(n);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv * 2.0 - 1.0;
    let t = u.time_val * u.speed;

    // Orbiting camera
    let cam_x = cos(t * 0.5) * 6.0;
    let cam_y = sin(t * 0.35) * 2.1;
    let cam_z = sin(t * 0.5) * 6.0;
    let ro = vec3<f32>(cam_x, cam_y, cam_z);

    // Look-at camera
    let target = vec3<f32>(0.0);
    let fwd = normalize(target - ro);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    let rd = normalize(fwd + right * uv.x * u.aspect_ratio * 0.8 + up * uv.y * 0.8);

    // Ray-box intersection
    let extent = HALF_EXTENT * 0.7;
    let box_t = ray_box(ro, rd, extent);

    if box_t.x > box_t.y {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Sphere tracing through volume
    var ray_t = box_t.x;
    var hit = false;
    var hit_pos = vec3<f32>(0.0);

    for (var i = 0; i < MAX_STEPS; i++) {
        let p = ro + rd * ray_t;
        let d = sample_volume(p);

        if abs(d) < SURFACE_THRESH {
            hit = true;
            hit_pos = p;
            break;
        }

        // Step by distance (clamped to avoid overshooting)
        ray_t += max(abs(d) * 0.5, 0.01);
        if ray_t > box_t.y {
            break;
        }
    }

    if !hit {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Lighting
    let n = calc_normal(hit_pos);
    let light_dir = normalize(vec3<f32>(0.5, 0.8, -0.3));
    let diffuse = max(dot(n, light_dir), 0.0);

    // Rim lighting
    let view_dir = normalize(ro - hit_pos);
    let rim = pow(1.0 - max(dot(n, view_dir), 0.0), 3.0);

    let ambient = 0.12;
    let lum = ambient + diffuse * 0.7 + rim * 0.35;

    return vec4<f32>(lum, lum, lum, 1.0);
}
