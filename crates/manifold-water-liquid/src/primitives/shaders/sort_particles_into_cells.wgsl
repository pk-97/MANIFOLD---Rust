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
// records word for word. A companion (`companion_words` > 0) is a record of
// that many words beside each slot, which moves with its slot: `stabilise`
// writes slot i's companion where it writes slot i (GPU FLIP's affine
// velocities beside its particles).
//
// The slots covered are the first `params.count`, or (`count_word` not
// 0xffffffff) the first `counts[count_word]` of them, the slots in use a GPU
// record holds, capped at `params.count`. The dispatch is sized from the same
// record, so no thread reads a slot past it.
//
// A point's key is `sort_key_of(p)`, defined by the source composed before
// this body: the dense grid index of the bin holding the point (clamped into
// the grid), or (the world tiles) the pool index `ts_cell_at` of the world
// cell holding it, with `bin_total` keys either way. A pooled point whose
// tile has no slot keys to the canonical index, the bucket no stored cell
// reads; with `drop_absent` it is not counted, so the canonical bin's range
// is empty and a neighbour search that crosses into an absent tile finds
// nothing there.

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
    // Tiles of the pool a pooled key indexes (`ts_pool_tiles`); 0 when dense.
    pool_tiles: u32,
    // The word of `counts` holding the slots in use, or NO_COUNT_WORD.
    count_word: u32,
    // Padding where H was: a pooled sort's table carries its own.
    _pad_h: u32,
    // 1 when a point keyed to the last, canonical, bin (its tile has no slot)
    // is left out of the sort instead of counted there.
    drop_absent: u32,
    // Words of each slot's companion record (bindings 11 and 12); 0: none.
    companion_words: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
// A GPU record of the slots in use; unread unless `count_word` names a word.
@group(0) @binding(10) var<storage, read> counts: array<u32>;
// Each input slot's companion record, and the sorted slots' (`companion_words`).
@group(0) @binding(11) var<storage, read> companion: array<u32>;
@group(0) @binding(12) var<storage, read_write> companion_sorted: array<u32>;

fn gated_off() -> bool {
    return gate[11] != 0u && !(bitcast<f32>(gate[0]) > 0.0);
}

const NO_RANK: u32 = 0xffffffffu;
const NO_COUNT_WORD: u32 = 0xffffffffu;

fn live_slots() -> u32 {
    if params.count_word == NO_COUNT_WORD {
        return params.count;
    }
    return min(counts[params.count_word], params.count);
}

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
    if gated_off() || i >= live_slots() {
        return;
    }
    if !is_live(i) {
        rank[i] = NO_RANK;
        return;
    }
    let key = sort_key_of(position_of(i));
    if params.drop_absent != 0u && key == params.bin_total - 1u {
        rank[i] = NO_RANK;
        return;
    }
    rank[i] = atomicAdd(&cell_counts[key], 1u);
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
        for (var w = 0u; w < params.companion_words; w = w + 1u) {
            companion_sorted[i * params.companion_words + w] = 0u;
        }
    }
}

@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if gated_off() || i >= live_slots() || rank[i] == NO_RANK {
        return;
    }
    let slot = bin_start(sort_key_of(position_of(i))) + rank[i];
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
    if gated_off() || (params.write_sorted == 0u && params.write_order == 0u && params.companion_words == 0u) {
        return;
    }
    if s >= atomicLoad(&cell_counts[params.bin_total - 1u]) {
        return;
    }
    let i = slot_input[s];
    // The bin the count and the scatter put input i in, by the same rule.
    let b = sort_key_of(position_of(i));
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
    let words = params.companion_words;
    for (var w = 0u; w < words; w = w + 1u) {
        companion_sorted[place * words + w] = companion[i * words + w];
    }
}
