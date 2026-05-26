// node.sample_texture_at_particles — bilinear sample of a 2D texture
// at each particle's position.xy, write RG into Array<vec2<f32>>.
//
// Pair with node.euler_step_particles + node.wrap_particles_torus to
// reconstruct the legacy `integrate_particles` advection. Reusable
// for any field-driven particle pipeline (velocity, density-weighted
// scaling, per-particle colour LUT lookup, etc).
//
// Output entries for indices ≥ active_count are unwritten — downstream
// consumers must also respect active_count.

struct Uniforms {
    active_count: u32,
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
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var field_tex: texture_2d<f32>;
@group(0) @binding(3) var field_sampler: sampler;
@group(0) @binding(4) var<storage, read_write> out_forces: array<vec2<f32>>;

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    let p = particles[i];
    let uv = vec2<f32>(p.position.x, p.position.y);
    let sample = textureSampleLevel(field_tex, field_sampler, uv, 0.0);
    out_forces[i] = sample.xy;
}
