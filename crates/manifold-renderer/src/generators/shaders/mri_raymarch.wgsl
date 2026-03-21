// MRI Volume — Raymarch Volume Renderer
// Front-to-back alpha compositing through a 3D volume texture.
// Manual X/Y/Z rotation controls for camera orientation.

struct Uniforms {
    cam_dist: f32,
    rot_x: f32,       // -1..1 → -π..π rotation around X axis (pitch)
    rot_y: f32,       // -1..1 → -π..π rotation around Y axis (yaw)
    rot_z: f32,       // -1..1 → -π..π rotation around Z axis (roll)
    aspect_ratio: f32,
    uv_scale: f32,
    window_center: f32,
    window_width: f32,
    opacity_scale: f32,
    step_count: f32,
    invert: f32,
    _pad: f32,
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

// Ray-AABB intersection for unit cube [0, 1]^3
fn ray_box(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    let inv_rd = 1.0 / rd;
    let t1 = (vec3<f32>(0.0) - ro) * inv_rd;
    let t2 = (vec3<f32>(1.0) - ro) * inv_rd;
    let tmin = min(t1, t2);
    let tmax = max(t1, t2);
    let near = max(max(tmin.x, tmin.y), tmin.z);
    let far = min(min(tmax.x, tmax.y), tmax.z);
    return vec2<f32>(max(near, 0.0), far);
}

// Rotate a vector around the X axis
fn rot_x(v: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a); let s = sin(a);
    return vec3<f32>(v.x, v.y * c - v.z * s, v.y * s + v.z * c);
}

// Rotate a vector around the Y axis
fn rot_y(v: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a); let s = sin(a);
    return vec3<f32>(v.x * c + v.z * s, v.y, -v.x * s + v.z * c);
}

// Rotate a vector around the Z axis
fn rot_z(v: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a); let s = sin(a);
    return vec3<f32>(v.x * c - v.y * s, v.x * s + v.y * c, v.z);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = (in.uv - 0.5) * u.uv_scale;
    uv.x *= u.aspect_ratio;

    // Camera position: start at (0, 0, cam_dist) relative to volume center,
    // then apply Y rotation (yaw), X rotation (pitch)
    let ax = u.rot_x * 3.14159265;
    let ay = u.rot_y * 3.14159265;
    let az = u.rot_z * 3.14159265;

    // Camera starts looking down -Z toward volume center
    var cam_offset = vec3<f32>(0.0, 0.0, u.cam_dist);
    cam_offset = rot_x(cam_offset, ax);
    cam_offset = rot_y(cam_offset, ay);
    let cam_pos = cam_offset + vec3<f32>(0.5, 0.5, 0.5);

    // View direction and basis
    let fwd = normalize(vec3<f32>(0.5, 0.5, 0.5) - cam_pos);
    var world_up = vec3<f32>(0.0, 1.0, 0.0);
    // Apply roll to up vector
    world_up = rot_z(world_up, az);
    // Handle degenerate case when looking straight up/down
    let right = normalize(cross(world_up, fwd));
    let up = cross(fwd, right);
    let rd = normalize(fwd + uv.x * right + uv.y * up);

    // Ray-box intersection with unit cube
    let box_t = ray_box(cam_pos, rd);
    if box_t.x >= box_t.y {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Window/level parameters
    let w_low = u.window_center - u.window_width * 0.5;
    let w_high = u.window_center + u.window_width * 0.5;
    let w_range = max(w_high - w_low, 0.001);

    // Front-to-back alpha compositing
    let ray_len = box_t.y - box_t.x;
    let num_steps = i32(u.step_count);
    let step_size = ray_len / f32(num_steps);

    var accum_color = vec3<f32>(0.0);
    var accum_alpha: f32 = 0.0;

    for (var i = 0; i < num_steps; i = i + 1) {
        let t = box_t.x + (f32(i) + 0.5) * step_size;
        let pos = cam_pos + rd * t;

        // Sample volume
        let raw = textureSampleLevel(volume_tex, volume_sampler, pos, 0.0).r;

        // Apply window/level
        var density = clamp((raw - w_low) / w_range, 0.0, 1.0);
        density = mix(density, 1.0 - density, u.invert);

        // Transfer function: density → opacity + color
        let alpha = density * u.opacity_scale * step_size * 10.0;
        let color = vec3<f32>(density);

        // Front-to-back compositing
        accum_color += (1.0 - accum_alpha) * color * alpha;
        accum_alpha += (1.0 - accum_alpha) * alpha;

        // Early exit
        if accum_alpha > 0.99 {
            break;
        }
    }

    return vec4<f32>(accum_color, 1.0);
}
