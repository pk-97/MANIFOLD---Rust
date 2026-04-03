// Black Hole — Particle Scatter (splat only)
//
// Splatters particle positions onto a 2D polar-mapped density accumulator.
// Separate file from resolve to avoid naga uniform size mismatch at same binding.

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

    if r < params.disk_inner || r > params.disk_outer {
        return;
    }

    // Polar mapping
    let angle = atan2(pos.z, pos.x) + 3.14159265; // [0, 2π]
    let angle_norm = angle / 6.28318530; // [0, 1]
    let r_norm = (r - params.disk_inner) / (params.disk_outer - params.disk_inner);

    let px = u32(angle_norm * f32(params.tex_w)) % params.tex_w;
    let py = u32(r_norm * f32(params.tex_h)) % params.tex_h;
    let idx = py * params.tex_w + px;

    let brightness = u32(f32(params.scaled_energy) * (1.0 + (1.0 - r_norm) * 2.0));
    atomicAdd(&accum[idx], brightness);

    // 2x2 neighbor smoothing
    let px1 = (px + 1u) % params.tex_w;
    let py1 = min(py + 1u, params.tex_h - 1u);
    atomicAdd(&accum[py * params.tex_w + px1], params.scaled_energy / 2u);
    atomicAdd(&accum[py1 * params.tex_w + px], params.scaled_energy / 2u);
}
