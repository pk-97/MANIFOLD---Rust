// node.wrap_particles_torus — per-particle toroidal wrap of
// position.xy to [0, 1]² via `fract(position.xy + 1)`.
//
// The cyclic-boundary policy atom. Dead particles (life <= 0)
// pass through unchanged.

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
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

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
    p.position = vec3<f32>(
        fract(p.position.x + 1.0),
        fract(p.position.y + 1.0),
        0.0,
    );
    particles[i] = p;
}
