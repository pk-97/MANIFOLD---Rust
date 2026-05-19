// node.integrate_particles — advect each live particle by sampling a
// 2D velocity field and stepping its position via direct Euler.
// Phase A.7 of BUFFER_PORT_PLAN.
//
// Read/write particle layout matches `compute_common.rs::Particle`
// (position vec3, velocity vec3, life f32, age f32, color vec4 — 64 bytes).
// Particles with life ≤ 0 pass through unchanged.

struct IntegrateUniforms {
    active_count: u32,
    speed: f32,
    dt: f32,
    _pad: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: IntegrateUniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var velocity_field: texture_2d<f32>;
@group(0) @binding(3) var velocity_sampler: sampler;

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    // Sample the velocity field at the particle's current UV.
    let uv = vec2<f32>(p.position.x, p.position.y);
    let v = textureSampleLevel(velocity_field, velocity_sampler, uv, 0.0).xy;

    // Direct Euler step. Toroidal wrap so particles loop around the
    // boundary instead of escaping the [0,1] domain.
    let step = v * (params.speed * params.dt);
    p.position = vec3<f32>(
        fract(p.position.x + step.x + 1.0),
        fract(p.position.y + step.y + 1.0),
        0.0,
    );
    p.velocity = vec3<f32>(v.x, v.y, 0.0);
    p.age = p.age + params.dt;

    particles[i] = p;
}
