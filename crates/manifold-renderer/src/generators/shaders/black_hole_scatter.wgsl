// Black Hole — Polar Particle Scatter
//
// Splatters particle positions onto a 2D polar-mapped density texture.
//   X axis = angle (0..2π → 0..width)
//   Y axis = radius (disk_inner..disk_outer → 0..height)

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
@group(0) @binding(1) var<storage, read_write> accum_top: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read_write> accum_bottom: array<atomic<u32>>;
@group(0) @binding(3) var<uniform> params: ScatterUniforms;

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

    let angle = atan2(pos.z, pos.x) + 3.14159265;
    let angle_norm = angle / 6.28318530;
    let r_norm = (r - params.disk_inner) / (params.disk_outer - params.disk_inner);

    let px = u32(angle_norm * f32(params.tex_w)) % params.tex_w;
    let py = u32(r_norm * f32(params.tex_h)) % params.tex_h;
    let idx = py * params.tex_w + px;

    // Two-layer polar: split by signed disk-plane height. Particles above the
    // plane go into the top texture, below go into the bottom. The display
    // shader blends them with a bias so each disk crossing favors its own
    // side, giving the disk a sense of thickness.
    if pos.y >= 0.0 {
        atomicAdd(&accum_top[idx], params.scaled_energy);
    } else {
        atomicAdd(&accum_bottom[idx], params.scaled_energy);
    }
}
