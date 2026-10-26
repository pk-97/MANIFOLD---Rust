// Spray mist: per lattice node, the extinction coefficient (per metre) of the
// spray's droplets in the air.
//
// Each live spray particle is a droplet of geometric cross-section pi r^2 and
// extinction efficiency Q = 2: droplets far larger than the wavelength remove
// twice their cross-section from a beam, the extinction paradox (van de Hulst
// 1957). Extinction at a node x is strength * sum_k Q pi r_k^2 W3(|x - x_k|),
// W3 the 3D cubic-spline kernel (Monaghan 1992) of smoothing length h, the
// lattice's smallest node spacing, reaching 2h.
//
// Three passes in command order: cs_clear zeroes the accumulator, cs_scatter
// adds every droplet's kernel-weighted extinction area to the nodes within 2h,
// cs_resolve turns each node's sum into the coefficient.

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
@group(0) @binding(3) var<storage, read_write> mist: array<f32>;

const PI: f32 = 3.14159265358979;
// Extinction efficiency of a droplet much larger than the wavelength.
const EXTINCTION_EFFICIENCY: f32 = 2.0;

// The 3D cubic-spline kernel, normalised over space: 1 / (pi h^3) f(q).
fn kernel(distance: f32, h: f32) -> f32 {
    return 1.0 / (PI * h * h * h) * splat_spline(distance / h);
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
    let area = EXTINCTION_EFFICIENCY * PI * r * r;
    for (var k = lo.z; k <= hi.z; k++) {
        for (var j = lo.y; j <= hi.y; j++) {
            for (var i = lo.x; i <= hi.x; i++) {
                let node = params.lattice_min + params.spacing * vec3<f32>(f32(i), f32(j), f32(k));
                let d = distance(node, x);
                if d >= reach {
                    continue;
                }
                let add = u32(round(area * kernel(d, params.h) * SPLAT_FIXED_SCALE));
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
    mist[gid.x] = params.strength * f32(atomicLoad(&accum[gid.x])) / SPLAT_FIXED_SCALE;
}
