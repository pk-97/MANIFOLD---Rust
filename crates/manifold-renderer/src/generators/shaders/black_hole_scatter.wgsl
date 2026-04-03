// Black Hole — Particle Scatter to Disk Density
//
// Splatters particle positions onto a 2D polar-mapped density texture
// representing the accretion disk (top-down view, y=0 plane).
//
// The density texture is indexed as:
//   X axis = angle (0..2π mapped to 0..width)
//   Y axis = radius (disk_inner..disk_outer mapped to 0..height)
//
// Uses atomic fixed-point accumulation, same pattern as fluid scatter.

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};

struct ScatterUniforms {
    active_count: u32,
    tex_w: u32,
    tex_h: u32,
    scaled_energy: u32,
    disk_inner: f32,
    disk_outer: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<storage, read> particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: ScatterUniforms;

@compute @workgroup_size(256)
fn splat(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.active_count {
        return;
    }

    let p = particles[gid.x];
    if p.life < 0.5 {
        return;
    }

    let pos = p.position;
    let r = length(vec2<f32>(pos.x, pos.z));

    // Only splat particles within disk bounds
    if r < params.disk_inner || r > params.disk_outer {
        return;
    }

    // Polar mapping
    let angle = atan2(pos.z, pos.x) + 3.14159265; // [0, 2π]
    let angle_norm = angle / 6.28318530; // [0, 1]
    let r_norm = (r - params.disk_inner) / (params.disk_outer - params.disk_inner); // [0, 1]

    let px = u32(angle_norm * f32(params.tex_w)) % params.tex_w;
    let py = u32(r_norm * f32(params.tex_h)) % params.tex_h;
    let idx = py * params.tex_w + px;

    // Accumulate with color-weighted energy (brighter near inner edge)
    let brightness = u32(f32(params.scaled_energy) * (1.0 + (1.0 - r_norm) * 2.0));
    atomicAdd(&accum[idx], brightness);

    // Also splat to neighbors for smoothing (2x2 kernel)
    let px1 = (px + 1u) % params.tex_w;
    let py1 = min(py + 1u, params.tex_h - 1u);
    atomicAdd(&accum[py * params.tex_w + px1], params.scaled_energy / 2u);
    atomicAdd(&accum[py1 * params.tex_w + px], params.scaled_energy / 2u);
}

// ── Resolve: atomic accumulator → RGBA texture + self-clear ──

struct ResolveUniforms {
    tex_w: u32,
    tex_h: u32,
    disk_inner: f32,
    disk_outer: f32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var disk_density: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> resolve_params: ResolveUniforms;

@compute @workgroup_size(16, 16)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= resolve_params.tex_w || gid.y >= resolve_params.tex_h {
        return;
    }

    let idx = gid.y * resolve_params.tex_w + gid.x;
    let raw = atomicLoad(&resolve_accum[idx]);
    let density = f32(raw) / 4096.0;

    // Color based on radial position (Y axis = radius)
    let r_norm = f32(gid.y) / f32(resolve_params.tex_h);
    let r = resolve_params.disk_inner + r_norm
        * (resolve_params.disk_outer - resolve_params.disk_inner);

    // Temperature gradient
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.55, 0.15);
    let outer_col = vec3<f32>(0.6, 0.12, 0.02);
    var col: vec3<f32>;
    if r_norm < 0.5 {
        col = mix(inner_col, mid_col, r_norm * 2.0);
    } else {
        col = mix(mid_col, outer_col, (r_norm - 0.5) * 2.0);
    }

    // Apply density as emission intensity
    let intensity = density * (1.0 + (1.0 - r_norm) * 3.0);
    col *= intensity;

    textureStore(disk_density, gid.xy, vec4<f32>(col, density));

    // Self-clear
    atomicStore(&resolve_accum[idx], 0u);
}
