// node.euler_step_particles — apply forces[i] × speed × dt_scaled to
// each live particle's position.xy.
//
// `dt_scaled` is `delta * 60` from the host so the same `speed` knob
// produces consistent visual motion across frame rates (matches the
// legacy fluid_simulate's `dt_scale = dt * 60`).
//
// Dead particles (life <= 0) pass through unchanged. Boundary handling
// is a downstream concern (`node.wrap_particles_torus` or future
// `boundary_death`).

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

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> forces: array<vec2<f32>>;

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
    let step = forces[i] * u.speed * u.dt_scaled;
    p.position = vec3<f32>(p.position.x + step.x, p.position.y + step.y, 0.0);
    particles[i] = p;
}
