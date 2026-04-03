// Black Hole — Projected Particle Scatter
//
// Projects 3D particle positions to 2D screen space and splatters
// to a density accumulator at output resolution. Same pattern as
// FluidSimulation3D's projected scatter.

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};

struct ScatterUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    scaled_energy: u32,
    // Camera vectors (precomputed on CPU)
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    _pad0: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad1: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    _pad2: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    fov_factor: f32,
    aspect: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
};

@group(0) @binding(0) var<storage, read> particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: ScatterUniforms;

@compute @workgroup_size(256)
fn splat(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.active_count {
        return;
    }

    let p = particles[gid.x];
    if p.life < 0.5 {
        return;
    }

    // Camera vectors
    let cam_pos = vec3<f32>(params.cam_pos_x, params.cam_pos_y, params.cam_pos_z);
    let cam_fwd = vec3<f32>(params.cam_fwd_x, params.cam_fwd_y, params.cam_fwd_z);
    let cam_right = vec3<f32>(params.cam_right_x, params.cam_right_y, params.cam_right_z);
    let cam_up = vec3<f32>(params.cam_up_x, params.cam_up_y, params.cam_up_z);

    // Project particle to camera space
    let to_particle = p.position - cam_pos;
    let depth = dot(to_particle, cam_fwd);

    // Behind camera
    if depth < 0.1 {
        return;
    }

    // Perspective projection
    let proj_x = dot(to_particle, cam_right) / (depth * params.fov_factor);
    let proj_y = dot(to_particle, cam_up) / (depth * params.fov_factor);

    // NDC to pixel
    let screen_x = (proj_x / params.aspect + 1.0) * 0.5 * f32(params.disp_w);
    let screen_y = (-proj_y + 1.0) * 0.5 * f32(params.disp_h);

    let px = i32(screen_x);
    let py = i32(screen_y);

    if px < 0 || px >= i32(params.disp_w) || py < 0 || py >= i32(params.disp_h) {
        return;
    }

    let idx = u32(py) * params.disp_w + u32(px);

    // Encode color + density into accumulator
    // Use particle color brightness as energy weight
    let luma = 0.299 * p.color.r + 0.587 * p.color.g + 0.114 * p.color.b;
    let energy = u32(f32(params.scaled_energy) * max(luma, 0.1));
    atomicAdd(&accum[idx], energy);
}
