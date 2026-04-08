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

    // Vertical cull. The deflection bake's volumetric integral uses a
    // Gaussian profile with half-thickness 0.12*r — beyond ~2 sigma the
    // contribution is negligible. Without this cull, particles that
    // drift far above or below the plane still splat into their (angle,
    // radius) cell and create phantom radial streaks because they
    // persistently illuminate a single polar column from outside the
    // disk volume.
    let half_thick = 0.24 * r;
    if abs(pos.y) > half_thick {
        return;
    }

    let angle = atan2(pos.z, pos.x) + 3.14159265;
    let angle_norm = angle / 6.28318530;
    let r_norm = (r - params.disk_inner) / (params.disk_outer - params.disk_inner);

    let cx = i32(angle_norm * f32(params.tex_w));
    let cy = i32(r_norm * f32(params.tex_h));

    // 3×3 weighted splat. Without this, single-cell scatter at 800k–2.5M
    // particles across 2M cells produces visible discrete-dot noise on
    // the outer disk where each polar cell maps to multiple screen
    // pixels. The kernel weights sum to 1.0 (center 0.40, edges 0.10
    // each, corners 0.05 each), so total per-particle energy stays
    // unchanged from a single-cell splat.
    let center_e = u32(f32(params.scaled_energy) * 0.40 + 0.5);
    let edge_e   = u32(f32(params.scaled_energy) * 0.10 + 0.5);
    let corner_e = u32(f32(params.scaled_energy) * 0.05 + 0.5);

    let above = pos.y >= 0.0;
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let nx_signed = cx + dx;
            let ny_signed = cy + dy;
            // Wrap angle, clamp radius
            let nx = u32(((nx_signed % i32(params.tex_w)) + i32(params.tex_w))
                         % i32(params.tex_w));
            if ny_signed < 0 || ny_signed >= i32(params.tex_h) {
                continue;
            }
            let ny = u32(ny_signed);
            let idx = ny * params.tex_w + nx;

            var e: u32;
            if dx == 0 && dy == 0 {
                e = center_e;
            } else if dx == 0 || dy == 0 {
                e = edge_e;
            } else {
                e = corner_e;
            }

            // Two-layer polar: split by signed disk-plane height.
            if above {
                atomicAdd(&accum_top[idx], e);
            } else {
                atomicAdd(&accum_bottom[idx], e);
            }
        }
    }
}
