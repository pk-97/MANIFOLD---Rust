// StrangeAttractorScatter — mono atomic scatter + resolve.
//
// Same shape as fluid_scatter.wgsl, but discards out-of-bounds particles
// instead of toroidal-wrapping them. Attractors project to UV space with
// possibly out-of-bounds coordinates (camera zoom, perspective extremes);
// wrapping creates a hard visual seam at the boundary. Discarding gives a
// clean clip with no edge artifact.

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

// ── SplatKernel ──

struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
};

@group(0) @binding(0) var<storage, read> splat_particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> splat_accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> splat_params: SplatUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= splat_params.active_count {
        return;
    }

    let p = splat_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    // Discard out-of-bounds particles (no wrap — see file header)
    if p.position.x < 0.0 || p.position.x >= 1.0 ||
       p.position.y < 0.0 || p.position.y >= 1.0 {
        return;
    }

    let coord = vec2<u32>(
        u32(p.position.x * f32(splat_params.width)),
        u32(p.position.y * f32(splat_params.height)),
    );

    let idx = coord.y * splat_params.width + coord.x;
    atomicAdd(&splat_accum[idx], splat_params.scaled_energy);
}

// ── ResolveKernel ──

struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var resolve_density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> resolve_params: ResolveUniforms;

@compute @workgroup_size(16, 16, 1)
fn resolve_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= resolve_params.width || id.y >= resolve_params.height {
        return;
    }

    let idx = id.y * resolve_params.width + id.x;
    let density = f32(atomicLoad(&resolve_accum[idx])) / 4096.0;

    textureStore(resolve_density_out, vec2<i32>(i32(id.x), i32(id.y)),
        vec4<f32>(density, 1.0, 1.0, 1.0));

    // Self-clearing for next frame
    atomicStore(&resolve_accum[idx], 0u);
}
