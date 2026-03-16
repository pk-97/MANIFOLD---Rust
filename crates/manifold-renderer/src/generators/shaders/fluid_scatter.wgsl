// Density scatter: atomic splat + resolve for particle-based fluid simulation.
// Two entry points in one file:
//   splat_main: per-particle atomic deposit into accumulator
//   resolve_main: resolve accumulator to density texture + self-clear

struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    splat_size: f32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: SplatUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let p = particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    let coord = vec2<u32>(
        u32(fract(p.position.x + 1.0) * f32(params.width)) % params.width,
        u32(fract(p.position.y + 1.0) * f32(params.height)) % params.height,
    );
    let idx = coord.y * params.width + coord.x;
    let energy = u32(
        0.005 * (params.splat_size / 3.0) * (1000000.0 / f32(params.active_count)) * 4096.0 + 0.5
    );
    atomicAdd(&accum[idx], energy);
}

// ── Resolve pass ──

struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> resolve_params: ResolveUniforms;

@compute @workgroup_size(16, 16, 1)
fn resolve_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= resolve_params.width || id.y >= resolve_params.height {
        return;
    }

    let idx = id.y * resolve_params.width + id.x;
    let val = atomicLoad(&resolve_accum[idx]);
    let density = f32(val) / 4096.0;

    textureStore(density_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(density, 0.0, 0.0, 1.0));

    // Self-clear for next frame
    atomicStore(&resolve_accum[idx], 0u);
}
