// node.euler_step_particles_3d — apply a per-particle 3D force to each
// live particle's position via one Euler step.
//
//   position.xyz += forces[i] * speed * (dt * 60)
//
// The 3D sibling of node.euler_step_particles. Frame-rate-normalised via
// the `* 60` scale so the same `speed` value gives consistent motion
// across frame rates (matches the legacy fluid_simulate_3d's
// `dt_scale = dt * 60`). Dead particles (life <= 0) pass through
// unchanged. No boundary handling — pair with node.container_bounds_3d.

struct Uniforms {
    active_count: u32,
    speed: f32,
    dt_scaled: f32,
    _pad: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

struct ForceVec {
    x: f32,
    y: f32,
    z: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> forces: array<ForceVec>;

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }
    let f = forces[i];
    let force = vec3<f32>(f.x, f.y, f.z);
    p.position = p.position + force * u.speed * u.dt_scaled;
    particles[i] = p;
}
