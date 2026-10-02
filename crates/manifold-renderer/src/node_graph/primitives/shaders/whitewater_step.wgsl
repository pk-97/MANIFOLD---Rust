// node.whitewater_step — the pool passes the codegen atoms cannot be: the
// seed, the append, the compaction and the split into populations
// (GPU_WHITEWATER_DESIGN.md section 3.9). Atomic-free: every scatter writes
// slots no other thread writes, placed by an inclusive scan in `scan`.
//
// The pool is `capacity` WhitewaterParticle slots, live ones first; `state`
// holds what FLIP keeps across ticks:
//   [0] live slots  [1] next id  [2] pool full  [3] emitted  [4] thinned
// pool full, emitted and thinned count since the pool was seeded.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

struct Particle {
    position_lifetime: vec4<f32>,
    velocity: vec3<f32>,
    kind: u32,
    id: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

struct Spawn {
    position_lifetime: vec4<f32>,
    velocity: vec3<f32>,
    kind: u32,
}

struct Fluid {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct StepParams {
    capacity: u32,
    spawn_slots: u32,
    emitters: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: StepParams;
@group(0) @binding(1) var<storage, read> pool_in: array<Particle>;
@group(0) @binding(2) var<storage, read_write> pool_out: array<Particle>;
@group(0) @binding(3) var<storage, read_write> scan: array<u32>;
@group(0) @binding(4) var<storage, read> spawns: array<Spawn>;
@group(0) @binding(5) var<storage, read_write> state: array<u32>;
@group(0) @binding(6) var<storage, read> offsets: array<u32>;
@group(0) @binding(7) var<storage, read_write> foam: array<Fluid>;
@group(0) @binding(8) var<storage, read_write> bubble: array<Fluid>;
@group(0) @binding(9) var<storage, read_write> spray: array<Fluid>;
@group(0) @binding(10) var<storage, read_write> counts: array<u32>;

const EMPTY: u32 = 3u;
// FLIP's _diffuseParticleIDLimit.
const ID_LIMIT: u32 = 256u;
const STATE_WORDS: u32 = 8u;
// Populations in output order: foam (1), bubbles (0), spray (2).
fn population_kind(p: u32) -> u32 {
    return select(select(2u, 0u, p == 1u), 1u, p == 0u);
}

fn empty_particle() -> Particle {
    var p: Particle;
    p.kind = EMPTY;
    return p;
}

// The scan's own value at i, from the inclusive scan.
fn flag(i: u32) -> u32 {
    if i == 0u {
        return scan[0];
    }
    return scan[i] - scan[i - 1u];
}

// Every slot empty, every state word 0.
@compute @workgroup_size(256)
fn seed_pool(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i < STATE_WORDS {
        state[i] = 0u;
    }
    if i < params.capacity {
        pool_out[i] = empty_particle();
    }
}

// 1 for a spawn slot holding a particle (lifetime above 0).
@compute @workgroup_size(256)
fn live_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i < params.spawn_slots {
        scan[i] = select(0u, 1u, spawns[i].position_lifetime.w > 0.0);
    }
}

fn live_spawns() -> u32 {
    if params.spawn_slots == 0u {
        return 0u;
    }
    return scan[params.spawn_slots - 1u];
}

// FLIP's load: the frame's live spawns, in spawn order, into the empty slots
// after the live ones, each taking the next id. Spawns past capacity find no
// room. The pool is written in place; only its empty slots change.
@compute @workgroup_size(256)
fn append(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    let live = state[0];
    let placed = min(live_spawns(), params.capacity - min(live, params.capacity));
    if i < live || i >= live + placed || i >= params.capacity {
        return;
    }
    let k = i - live;
    var lo = 0u;
    var hi = params.spawn_slots - 1u;
    while lo < hi {
        let mid = (lo + hi) / 2u;
        if scan[mid] >= k + 1u {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    let spawn = spawns[lo];
    var p: Particle;
    p.position_lifetime = spawn.position_lifetime;
    p.velocity = spawn.velocity;
    p.kind = spawn.kind;
    p.id = (state[1] + k) % ID_LIMIT;
    pool_out[i] = p;
}

// After append: the live count and id advance by what was placed; the rest
// is counted as pool full. Emitted counts the live spawns, thinned the
// emissions past the spawn slots (`offsets` is the emitters' running total).
@compute @workgroup_size(1)
fn append_state() {
    let live = min(state[0], params.capacity);
    let spawned = live_spawns();
    let placed = min(spawned, params.capacity - live);
    var total = 0u;
    if params.emitters > 0u {
        total = offsets[params.emitters - 1u];
    }
    state[0] = live + placed;
    state[1] = (state[1] + placed) % ID_LIMIT;
    state[2] = state[2] + (spawned - placed);
    state[3] = state[3] + spawned;
    state[4] = state[4] + (total - min(total, params.spawn_slots));
}

// FLIP's removeParticles: each kept slot (its keep flag scanned) moves to
// the front in pool order; slots past the kept total go empty.
@compute @workgroup_size(256)
fn compact(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if flag(i) == 1u {
        pool_out[scan[i] - 1u] = pool_in[i];
    }
    if i >= scan[params.capacity - 1u] {
        pool_out[i] = empty_particle();
    }
}

@compute @workgroup_size(1)
fn compact_state() {
    if params.capacity > 0u {
        state[0] = scan[params.capacity - 1u];
    }
}

// One flag per (population, slot): slot s of population p is flagged when
// its kind is p's.
@compute @workgroup_size(256)
fn split_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= 3u * params.capacity {
        return;
    }
    let p = i / params.capacity;
    let s = i % params.capacity;
    scan[i] = select(0u, 1u, pool_in[s].kind == population_kind(p));
}

fn population_end(p: u32) -> u32 {
    return scan[(p + 1u) * params.capacity - 1u];
}

fn population_start(p: u32) -> u32 {
    if p == 0u {
        return 0u;
    }
    return population_end(p - 1u);
}

fn write_population(p: u32, slot: u32, value: Fluid) {
    if p == 0u {
        foam[slot] = value;
    } else if p == 1u {
        bubble[slot] = value;
    } else {
        spray[slot] = value;
    }
}

// Each population's particles to the front of its output in pool order,
// the radius FLIP's fade of the lifetime (fluid.rs whitewater_fade). Past
// the count the output is zero: the slot's buffers start zeroed and
// `counts` still holds what this slot's previous publish filled, so only
// [new count, previous count) needs zeroing. `publish_counts` overwrites
// `counts` after.
@compute @workgroup_size(256)
fn split(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= 3u * params.capacity {
        return;
    }
    let p = i / params.capacity;
    let s = i % params.capacity;
    let start = population_start(p);
    if flag(i) == 1u {
        let particle = pool_in[s];
        var out: Fluid;
        let fade = sqrt(clamp(particle.position_lifetime.w / 0.2, 0.0, 1.0));
        out.position_radius = vec4<f32>(particle.position_lifetime.xyz, fade);
        out.velocity = particle.velocity;
        out.id = 0u;
        write_population(p, scan[i] - 1u - start, out);
    }
    if s >= population_end(p) - start && s < counts[p] {
        var zero: Fluid;
        write_population(p, s, zero);
    }
}

// The populations' sizes, then the state words, for the CPU to read once
// the frame retired.
@compute @workgroup_size(1)
fn publish_counts() {
    for (var p = 0u; p < 3u; p = p + 1u) {
        counts[p] = population_end(p) - population_start(p);
    }
    counts[3] = state[3];
    counts[4] = state[4];
    counts[5] = state[2];
    counts[6] = state[0];
    counts[7] = state[1];
}
