// node.matter_stats — per-tick statistics of a matter domain
// (GPU_MPM_SOLVER_DESIGN.md D14, section 3.1). Three passes with one shared
// uniform (same size at binding 0 across entry points):
//   points_main — each workgroup folds a fixed block of points, then a fixed
//                 workgroup-memory tree;
//   grid_main   — the same over lattice nodes;
//   finish_main — one workgroup folds the partials in a fixed order and
//                 writes the 16 stats words.
// Every sum runs in a fixed order, so stats are deterministic. Non-finite
// values are found with an exponent-bits test: fast math may fold isfinite().

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
}

struct MatterGridNode {
    velocity_mass: vec4<f32>,
    velocity_before: vec4<f32>,
}

struct Partial {
    nonfinite: u32,
    live: u32,
    clamped: u32,
    max_accum: u32,
    max_speed: f32,
    min_j: f32,
    max_j: f32,
    volume: f32,
    mass: f32,
    momentum_x: f32,
    momentum_y: f32,
    momentum_z: f32,
    kinetic: f32,
    potential: f32,
    elastic: f32,
    _pad: f32,
}

struct StatsParams {
    point_count: u32,
    node_count: u32,
    point_groups: u32,
    node_groups: u32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    lambda: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    cohesion: f32,
    density: f32,
    tick_index: u32,
    _pad0: u32,
    _pad1: u32,
}

const GROUP: u32 = 256u;
const PER_THREAD: u32 = 8u;

@group(0) @binding(0) var<uniform> params: StatsParams;
@group(0) @binding(1) var<storage, read> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> grid: array<MatterGridNode>;
@group(0) @binding(3) var<storage, read> accum: array<i32>;
@group(0) @binding(4) var<storage, read_write> partials: array<Partial>;
@group(0) @binding(5) var<storage, read_write> stats: array<u32>;

var<workgroup> scratch: array<Partial, 256>;

fn finite(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

fn finite3(v: vec3<f32>) -> bool {
    return finite(v.x) && finite(v.y) && finite(v.z);
}

fn empty() -> Partial {
    var p: Partial;
    p.nonfinite = 0u;
    p.live = 0u;
    p.clamped = 0u;
    p.max_accum = 0u;
    p.max_speed = 0.0;
    p.min_j = 3.0e38;
    p.max_j = -3.0e38;
    p.volume = 0.0;
    p.mass = 0.0;
    p.momentum_x = 0.0;
    p.momentum_y = 0.0;
    p.momentum_z = 0.0;
    p.kinetic = 0.0;
    p.potential = 0.0;
    p.elastic = 0.0;
    p._pad = 0.0;
    return p;
}

fn combine(a: Partial, b: Partial) -> Partial {
    var p: Partial;
    p.nonfinite = a.nonfinite + b.nonfinite;
    p.live = a.live + b.live;
    p.clamped = a.clamped + b.clamped;
    p.max_accum = max(a.max_accum, b.max_accum);
    p.max_speed = max(a.max_speed, b.max_speed);
    p.min_j = min(a.min_j, b.min_j);
    p.max_j = max(a.max_j, b.max_j);
    p.volume = a.volume + b.volume;
    p.mass = a.mass + b.mass;
    p.momentum_x = a.momentum_x + b.momentum_x;
    p.momentum_y = a.momentum_y + b.momentum_y;
    p.momentum_z = a.momentum_z + b.momentum_z;
    p.kinetic = a.kinetic + b.kinetic;
    p.potential = a.potential + b.potential;
    p.elastic = a.elastic + b.elastic;
    p._pad = 0.0;
    return p;
}

fn point_partial(pt: MatterPoint) -> Partial {
    var p = empty();
    if pt.id == 0u {
        return p;
    }
    let j = pt.volume_ratio;
    if !(finite3(pt.position) && finite3(pt.velocity) && finite(j)
        && finite3(pt.affine_x.xyz) && finite3(pt.affine_y.xyz) && finite3(pt.affine_z.xyz)) {
        p.nonfinite = 1u;
        return p;
    }
    let v0 = pt.affine_y.w;
    let m = v0 * params.density;
    let v = pt.velocity;
    let rel = pt.position - vec3<f32>(params.lattice_min_x, params.lattice_min_y, params.lattice_min_z);
    let g = vec3<f32>(params.gravity_x, params.gravity_y, params.gravity_z);
    var e = 0.5 * params.lambda * (j - 1.0) * (j - 1.0);
    if j >= 1.0 {
        e = e * params.cohesion;
    }
    p.live = 1u;
    p.max_speed = length(v);
    p.min_j = j;
    p.max_j = j;
    p.volume = v0 * j;
    p.mass = m;
    p.momentum_x = m * v.x;
    p.momentum_y = m * v.y;
    p.momentum_z = m * v.z;
    p.kinetic = 0.5 * m * dot(v, v);
    p.potential = -m * dot(g, rel);
    p.elastic = v0 * e;
    return p;
}

fn node_partial(i: u32) -> Partial {
    var p = empty();
    let node = grid[i];
    if !(finite3(node.velocity_mass.xyz) && finite(node.velocity_mass.w)) {
        p.nonfinite = 1u;
    }
    if node.velocity_before.w > 0.5 {
        p.clamped = 1u;
    }
    var peak = 0u;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let a = accum[i * 4u + k];
        // |i32::MIN| does not fit; saturate it.
        peak = max(peak, select(u32(abs(a)), 0x80000000u, a == -2147483647 - 1));
    }
    p.max_accum = peak;
    return p;
}

fn reduce_shared(t: u32) {
    for (var stride = GROUP / 2u; stride > 0u; stride = stride / 2u) {
        if t < stride {
            scratch[t] = combine(scratch[t], scratch[t + stride]);
        }
        workgroupBarrier();
    }
}

@compute @workgroup_size(256)
fn points_main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var acc = empty();
    let base = wg.x * GROUP * PER_THREAD;
    for (var k = 0u; k < PER_THREAD; k = k + 1u) {
        let i = base + k * GROUP + t;
        if i < params.point_count {
            acc = combine(acc, point_partial(points[i]));
        }
    }
    scratch[t] = acc;
    workgroupBarrier();
    reduce_shared(t);
    if t == 0u {
        partials[wg.x] = scratch[0];
    }
}

@compute @workgroup_size(256)
fn grid_main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var acc = empty();
    let base = wg.x * GROUP * PER_THREAD;
    for (var k = 0u; k < PER_THREAD; k = k + 1u) {
        let i = base + k * GROUP + t;
        if i < params.node_count {
            acc = combine(acc, node_partial(i));
        }
    }
    scratch[t] = acc;
    workgroupBarrier();
    reduce_shared(t);
    if t == 0u {
        partials[params.point_groups + wg.x] = scratch[0];
    }
}

@compute @workgroup_size(256)
fn finish_main(@builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var acc = empty();
    let total = params.point_groups + params.node_groups;
    for (var i = t; i < total; i = i + GROUP) {
        acc = combine(acc, partials[i]);
    }
    scratch[t] = acc;
    workgroupBarrier();
    reduce_shared(t);
    if t == 0u {
        let s = scratch[0];
        stats[0] = s.nonfinite;
        stats[1] = s.clamped;
        stats[2] = s.live;
        stats[3] = bitcast<u32>(s.max_speed);
        stats[4] = bitcast<u32>(select(s.min_j, 1.0, s.live == 0u));
        stats[5] = bitcast<u32>(select(s.max_j, 1.0, s.live == 0u));
        stats[6] = bitcast<u32>(s.volume);
        stats[7] = s.max_accum;
        stats[8] = bitcast<u32>(s.mass);
        stats[9] = bitcast<u32>(s.momentum_x);
        stats[10] = bitcast<u32>(s.momentum_y);
        stats[11] = bitcast<u32>(s.momentum_z);
        stats[12] = bitcast<u32>(s.kinetic);
        stats[13] = bitcast<u32>(s.potential);
        stats[14] = bitcast<u32>(s.elastic);
        stats[15] = params.tick_index;
    }
}
