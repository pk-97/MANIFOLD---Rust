// node.scatter_particles — atomic scatter of particles into a u32
// fixed-point accumulator. Phase A.7 of BUFFER_PORT_PLAN.
//
// Single entry point `splat_main`: each live particle adds
// `scaled_energy` to its texel via atomicAdd. The downstream
// node.resolve_accumulator self-clears the buffer after reading it,
// so this shader doesn't need a pre-clear pass.
//
// The accumulator is `array<atomic<u32>>` indexed by `y * width + x`.
// node.resolve_accumulator converts the u32 grid into a float
// texture by dividing by FIXED_POINT_SCALE (4096.0).
//
// `boundary` selects the out-of-bounds policy:
//   0 = Wrap    — toroidal wrap (`pos % width`); seamless tiling.
//   1 = Discard — drop the particle; no edge-seam artifact for
//                 perspective-projected sims (StrangeAttractor).

struct ScatterUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
    boundary: u32,
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

@group(0) @binding(0) var<uniform> params: ScatterUniforms;
@group(0) @binding(1) var<storage, read> in_particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<u32>>;

@compute @workgroup_size(256, 1, 1)
fn splat_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let p = in_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    if params.boundary == 1u {
        // Discard mode: skip out-of-bounds particles entirely.
        if p.position.x < 0.0 || p.position.x >= 1.0 ||
           p.position.y < 0.0 || p.position.y >= 1.0 {
            return;
        }
        let coord = vec2<u32>(
            u32(p.position.x * f32(params.width)),
            u32(p.position.y * f32(params.height)),
        );
        let idx = coord.y * params.width + coord.x;
        atomicAdd(&accum[idx], params.scaled_energy);
    } else {
        // Wrap mode: nearest texel + toroidal wrap.
        let coord = vec2<u32>(
            u32(p.position.x * f32(params.width))  % params.width,
            u32(p.position.y * f32(params.height)) % params.height,
        );
        let idx = coord.y * params.width + coord.x;
        atomicAdd(&accum[idx], params.scaled_energy);
    }
}
