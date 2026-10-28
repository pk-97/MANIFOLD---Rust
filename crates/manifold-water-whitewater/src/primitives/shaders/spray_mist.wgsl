// Spray mist: per display node, the extinction coefficient (per metre) of the
// spray's droplets in the air, on the display's world tiles (composed after
// the read side, `manifold_node_engine::scene::world_tiles::READ_SIDE`, and
// the shared splat shape).
//
// Each live spray particle is a droplet of radius r, `params.radius` times the
// particle's fade, of geometric cross-section pi r^2 and
// extinction efficiency Q = 2: droplets far larger than the wavelength remove
// twice their cross-section from a beam, the extinction paradox (van de Hulst
// 1957). Extinction at a node x is strength * sum_k Q pi r_k^2 W3(|x - x_k|),
// W3 the 3D cubic-spline kernel (Monaghan 1992) of smoothing length 2h, twice
// the display cell size, reaching 4h: neighbouring droplets merge into one
// haze instead of a puff each. Node p sits at (p - 1/2) h and is element
// `ts_cell_at(p)` of the pool; a node in a tile the table does not hold takes
// nothing, and the canonical element reads 0.
//
// Three passes in command order: cs_clear zeroes the accumulator, cs_scatter
// adds every droplet's kernel-weighted extinction area to the nodes within 4h,
// cs_resolve turns each node's sum into the coefficient.

struct Params {
    h: f32,
    strength: f32,
    pool_tiles: u32,
    particle_capacity: u32,
    elements: u32,
    radius: f32,
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
@group(0) @binding(2) var<storage, read> tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> mist: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

const PI: f32 = 3.14159265358979;
// Extinction efficiency of a droplet much larger than the wavelength.
const EXTINCTION_EFFICIENCY: f32 = 2.0;
// Past this many cells from the origin a position is no water's: it keeps
// the node coordinate inside an i32.
const REACH_CELLS: f32 = 1.0e9;
// The kernel's smoothing length, in display cells.
const SMOOTHING_CELLS: f32 = 2.0;

// The 3D cubic-spline kernel, normalised over space: 1 / (pi h^3) f(q).
fn kernel(distance: f32, h: f32) -> f32 {
    return 1.0 / (PI * h * h * h) * splat_spline(distance / h);
}

@compute @workgroup_size(64)
fn cs_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.elements {
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
    // The particle's w is its fade, 0 to 1 (0 an empty slot); its size is
    // the node's radius times the fade.
    let fade = p.position_radius.w;
    if fade <= 0.0 {
        return;
    }
    let r = params.radius * fade;
    let x = p.position_radius.xyz;
    let smoothing = SMOOTHING_CELLS * params.h;
    let reach = 2.0 * smoothing;
    // The droplet's node coordinate: node n is at n = x / h + 1/2.
    let q = x / params.h + vec3<f32>(0.5);
    if !all(abs(q) < vec3<f32>(REACH_CELLS)) {
        return;
    }
    let lo = vec3<i32>(floor(q - vec3<f32>(2.0 * SMOOTHING_CELLS)));
    let hi = vec3<i32>(ceil(q + vec3<f32>(2.0 * SMOOTHING_CELLS)));
    ts_home(ts_cell_at(vec3<i32>(floor(q))));
    let area = EXTINCTION_EFFICIENCY * PI * r * r;
    for (var k = lo.z; k <= hi.z; k++) {
        for (var j = lo.y; j <= hi.y; j++) {
            for (var i = lo.x; i <= hi.x; i++) {
                let node = vec3<i32>(i, j, k);
                let d = distance((vec3<f32>(node) - vec3<f32>(0.5)) * params.h, x);
                if d >= reach {
                    continue;
                }
                let index = ts_cell_at(node);
                if index == ts_canon() {
                    continue;
                }
                let add = u32(round(area * kernel(d, smoothing) * SPLAT_FIXED_SCALE));
                atomicAdd(&accum[index], add);
            }
        }
    }
}

@compute @workgroup_size(64)
fn cs_resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.elements {
        return;
    }
    mist[gid.x] = params.strength * f32(atomicLoad(&accum[gid.x])) / SPLAT_FIXED_SCALE;
}
