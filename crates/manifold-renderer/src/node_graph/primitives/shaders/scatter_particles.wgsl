// node.scatter_particles — atomic scatter of particles into a u32
// fixed-point accumulator. Phase A.7 of BUFFER_PORT_PLAN.
//
// Two entry points share the layout:
//   clear_main: zero the accumulator before scattering.
//   splat_main: each live particle adds `scaled_energy` to its texel.
//
// The accumulator is `array<atomic<u32>>` indexed by `y * width + x`.
// A separate pass (node.resolve_accumulator) converts the u32 grid
// into a float texture by dividing by FIXED_POINT_SCALE (4096.0).

struct ScatterUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: ScatterUniforms;
@group(0) @binding(1) var<storage, read> in_particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<u32>>;

@compute @workgroup_size(16, 16, 1)
fn clear_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }
    let idx = id.y * params.width + id.x;
    atomicStore(&accum[idx], 0u);
}

@compute @workgroup_size(256, 1, 1)
fn splat_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let p = in_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    // Nearest texel + toroidal wrap.
    let coord = vec2<u32>(
        u32(p.position.x * f32(params.width))  % params.width,
        u32(p.position.y * f32(params.height)) % params.height,
    );

    let idx = coord.y * params.width + coord.x;
    atomicAdd(&accum[idx], params.scaled_energy);
}
