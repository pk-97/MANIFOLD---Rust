// node.sample_texture_3d_at_particles — trilinear sample of a vec3
// Texture3D at each particle's position.xyz, write into an
// Array<[f32; 3]> force buffer.
//
// The 3D sibling of node.sample_texture_at_particles. Writes (not adds)
// the sampled RGB so it seeds the per-particle force buffer the
// FluidSim3D integrator chain accumulates into (matches the legacy
// `force = textureSampleLevel(t_field, ...).xyz` first step).
//
// Output entries for indices >= active_count are unwritten — downstream
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

// Packed 3-float force element (stride 12, matches Array<[f32; 3]>).
struct ForceVec {
    x: f32,
    y: f32,
    z: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var field_tex: texture_3d<f32>;
@group(0) @binding(3) var field_sampler: sampler;
@group(0) @binding(4) var<storage, read_write> out_forces: array<ForceVec>;

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    let p = particles[i];
    let sample = textureSampleLevel(field_tex, field_sampler, p.position, 0.0).xyz;
    out_forces[i] = ForceVec(sample.x, sample.y, sample.z);
}
