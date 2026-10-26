// Foam cover: per lattice node, the fraction of the water surface foam covers.
//
// Each live foam particle is a disc of area pi r^2 on the surface. Discs placed
// independently cover 1 - exp(-tau) of the surface, tau being the expected disc
// area per unit area at the node: strength * sum_k A_k W(|node - x_k|), W the
// 2D cubic-spline kernel (Monaghan 1992) of smoothing length h, the lattice's
// smallest node spacing, reaching 2h.
//
// Three passes in command order: cs_clear zeroes the accumulator, cs_scatter
// adds every particle's kernel-weighted area to the nodes within 2h, cs_resolve
// turns each node's sum into a cover.

struct Params {
    lattice_min: vec3<f32>,
    h: f32,
    spacing: vec3<f32>,
    strength: f32,
    nodes: vec3<u32>,
    particle_capacity: u32,
    node_count: u32,
    pad_a: u32,
    pad_b: u32,
    pad_c: u32,
}

struct Particle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> cover: array<f32>;

// Fixed-point scale of the accumulator (2^20). A disc of a few millimetres on
// centimetre cells adds about 0.04 to a node, and a node's sum saturates the
// cover (1 - exp(-sum)) long before 4096 (the u32 range at this scale).
const FOAM_FIXED_SCALE: f32 = 1048576.0;
const PI: f32 = 3.14159265358979;

// The 2D cubic-spline kernel, normalised over a plane: 10 / (7 pi h^2) f(q).
fn kernel(distance: f32, h: f32) -> f32 {
    let q = distance / h;
    var f = 0.0;
    if q < 1.0 {
        f = 1.0 - 1.5 * q * q + 0.75 * q * q * q;
    } else if q < 2.0 {
        let t = 2.0 - q;
        f = 0.25 * t * t * t;
    }
    return 10.0 / (7.0 * PI * h * h) * f;
}

@compute @workgroup_size(64)
fn cs_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.node_count {
        return;
    }
    atomicStore(&accum[gid.x], 0u);
}

@compute @workgroup_size(64)
fn cs_scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.particle_capacity {
        return;
    }
    let p = particles[gid.x];
    let r = p.position_radius.w;
    if r <= 0.0 {
        return;
    }
    let x = p.position_radius.xyz;
    let reach = 2.0 * params.h;
    let top = vec3<f32>(params.nodes) - vec3<f32>(1.0);
    let lo = vec3<i32>(clamp(floor((x - reach - params.lattice_min) / params.spacing), vec3<f32>(0.0), top));
    let hi = vec3<i32>(clamp(ceil((x + reach - params.lattice_min) / params.spacing), vec3<f32>(0.0), top));
    let area = PI * r * r;
    for (var k = lo.z; k <= hi.z; k++) {
        for (var j = lo.y; j <= hi.y; j++) {
            for (var i = lo.x; i <= hi.x; i++) {
                let node = params.lattice_min + params.spacing * vec3<f32>(f32(i), f32(j), f32(k));
                let d = distance(node, x);
                if d >= reach {
                    continue;
                }
                let add = u32(round(area * kernel(d, params.h) * FOAM_FIXED_SCALE));
                let index = ts_grid_index(vec3<i32>(i, j, k), vec3<i32>(params.nodes));
                atomicAdd(&accum[index], add);
            }
        }
    }
}

@compute @workgroup_size(64)
fn cs_resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.node_count {
        return;
    }
    let tau = params.strength * f32(atomicLoad(&accum[gid.x])) / FOAM_FIXED_SCALE;
    cover[gid.x] = 1.0 - exp(-tau);
}
