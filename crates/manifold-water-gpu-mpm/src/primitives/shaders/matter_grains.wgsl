// node.matter_grains — a matter frame's outer layer and loose sand drawn as
// grains: the sand's skin, flying grains, thin streams and the spray off an
// impact. Hand WGSL under exclusion 1 (atomics across passes):
//   count_main: each live point adds one to its occupancy cell, two point
//     spacings on a side (eight points when packed), in an open-addressed
//     table of (cell key, count) pairs the node clears first.
//   emit_main: each point sums its own cell and the 26 round it. Fewer than
//     OUTER points there is the outer layer or loose sand, which draws its
//     grains; more is the deep bulk, whose grains are holes the draw skips. A
//     grain fades in by size as its block thins from OUTER to OUTER_FULL, so
//     no grain pops in or out from one frame to the next.
// A drawing point of radius r draws n = ceil(2 r² / g²) grains of Grain Size
// g, about one and a half times its patch's area, at most K, which the host
// sets from Grain Size and the rest radius (so any Cell Size reaches any
// Grain Size) within its slot budget. Past K the grains grow to
// r · sqrt(2 / K) instead of leaving gaps.
// Copy p·K + j is grain j of point p: a sand grain at a hashed offset in the
// point's cube (spread · radius each way), scaled to its size times a hashed
// 0.7 to 1.3, each hash keyed on the point's id and j, so a grain keeps its
// place and size on the point it rides. Its turn is a hashed start plus
// the point's position over the grain's radius, as a grain rolls: moving x
// turns it about z, moving z about x, falling about both, so still sand stays
// still and moving sand tumbles at its rolling rate. A point past the live
// count, with radius 0 or with no id draws holes.
// bulk (write_bulk 1) is the particles the surface builds from: a point
// drawing grains shrinks to UNDER its radius, so the surface sits just under
// its grains and shows only between them, and the sparsest points shrink
// away, from BULK_KEPT points in the block down to none at BULK_GONE: spray
// and lone grains leave the surface, while a sheet one point thick keeps it
// under its grains, so thin flows do not tear into holes.

struct GrainParams {
    // Below 0: every point.
    live_count: f32,
    // K, the most grains a point draws.
    grains: u32,
    // Metres.
    grain_size: f32,
    spread: f32,
    points: u32,
    // Table pairs, a power of two.
    entries: u32,
    // 1: write bulk.
    write_bulk: u32,
    _pad1: u32,
}

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct Copy {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: GrainParams;
@group(0) @binding(1) var<storage, read> particles: array<FluidParticle>;
@group(0) @binding(2) var<storage, read_write> copies: array<Copy>;
// Pair e is (key, count) at words 2e and 2e + 1; key 0 is an empty pair.
@group(0) @binding(3) var<storage, read_write> table: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> bulk: array<FluidParticle>;

// A point's radius over its spacing (matter.rs RADIUS_SCALE): an occupancy
// cell of two spacings is radius · 2 / RADIUS_SCALE on a side.
const RADIUS_SCALE: f32 = 0.6203505;
// Points in a point's 3³ block of cells below which it draws grains at full
// size, and at which they have faded to nothing: packed sand holds 216, a
// flat surface about half that, a one-point-thick sheet about a sixth and a
// lone grain one, so the outer layer and everything looser draw grains.
const OUTER_FULL: f32 = 150.0;
const OUTER: f32 = 200.0;
// The share of its radius a grain-drawing point gives the surface under it.
const UNDER: f32 = 0.5;
const BULK_GONE: f32 = 6.0;
const BULK_KEPT: f32 = 18.0;
// Probes before a lookup or insert gives up: a full table reads as empty
// cells, which draws grains (never a hole where loose sand flies).
const MAX_PROBES: u32 = 64u;

fn mg_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// Four uniform values in [0, 1) from one key.
fn mg_unit4(key: u32) -> vec4<f32> {
    let a = mg_hash(key);
    let b = mg_hash(a ^ 0x9e3779b9u);
    return vec4<f32>(
        f32(a & 0xffffu),
        f32(a >> 16u),
        f32(b & 0xffffu),
        f32(b >> 16u),
    ) / 65536.0;
}

// Whether point p draws: live, placed and with an id.
fn mg_live(p: u32) -> bool {
    if params.live_count >= 0.0 && f32(p) >= params.live_count {
        return false;
    }
    let q = particles[p];
    return q.position_radius.w > 0.0 && q.id != 0u;
}

fn mg_cell(q: FluidParticle) -> vec3<i32> {
    let edge = 2.0 * q.position_radius.w / RADIUS_SCALE;
    return vec3<i32>(floor(q.position_radius.xyz / edge));
}

// A cell's key: ten bits per axis, so cells 1024 apart share one, far past
// any Matter scene, plus one so no cell keys 0.
fn mg_key(c: vec3<i32>) -> u32 {
    let u = bitcast<vec3<u32>>(c + vec3<i32>(512)) & vec3<u32>(1023u);
    return (u.x | (u.y << 10u) | (u.z << 20u)) + 1u;
}

fn mg_count(c: vec3<i32>) -> u32 {
    let key = mg_key(c);
    let mask = params.entries - 1u;
    var e = mg_hash(key) & mask;
    for (var probe = 0u; probe < MAX_PROBES; probe = probe + 1u) {
        let held = atomicLoad(&table[2u * e]);
        if held == key {
            return atomicLoad(&table[2u * e + 1u]);
        }
        if held == 0u {
            return 0u;
        }
        e = (e + 1u) & mask;
    }
    return 0u;
}

@compute @workgroup_size(256)
fn count_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let p = gid.x;
    if p >= params.points || !mg_live(p) {
        return;
    }
    let key = mg_key(mg_cell(particles[p]));
    let mask = params.entries - 1u;
    var e = mg_hash(key) & mask;
    for (var probe = 0u; probe < MAX_PROBES; probe = probe + 1u) {
        let swap = atomicCompareExchangeWeak(&table[2u * e], 0u, key);
        if swap.old_value == key || (swap.exchanged && swap.old_value == 0u) {
            atomicAdd(&table[2u * e + 1u], 1u);
            return;
        }
        // A weak exchange can fail spuriously on an empty pair: try it again.
        if swap.old_value != 0u {
            e = (e + 1u) & mask;
        }
    }
}

@compute @workgroup_size(256)
fn emit_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let p = gid.x;
    if p >= params.points {
        return;
    }
    let k = max(params.grains, 1u);
    let hole = Copy(vec4<f32>(0.0), vec4<f32>(0.0));
    if !mg_live(p) {
        if params.write_bulk == 1u {
            bulk[p] = particles[p];
        }
        for (var j = 0u; j < k; j = j + 1u) {
            copies[p * k + j] = hole;
        }
        return;
    }
    let q = particles[p];
    let radius = q.position_radius.w;
    let x = q.position_radius.xyz;
    let size = max(params.grain_size, radius * sqrt(2.0 / f32(k)));
    let cover = min(u32(ceil(2.0 * radius * radius / (size * size))), k);
    let c = mg_cell(q);
    var block = 0u;
    for (var n = 0; n < 27; n = n + 1) {
        block = block + mg_count(c + vec3<i32>(n % 3 - 1, (n / 3) % 3 - 1, n / 9 - 1));
    }
    let fade = 1.0 - smoothstep(OUTER_FULL, OUTER, f32(block));
    if params.write_bulk == 1u {
        var kept = q;
        let under = mix(1.0, UNDER, fade);
        kept.position_radius.w = radius * under * smoothstep(BULK_GONE, BULK_KEPT, f32(block));
        bulk[p] = kept;
    }
    let drawn = select(0u, cover, fade > 0.0);
    for (var j = drawn; j < k; j = j + 1u) {
        copies[p * k + j] = hole;
    }
    let grain = size * fade;
    for (var j = 0u; j < drawn; j = j + 1u) {
        let key = mg_hash(mg_hash(q.id) ^ j);
        let place = mg_unit4(key);
        let turn = mg_unit4(key ^ 0x632be5abu);
        let offset = (place.xyz * 2.0 - vec3<f32>(1.0)) * params.spread * radius;
        let scale = grain * (0.7 + 0.6 * place.w);
        let rolled = vec3<f32>(x.z + 0.5 * x.y, 0.0, 0.5 * x.y - x.x) / scale;
        copies[p * k + j] = Copy(
            vec4<f32>(x + offset, scale),
            vec4<f32>(turn.xyz * 6.2831853 + rolled, 0.0),
        );
    }
}
