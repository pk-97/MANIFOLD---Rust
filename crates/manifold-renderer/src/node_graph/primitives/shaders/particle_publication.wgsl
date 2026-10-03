// Publication copy: live records sorted by birth id, ties in source order,
// then zeroed records. A stable LSD radix sort of (key, source index) pairs,
// 4 bits a pass, then one gather of the records. Records are moved as words,
// so every bit survives. Solver order is immutable.
struct Params {
    count: u32,
    slots: u32,
    shift: u32,
    tiles: u32,
    // Pair capacity: keys at [0, cap), source indices at [cap, 2 cap).
    cap: u32,
    // Nonzero on the pass that reads first_upsweep's pairs, dead ones marked.
    first: u32,
    pad0: u32,
    pad1: u32,
}
@group(0) @binding(0) var<uniform> p: Params;
// FluidParticle records as two vec4<u32>: the radius is word 3, the id word 7.
@group(0) @binding(1) var<storage, read> source: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read_write> published: array<vec4<u32>>;
// Tile digit counts, digit-major, scanned in place between upsweep and downsweep.
@group(0) @binding(3) var<storage, read_write> scan: array<u32>;
@group(0) @binding(4) var<storage, read> identity: array<u32>;
@group(0) @binding(5) var<storage, read> stats: array<u32>;
// count, identity epoch, accepted, reserved. Only metadata is fenced/read back.
@group(0) @binding(6) var<storage, read_write> metadata: array<u32>;
@group(0) @binding(7) var<storage, read> pairs_in: array<u32>;
@group(0) @binding(8) var<storage, read_write> pairs_out: array<u32>;
// Word 0: the live count, from the first pass's scan.
@group(0) @binding(9) var<storage, read_write> live: array<u32>;

const TILE: u32 = 256u;
const DEAD: u32 = 0xffffffffu;

// One 16-bit count per digit, two digits a word: a tile holds at most 256.
var<workgroup> counts_lo: array<vec4<u32>, 256>;
var<workgroup> counts_hi: array<vec4<u32>, 256>;

struct Counts { lo: vec4<u32>, hi: vec4<u32> }

fn one_hot(valid: bool, digit: u32) -> Counts {
    let word = vec4<u32>(digit >> 1u);
    let field = select(vec4<u32>(0u), vec4<u32>(1u << ((digit & 1u) * 16u)), vec4<bool>(valid));
    return Counts(
        select(vec4<u32>(0u), field, word == vec4<u32>(0u, 1u, 2u, 3u)),
        select(vec4<u32>(0u), field, word == vec4<u32>(4u, 5u, 6u, 7u)),
    );
}

fn digit_count(counts: Counts, digit: u32) -> u32 {
    let half = select(counts.hi, counts.lo, digit < 8u);
    let lane = (digit >> 1u) & 3u;
    let word = select(select(half.z, half.w, lane == 3u), select(half.x, half.y, lane == 1u), lane < 2u);
    return (word >> ((digit & 1u) * 16u)) & 0xffffu;
}

// The publisher's `radius > 0` with subnormals flushed to zero, as the GPU
// compare evaluates it: a positive normal or +inf. Bits, so fast math cannot
// move it.
fn is_live(radius: u32) -> bool {
    return radius - 0x00800000u <= 0x7f000000u;
}

// This tile's count of each digit, written digit-major for the scan.
fn count_tile(lid: u32, tile: u32, valid: bool, digit: u32) {
    let mine = one_hot(valid, digit);
    counts_lo[lid] = mine.lo;
    counts_hi[lid] = mine.hi;
    workgroupBarrier();
    for (var stride = TILE / 2u; stride > 0u; stride = stride / 2u) {
        if lid < stride {
            counts_lo[lid] = counts_lo[lid] + counts_lo[lid + stride];
            counts_hi[lid] = counts_hi[lid] + counts_hi[lid + stride];
        }
        workgroupBarrier();
    }
    if lid < 16u {
        scan[lid * p.tiles + tile] = digit_count(Counts(counts_lo[0], counts_hi[0]), lid);
    }
}

// (earlier elements of this tile with `digit`, the tile's count of `digit`).
fn rank_tile(lid: u32, valid: bool, digit: u32) -> vec2<u32> {
    let mine = one_hot(valid, digit);
    counts_lo[lid] = mine.lo;
    counts_hi[lid] = mine.hi;
    workgroupBarrier();
    for (var stride = 1u; stride < TILE; stride = stride * 2u) {
        var lo = vec4<u32>(0u);
        var hi = vec4<u32>(0u);
        if lid >= stride {
            lo = counts_lo[lid - stride];
            hi = counts_hi[lid - stride];
        }
        workgroupBarrier();
        counts_lo[lid] = counts_lo[lid] + lo;
        counts_hi[lid] = counts_hi[lid] + hi;
        workgroupBarrier();
    }
    let through = digit_count(Counts(counts_lo[lid], counts_hi[lid]), digit);
    let total = digit_count(Counts(counts_lo[TILE - 1u], counts_hi[TILE - 1u]), digit);
    return vec2<u32>(through - 1u, total);
}

// The first pass's counts, from the records: each becomes a (birth id,
// source index) pair, a dead one marked.
@compute @workgroup_size(256)
fn first_upsweep(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let i = wid.x * TILE + lid.x;
    var valid = false;
    var key = 0u;
    if i < p.count {
        valid = is_live(source[2u * i].w);
        key = source[2u * i + 1u].w;
        pairs_out[i] = key;
        pairs_out[p.cap + i] = select(DEAD, i, valid);
    }
    count_tile(lid.x, wid.x, valid, (key >> p.shift) & 15u);
}

@compute @workgroup_size(256)
fn upsweep(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let i = wid.x * TILE + lid.x;
    let valid = i < live[0];
    var key = 0u;
    if valid {
        key = pairs_in[i];
    }
    count_tile(lid.x, wid.x, valid, (key >> p.shift) & 15u);
}

// Stable scatter: the scanned counts place each tile's run of a digit, the
// workgroup rank orders the run.
@compute @workgroup_size(256)
fn downsweep(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let i = wid.x * TILE + lid.x;
    var valid = false;
    var index = 0u;
    if p.first != 0u {
        if i < p.count {
            index = pairs_in[p.cap + i];
            valid = index != DEAD;
        }
    } else {
        valid = i < live[0];
        if valid {
            index = pairs_in[p.cap + i];
        }
    }
    var key = 0u;
    if valid {
        key = pairs_in[i];
    }
    let digit = (key >> p.shift) & 15u;
    let rank = rank_tile(lid.x, valid, digit);
    if valid {
        let destination = scan[digit * p.tiles + wid.x] - rank.y + rank.x;
        pairs_out[destination] = key;
        pairs_out[p.cap + destination] = index;
    }
    if p.first != 0u && i == 0u {
        live[0] = scan[16u * p.tiles - 1u];
    }
}

@compute @workgroup_size(256)
fn gather(@builtin(global_invocation_id) gid: vec3<u32>) {
    let k = gid.x;
    if k == 0u {
        metadata[0] = live[0];
        metadata[1] = identity[1];
        metadata[2] = select(0u, 1u, stats[0] == 0u && stats[NARROW_BAND_SHORTAGE_WORD] == 0u && identity[3] == 0u);
        metadata[3] = 0u;
    }
    if k >= p.slots {
        return;
    }
    var head = vec4<u32>(0u);
    var tail = vec4<u32>(0u);
    if k < live[0] {
        let origin = pairs_in[p.cap + k];
        head = source[2u * origin];
        tail = source[2u * origin + 1u];
    }
    published[2u * k] = head;
    published[2u * k + 1u] = tail;
}
