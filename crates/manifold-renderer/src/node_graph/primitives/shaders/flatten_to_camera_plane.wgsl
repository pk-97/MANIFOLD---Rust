// node.flatten_to_camera_plane — compress particles toward the camera
// viewing plane. The "flatten" depth-collapse used by FluidSim3D to make
// the volume read as a flat sheet facing the camera.
//
// Bit-exact with the flatten step of the legacy fused fluid_simulate_3d:
//
//   if flatten > 0:
//     depth = dot(pos - 0.5, cam_fwd)
//     pos  -= cam_fwd * depth * flatten * 0.1
//
// cam_fwd is the camera's forward vector, supplied CPU-side from the
// wired Camera port.

struct Uniforms {
    active_count: u32,
    flatten: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    if u.flatten <= 0.0 {
        return;
    }
    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let cam_fwd = vec3<f32>(u.cam_fwd_x, u.cam_fwd_y, u.cam_fwd_z);
    let depth_from_center = dot(p.position - 0.5, cam_fwd);
    p.position = p.position - cam_fwd * depth_from_center * u.flatten * 0.1;
    particles[i] = p;
}
