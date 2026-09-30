// node.liquid_stats — one tick's statistics of a particle liquid
// (LIQUID_SOLVER_SEAM_DESIGN.md section 3.1, amendment 2). Two passes with one
// shared uniform:
//   particles_main — each workgroup folds a fixed block of records, then a
//                    fixed workgroup-memory tree, into one partial;
//   finish_main    — one workgroup folds the partials in a fixed order and
//                    writes the stats words.
// Every sum runs in a fixed order with no read-modify-write races, so the
// words are the same on every run. Non-finite values are found with an exponent-bits test:
// fast math may fold isfinite().

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct Partial {
    nonfinite: u32,
    live: u32,
    max_speed: f32,
    mass: f32,
    momentum_x: f32,
    momentum_y: f32,
    momentum_z: f32,
    kinetic: f32,
}

struct StatsParams {
    count: u32,
    groups: u32,
    particle_mass: f32,
    _pad0: u32,
}

const GROUP: u32 = 256u;
const PER_THREAD: u32 = 8u;

@group(0) @binding(0) var<uniform> params: StatsParams;
@group(0) @binding(1) var<storage, read> particles: array<FluidParticle>;
@group(0) @binding(2) var<storage, read_write> partials: array<Partial>;
@group(0) @binding(3) var<storage, read_write> stats: array<u32>;

var<workgroup> scratch: array<Partial, 256>;

fn finite(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

fn empty() -> Partial {
    return Partial(0u, 0u, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
}

fn combine(a: Partial, b: Partial) -> Partial {
    return Partial(
        a.nonfinite + b.nonfinite,
        a.live + b.live,
        max(a.max_speed, b.max_speed),
        a.mass + b.mass,
        a.momentum_x + b.momentum_x,
        a.momentum_y + b.momentum_y,
        a.momentum_z + b.momentum_z,
        a.kinetic + b.kinetic,
    );
}

fn record_partial(p: FluidParticle) -> Partial {
    var r = empty();
    let x = p.position_radius;
    let v = p.velocity;
    if !(finite(x.x) && finite(x.y) && finite(x.z) && finite(x.w) && finite(v.x) && finite(v.y) && finite(v.z)) {
        r.nonfinite = 1u;
        return r;
    }
    // Radius 0 marks an unused slot.
    if x.w <= 0.0 {
        return r;
    }
    let m = params.particle_mass;
    r.live = 1u;
    r.max_speed = length(v);
    r.mass = m;
    r.momentum_x = m * v.x;
    r.momentum_y = m * v.y;
    r.momentum_z = m * v.z;
    r.kinetic = 0.5 * m * dot(v, v);
    return r;
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
fn particles_main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var acc = empty();
    let base = wg.x * GROUP * PER_THREAD;
    for (var k = 0u; k < PER_THREAD; k = k + 1u) {
        let i = base + k * GROUP + t;
        if i < params.count {
            acc = combine(acc, record_partial(particles[i]));
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
fn finish_main(@builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var acc = empty();
    for (var i = t; i < params.groups; i = i + GROUP) {
        acc = combine(acc, partials[i]);
    }
    scratch[t] = acc;
    workgroupBarrier();
    reduce_shared(t);
    if t == 0u {
        let s = scratch[0];
        stats[0] = s.nonfinite;
        stats[1] = s.live;
        stats[2] = bitcast<u32>(s.max_speed);
        stats[3] = bitcast<u32>(s.mass);
        stats[4] = bitcast<u32>(s.momentum_x);
        stats[5] = bitcast<u32>(s.momentum_y);
        stats[6] = bitcast<u32>(s.momentum_z);
        stats[7] = bitcast<u32>(s.kinetic);
    }
}
