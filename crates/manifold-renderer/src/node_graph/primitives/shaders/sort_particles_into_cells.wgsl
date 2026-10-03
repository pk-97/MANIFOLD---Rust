// node.sort_particles_into_cells — counting sort of liquid particles into
// spatial bins (GPU_FLUID_SURFACE_DESIGN.md D17). Passes, barrier between each:
// clear_counts → count_particles → prefix_scan (level 0 of `cell_counts`) →
// write_ranges → clear_tail → scatter → stabilise. The count pass's atomic ranks
// vary run to run; `stabilise` writes each bin's records in input-index order, so
// every output is the same on every run. `cell_counts` is the scan storage; after
// the scan it holds each bin's inclusive end. With `write_order`, `order` gets each
// sorted slot's input index (NO_RANK past the live total); `sorted` is written only
// with `write_sorted`.
//
// Records are read as words: `stride_words` per record, the position at
// `position_word`. With live_rule 0 a record is live when the word at
// `live_word` is a positive float (a liquid particle's radius); with live_rule 1
// when it is a non-zero id and the position is finite (a matter point). With
// live_rule 2 when it is a whitewater kind below 3 or dust (4; 3 is empty) and the
// position is finite.
// `sorted` is only bound for liquid particle records, so it copies whole
// records word for word.

struct CellRange {
    start: u32,
    count: u32,
}

struct SortParams {
    bin_min: vec3<f32>,
    inv_cell: f32,
    bins: vec3<u32>,
    count: u32,
    bin_total: u32,
    sorted_capacity: u32,
    write_order: u32,
    write_sorted: u32,
    stride_words: u32,
    position_word: u32,
    live_word: u32,
    live_rule: u32,
}

@group(0) @binding(0) var<uniform> params: SortParams;
@group(0) @binding(1) var<storage, read> particles: array<u32>;
@group(0) @binding(2) var<storage, read_write> sorted: array<u32>;
@group(0) @binding(3) var<storage, read_write> ranges: array<CellRange>;
@group(0) @binding(4) var<storage, read_write> cell_counts: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> rank: array<u32>;
@group(0) @binding(6) var<storage, read_write> order: array<u32>;
// The input index the scatter put in each slot, in atomic rank order within
// a bin. Nothing reads it after the sort.
@group(0) @binding(7) var<storage, read_write> slot_input: array<u32>;
// A GPU FLIP clock plan (gpu_flip_clock.wgsl Plan; word 0 step_dt, word 11
// live_mode): live with no time to step, every pass returns. Zeros run.
@group(0) @binding(8) var<storage, read> gate: array<u32>;

fn gated_off() -> bool {
    return gate[11] != 0u && !(bitcast<f32>(gate[0]) > 0.0);
}

const NO_RANK: u32 = 0xffffffffu;

fn finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn position_of(i: u32) -> vec3<f32> {
    let w = i * params.stride_words + params.position_word;
    return vec3<f32>(bitcast<f32>(particles[w]), bitcast<f32>(particles[w + 1u]), bitcast<f32>(particles[w + 2u]));
}

fn is_live(i: u32) -> bool {
    let word = particles[i * params.stride_words + params.live_word];
    if params.live_rule == 0u {
        return bitcast<f32>(word) > 0.0;
    }
    if params.live_rule == 2u {
        return (word < 3u || word == 4u) && finite3(position_of(i));
    }
    return word != 0u && finite3(position_of(i));
}

fn bin_of(p: vec3<f32>) -> u32 {
    let last = vec3<i32>(params.bins) - vec3<i32>(1);
    let b = clamp(vec3<i32>(floor((p - params.bin_min) * params.inv_cell)), vec3<i32>(0), last);
    return u32(b.x) + params.bins.x * (u32(b.y) + params.bins.y * u32(b.z));
}

fn bin_start(b: u32) -> u32 {
    if b == 0u {
        return 0u;
    }
    return atomicLoad(&cell_counts[b - 1u]);
}

@compute @workgroup_size(256)
fn clear_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !gated_off() && gid.x < params.bin_total {
        atomicStore(&cell_counts[gid.x], 0u);
    }
}

@compute @workgroup_size(256)
fn count_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if gated_off() || i >= params.count {
        return;
    }
    if !is_live(i) {
        rank[i] = NO_RANK;
        return;
    }
    rank[i] = atomicAdd(&cell_counts[bin_of(position_of(i))], 1u);
}

@compute @workgroup_size(256)
fn write_ranges(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = gid.x;
    if gated_off() || b >= params.bin_total {
        return;
    }
    let start = bin_start(b);
    ranges[b] = CellRange(start, atomicLoad(&cell_counts[b]) - start);
}

// Slots past the live total become inactive (radius 0) before the scatter
// fills the live ones.
@compute @workgroup_size(256)
fn clear_tail(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if gated_off() || i >= params.sorted_capacity {
        return;
    }
    if i >= atomicLoad(&cell_counts[params.bin_total - 1u]) {
        if params.write_sorted != 0u {
            for (var w = 0u; w < params.stride_words; w = w + 1u) {
                sorted[i * params.stride_words + w] = 0u;
            }
        }
        if params.write_order != 0u {
            order[i] = NO_RANK;
        }
    }
}

@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if gated_off() || i >= params.count || rank[i] == NO_RANK {
        return;
    }
    let slot = bin_start(bin_of(position_of(i))) + rank[i];
    slot_input[slot] = i;
}

// One thread per scattered slot. Its input lands at its bin's start plus the
// number of the bin's inputs with a smaller index: where sorting the bin by
// input index puts it. `slot_input` is only read here, so every thread
// counts against the same scatter. Each slot of a bin of n reads n words; the
// threads of a bin share them, since a bin's slots are adjacent.
@compute @workgroup_size(256)
fn stabilise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if gated_off() || (params.write_sorted == 0u && params.write_order == 0u) {
        return;
    }
    if s >= atomicLoad(&cell_counts[params.bin_total - 1u]) {
        return;
    }
    let i = slot_input[s];
    // The bin the count and the scatter put input i in, by the same rule.
    let b = bin_of(position_of(i));
    let end = atomicLoad(&cell_counts[b]);
    let start = bin_start(b);
    var place = start;
    for (var j = start; j < end; j = j + 1u) {
        place = place + select(0u, 1u, slot_input[j] < i);
    }
    if params.write_sorted != 0u {
        for (var w = 0u; w < params.stride_words; w = w + 1u) {
            sorted[place * params.stride_words + w] = particles[i * params.stride_words + w];
        }
    }
    if params.write_order != 0u {
        order[place] = i;
    }
}
