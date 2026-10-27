// GPU FLIP's pressure solver (gpu_flip_pressure.rs): the multigrid-
// preconditioned conjugate gradient for L p = f on the water, every pass a
// separate entry point.
//
// Every level is a world-tiled lattice (WATER_INFINITE_DOMAIN_DESIGN.md
// sections 2 and 7), read through world_tiles_read.wgsl. Level L's cells are
// 2^L·h, coarse cell c covering cells 2c and 2c + 1 of the level below along
// each axis (c = f >> 1, floor, negative cells included), in tiles of 8³ of
// its own cells. The fine level (0) is the step's table and fields. Level L ≥ 1 has its own table,
// built every prepare from the parents (tile >> 1) of level L − 1's held
// tiles, a children row per slot (its 8 tiles in the level below) and its own
// pooled fields. A pooled field has 512·P + 1 elements: slot s holds its
// tile's cells at s·512 + local, the last is the canonical element, read for
// a tile with no slot: 0 (no water, nothing touched, every face closed) in
// the step's water and faces and in the coarse levels' masks and faces. A
// face record lives with its cell: weight[a] is the open fraction of the
// cell's low face along axis a.
//
// A dispatch names one level as its own: `pool_tiles`, `entries` and the
// table at binding 22, the read side's includer contract. A transfer or a
// coarsening also names the level above it as its coarse side: `coarse_*`,
// that level's table at 23, its children rows at 24 and its parents at 25
// (per slot of the level below, the slot of its tile's parent).
//
// The free surface is ghost fluid on the finest level only (`ghost` 1): an
// air neighbour a of water cell c adds −w·θ to c's diagonal, θ =
// clamp(φ_a / φ_c, −25, 25) with φ_c taken at most −0.005h and φ_a at least
// 0 (FLIP Fluids pressuresolver.cpp, MIT, Copyright (C) 2026 Ryan L. Guy &
// Dennis Fassbaender; see THIRD_PARTY_NOTICES.md). θ ≤ 0, so a cell's
// diagonal is positive exactly when one of its faces is open, and the
// operator stays symmetric. Coarse levels and the density solve run zero φ:
// the plain rows.
//
// Every level's lattice passes run over its active slots
// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 7 (the pressure solve over
// tiles)): a slot is active when a touched cell lies in its tile grown by one
// cell, touched meaning water on the fine level and any touched child on a
// coarse one (classify_main). The list of active slots in slot order
// (lists_main) covers 512 cells per slot, in two workgroups. The smoother
// pairs adjacent x cells in each thread; other passes use one thread per
// cell. Every read of a vector is inside the active slots or reads 0: the
// stencil reads water neighbours only, restriction reads one cell past
// coarse water's children, prolongation one coarse cell past a fine water
// cell's parent, and a tap in a tile with no slot is 0; restriction also
// takes a tap in an inactive slot as 0 (no water lies there, so the residual
// is 0 anyway), never the stale value the slot holds. The pressure and r
// are written on every held slot by init, so outside the active slots they
// are the solve's zero. A folded partial is indexed by its slot, two a slot,
// and the finalize and check passes take an inactive slot's partial as 0, so
// the sums run the same tree whichever slots are active and in whatever
// order a list holds them.
//
// A coarse level that ran out of slots leaves some tiles of the level below
// with no parent. Restriction takes their cells as 0 taps and prolongation
// gives them no correction: the same pairs of cells both ways, so restriction
// stays the transpose of prolongation over 8 and the V-cycle stays symmetric.

struct Params {
    // The dispatched level's pool P and table entries H (the fine side of a
    // transfer or coarsening): the level the read side looks cells up in.
    pool_tiles: u32,
    entries: u32,
    // The coarse side of a transfer or coarsening: its P and H.
    coarse_pool_tiles: u32,
    coarse_entries: u32,
    // The sweep's color, or the dot product's partial count.
    color: u32,
    // Bit 0: smooth starts from zero; residual has no rhs (−L value); check
    // is the start; restrict masks its fine taps to open water. Bit 1
    // (REDUCE): the pass also folds its per-cell product into one partial
    // per workgroup. Bit 2: restrict adds into `out`; init keeps the
    // pressure (WARM, the warm start); check is the warm start's.
    mode: u32,
    cell_size: f32,
    // The conjugate gradient iteration, or the scalar a dot product writes.
    slot: u32,
    // smooth and residual: 1 reads φ (the finest level's ghost rows).
    ghost: u32,
    // check: the stop's relative tolerance; below 0 the solve never stops early.
    tolerance: f32,
    // The listed level's first word in `flags` and `lists`.
    list_base: u32,
    // The listed level's gate triple: its group count is armed[3 · level].
    // A coarsening's fine side (kind reads it).
    level: u32,
    // classify: 1 marks every held slot active (the test-only oracle).
    all_tiles: u32,
    // Inside a round: the solve-live triple (stopped); 0 outside rounds.
    live: u32,
    // arm and check: the gate's triples; arm: the first `armed_triples` of
    // them from `armed`, and the bodies.
    triples: u32,
    armed_triples: u32,
    bodies: u32,
    // restrict: the fine side's first word in `flags`.
    below_base: u32,
    // arm and check: the round layout's single executes (chunk_rounds).
    singles: u32,
    pad0: u32,
};

struct FaceSample {
    velocity: vec4<f32>,
    weight: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> faces: array<FaceSample>;
@group(0) @binding(3) var<storage, read> src: array<f32>;
@group(0) @binding(4) var<storage, read> aux: array<f32>;
@group(0) @binding(5) var<storage, read_write> out: array<f32>;
@group(0) @binding(6) var<storage, read_write> out2: array<f32>;
@group(0) @binding(7) var<storage, read_write> scalars: array<f32>;
@group(0) @binding(8) var<storage, read> coarse_water: array<f32>;
@group(0) @binding(9) var<storage, read_write> out_faces: array<FaceSample>;
@group(0) @binding(10) var<storage, read> phi: array<f32>;
@group(0) @binding(11) var<storage, read_write> progress: array<f32>;
@group(0) @binding(12) var<storage, read_write> gate: array<u32>;
@group(0) @binding(13) var<storage, read_write> tally: array<u32>;
// Every gated dispatch's group triple when on: level l's at 3l, written by
// lists_main from its active slot count, then the one-group triple.
@group(0) @binding(14) var<storage, read_write> armed: array<u32>;
// The replayed rounds' range entries, {location, length} pairs, two a round
// (gpu_flip_pressure.rs Gate): a round's recorded dispatches run when its
// length is its command count and skip when it is 0.
@group(0) @binding(15) var<storage, read_write> ranges: array<u32>;
// Two partials per slot of the gradient's level, by slot: the finalize and
// check passes sum or max them in slot order.
@group(0) @binding(16) var<storage, read_write> partials: array<f32>;
// A level's operator rows, assembled once at prepare (rows_main): the sweep
// and residual passes read a cell's row instead of its six faces, six
// neighbours' water and φ. Non-water cells hold zero rows.
@group(0) @binding(17) var<storage, read> rows: array<Row>;
@group(0) @binding(18) var<storage, read_write> out_rows: array<Row>;
// Per level from its list_base, one word a slot: 1 when the slot is active
// (classify_main), and the active slots in slot order (lists_main).
@group(0) @binding(19) var<storage, read_write> flags: array<u32>;
@group(0) @binding(20) var<storage, read_write> lists: array<u32>;
// The GPU FLIP clock's plan for the step this solve runs in (gpu_flip_clock.wgsl
// Plan; word 0 step_dt, word 11 live_mode). A live slot with no time to step
// is inactive: arm zeroes every gate triple and round, so the gated passes
// run no groups, and every plain pass returns. Zeros (live_mode 0) are
// always active: a solve outside a live clock.
@group(0) @binding(21) var<storage, read> slot_plan: array<u32>;
// The dispatched level's world-tile table (world_tiles_read.wgsl).
@group(0) @binding(22) var<storage, read> ts_table: array<u32>;
// A transfer's or coarsening's coarse level: its table, its children rows
// (8 slots of the level below per slot, child c at offset (c & 1,
// (c >> 1) & 1, c >> 2) from twice its tile), and per slot of the level
// below the slot of its parent (parents_main writes them).
@group(0) @binding(23) var<storage, read> coarse_table: array<u32>;
@group(0) @binding(24) var<storage, read> children: array<u32>;
@group(0) @binding(25) var<storage, read_write> parents: array<u32>;
// A coarse level's held slots: its parents build's list.
@group(0) @binding(26) var<storage, read> held: array<u32>;

fn ts_pool_tiles() -> u32 {
    return u.pool_tiles;
}

fn ts_table_entries() -> u32 {
    return u.entries;
}

fn ts_table_word(i: u32) -> u32 {
    return ts_table[i];
}

fn slot_inactive() -> bool {
    return slot_plan[11] != 0u && !(bitcast<f32>(slot_plan[0]) > 0.0);
}

// A round running after the solve stopped (a chunk executes rounds past the
// stop): in rounds `armed` is bound to the gate, whose solve-live triple the
// stop zeroed. Outside rounds `live` is 0 and nothing is stopped. Listed
// passes need no check: their live counts read the gate too.
fn stopped() -> bool {
    return u.live != 0u && armed[3u * u.live] == 0u;
}

// One cell's row of L: lo = w to −x, +x, −y, +y; hi = w to −z, +z, then the
// ghost diagonal (Σ w − Σ w·θ over air neighbours) and the plain one (Σ w).
// A weight is 0 where the neighbour is not water, so the neighbour sum skips
// exactly what the face stencil skipped.
struct Row {
    lo: vec4<f32>,
    hi: vec4<f32>,
};

const DIVISOR_FLOOR: f32 = 1e-30;
// Rounds a solve may run: the solver's MAX_ITERATIONS.
const ROUNDS: u32 = 900u;
const REDUCE: u32 = 2u;
// init: x starts from the pressure it holds; check: the warm start's test.
const WARM: u32 = 4u;

fn warm() -> bool {
    return (u.mode & WARM) != 0u;
}

// Neither infinite nor NaN, read from the bits so fast math keeps the test.
fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

var<workgroup> sums: array<f32, 256>;
var<workgroup> scan: array<u32, 256>;

// Whether thread gid's workgroup is inside the listed level's active list.
// A recorded dispatch runs every slot's two workgroups; the ones past the
// list return before any barrier, whole.
fn listed(gid: u32) -> bool {
    return (gid >> 8u) < armed[3u * u.level];
}

// Thread gid's slot in the listed level's active list.
fn listed_slot(gid: u32) -> u32 {
    return lists[u.list_base + (gid >> 9u)];
}

// Thread gid's cell through the listed level's active list: its pool index.
fn listed_cell(gid: u32) -> u32 {
    return ts_list_cell(listed_slot(gid), gid);
}

// Thread gid's partial: its slot's two, by workgroup.
fn listed_partial(gid: u32) -> u32 {
    return 2u * listed_slot(gid) + ((gid >> 8u) & 1u);
}

// Whether slot s of the dispatched level holds a tile.
fn held_slot(s: u32) -> bool {
    return s < u.pool_tiles && ts_slot_entry(s) != WT_NONE;
}

fn from_zero() -> bool {
    return (u.mode & 1u) == 1u;
}

fn reduces() -> bool {
    return (u.mode & REDUCE) != 0u;
}

// The workgroup's tree sum of its threads' values, in a fixed order, into
// partials[slot]. Called in uniform control flow by every thread.
fn fold_sum(li: u32, slot: u32, value: f32) {
    sums[li] = value;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = sums[li] + sums[li + width];
        }
        workgroupBarrier();
    }
    if li == 0u {
        partials[slot] = sums[0];
    }
}

fn fold_max(li: u32, slot: u32, value: f32) {
    sums[li] = value;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = max(sums[li], sums[li + width]);
        }
        workgroupBarrier();
    }
    if li == 0u {
        partials[slot] = sums[0];
    }
}

// Partial g of the gradient's level (its flags from list_base), two a slot:
// 0 for an inactive slot, whose partials the fold never wrote.
fn fine_partial(g: u32) -> f32 {
    return select(0.0, partials[g], flags[u.list_base + (g >> 1u)] != 0u);
}

// Thread li's strided share of the `count` partials, summed in order, then
// the workgroup's tree: the one-workgroup second pass of a reduction.
fn total_of_partials(li: u32, count: u32) -> f32 {
    var acc = 0.0;
    for (var g = li; g < count; g = g + 256u) {
        acc = acc + fine_partial(g);
    }
    sums[li] = acc;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = sums[li] + sums[li + width];
        }
        workgroupBarrier();
    }
    return sums[0];
}

fn max_of_partials(li: u32, count: u32) -> f32 {
    var acc = 0.0;
    for (var g = li; g < count; g = g + 256u) {
        acc = max(acc, fine_partial(g));
    }
    sums[li] = acc;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = max(sums[li], sums[li + width]);
        }
        workgroupBarrier();
    }
    return sums[0];
}

// The coarse side's table: the read side's slot rows, neighbour rows and
// lookup over the words at binding 23.
fn coarse_slot_tile(slot: u32) -> vec3<i32> {
    let row = 2u * u.coarse_entries + WT_SLOT_WORDS * slot;
    return bitcast<vec3<i32>>(vec3<u32>(coarse_table[row], coarse_table[row + 1u], coarse_table[row + 2u]));
}

fn coarse_neighbour_slot(slot: u32, k: u32) -> u32 {
    let rows = 2u * u.coarse_entries + WT_SLOT_WORDS * u.coarse_pool_tiles;
    return coarse_table[rows + WT_NEIGHBOURS * slot + k];
}

// The coarse side's slot of tile t, WT_NONE when absent: the read side's
// `ts_hash_slot` over the coarse table.
fn coarse_hash_slot(t: vec3<i32>) -> u32 {
    let entries = u.coarse_entries;
    let key = ts_tile_key(t);
    var i = ts_hash(key, entries);
    for (var walked = 0u; walked < entries; walked = walked + 1u) {
        let c = coarse_table[i];
        if c == key {
            let slot = coarse_table[entries + i];
            if slot == WT_NONE || any(coarse_slot_tile(slot) != t) {
                return WT_NONE;
            }
            return slot;
        }
        if c < key {
            return WT_NONE;
        }
        i = (i + 1u) & (entries - 1u);
    }
    return WT_NONE;
}

fn is_water(at: u32) -> bool {
    return water[at] > 0.5;
}

// The open fraction of the face between cell p and its neighbour one step
// along axis a in direction d: the record of the higher of the two cells.
fn face_weight(p: vec3<i32>, a: i32, d: i32) -> f32 {
    var face = p;
    if d > 0 {
        face[a] = p[a] + 1;
    }
    return faces[ts_face(face)].weight[a];
}

// The ghost ratio θ an air neighbour `air` of water cell `own` puts on the
// diagonal as −w·θ; 0 off the finest level.
fn ghost_ratio(own: u32, air: u32) -> f32 {
    if u.ghost == 0u {
        return 0.0;
    }
    let centre = min(phi[own], -0.005 * u.cell_size);
    return clamp(max(phi[air], 0.0) / (centre + 1e-9), -25.0, 25.0);
}

// One thread per cell of the dispatched level's active slots (an indirect
// dispatch over its list): its row of L from its faces, its neighbours'
// water and, with `ghost`, φ. The sums run in the face order −x, +x, −y, +y,
// −z, +z, the order the stencil summed in before the rows were assembled, so
// a solve on rows is bit for bit the solve that recomputed them. A cell that
// is not water, or has no open face, is a zero row.
@compute @workgroup_size(256, 1, 1)
fn rows_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let idx = listed_cell(gid.x);
    ts_home(idx);
    let p = ts_cell_coord(idx);
    var row = Row(vec4<f32>(0.0), vec4<f32>(0.0));
    if is_water(idx) {
        var ghost = 0.0;
        var plain = 0.0;
        for (var k = 0u; k < 6u; k = k + 1u) {
            let a = i32(k / 2u);
            let d = select(-1, 1, (k & 1u) == 1u);
            let w = face_weight(p, a, d);
            if w > 0.0 {
                ghost = ghost + w;
                plain = plain + w;
                var q = p;
                q[a] = p[a] + d;
                let at = ts_cell_at(q);
                if is_water(at) {
                    if k < 4u {
                        row.lo[k] = w;
                    } else {
                        row.hi[k - 4u] = w;
                    }
                } else {
                    ghost = ghost - w * ghost_ratio(idx, at);
                }
            }
        }
        row.hi.z = ghost;
        row.hi.w = plain;
    }
    out_rows[idx] = row;
}

// The cell's diagonal (the ghost one with `ghost` 1, else the plain one)
// and Σ w · value over its water neighbours, `value` read from `out`
// (smoothing in place), `aux`, or nowhere (source 2: the diagonal only).
struct Stencil {
    diagonal: f32,
    sum: f32,
};

// A zero weight must not read its neighbour (which may be in a tile with no
// slot). Keep the six additions in the original face order without a
// dynamic loop or dynamic vector-component indexing in each smoothing or
// operator pass.
fn stencil_add(sum: f32, source: u32, weight: f32, p: vec3<i32>, a: i32, d: i32) -> f32 {
    if !(weight > 0.0) {
        return sum;
    }
    var q = p;
    q[a] = p[a] + d;
    let at = ts_cell_at(q);
    if source == 0u {
        return sum + weight * out[at];
    }
    return sum + weight * aux[at];
}

// Cell idx's stencil, the thread's home set on idx.
fn stencil(idx: u32, source: u32) -> Stencil {
    let row = rows[idx];
    var s = Stencil(select(row.hi.w, row.hi.z, u.ghost == 1u), 0.0);
    if source == 2u {
        return s;
    }
    let p = ts_cell_coord(idx);
    s.sum = stencil_add(s.sum, source, row.lo.x, p, 0, -1);
    s.sum = stencil_add(s.sum, source, row.lo.y, p, 0, 1);
    s.sum = stencil_add(s.sum, source, row.lo.z, p, 1, -1);
    s.sum = stencil_add(s.sum, source, row.lo.w, p, 1, 1);
    s.sum = stencil_add(s.sum, source, row.hi.x, p, 2, -1);
    s.sum = stencil_add(s.sum, source, row.hi.y, p, 2, 1);
    return s;
}

// One red-black Gauss-Seidel sweep of L e = rhs in place in `out` (e), rhs
// in `src`. A water cell of the swept color becomes
// (Σ w · water neighbours' e − h² · rhs) / diagonal, or 0 with no open face.
// Each thread owns adjacent x cells: one of each color. A group still covers
// the same 256 cells of a tile, with half as many threads. From zero (mode
// bit 0) the other cell is written 0 and neighbours read 0. With REDUCE,
// both products occupy their original 256-element reduction positions, so
// the tree and partials stay bit-for-bit identical to one thread per cell.
@compute @workgroup_size(128, 1, 1)
fn smooth_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let pair = 2u * gid.x;
    if !listed(pair) {
        return;
    }
    // Tile origins are multiples of 8 and pair.x is even, so y/z alone
    // determine which member has the requested global checkerboard color.
    let offset = u.color ^ (((pair >> 3u) ^ (pair >> 6u)) & 1u);
    let idx = listed_cell(pair + offset);
    let other = listed_cell(pair + (offset ^ 1u));
    ts_home(idx);
    var e = out[idx];
    if is_water(idx) {
        let h2 = u.cell_size * u.cell_size;
        let s = stencil(idx, select(0u, 2u, from_zero()));
        e = select(0.0, (s.sum - h2 * src[idx]) / s.diagonal, s.diagonal > 0.0);
        out[idx] = e;
    } else if from_zero() {
        e = 0.0;
        out[idx] = e;
    }
    let product = src[idx] * e;
    if from_zero() {
        out[other] = 0.0;
    }
    var other_product = 0.0;
    if reduces() {
        other_product = src[other] * out[other];
    }
    if reduces() {
        sums[2u * li + offset] = product;
        sums[2u * li + (offset ^ 1u)] = other_product;
        workgroupBarrier();
        for (var width = 128u; width > 0u; width = width >> 1u) {
            if li < width {
                sums[li] = sums[li] + sums[li + width];
            }
            workgroupBarrier();
        }
        if li == 0u {
            partials[listed_partial(pair)] = sums[0];
        }
    }
}

// The coarsest level's whole approximate solve in one workgroup: red then
// black from zero, `color` sweeps of each, then as many black then red, the
// same updates smooth_main makes one sweep a dispatch. A color's cells read
// only the other color, so each sweep's order is free and the barrier
// between sweeps is all the passes need. A stopped solve's gate leaves the
// level no cells.
@compute @workgroup_size(256, 1, 1)
fn coarsest_main(@builtin(local_invocation_index) li: u32) {
    let cells = armed[3u * u.level] * 256u;
    let sweeps = 2u * u.color;
    let h2 = u.cell_size * u.cell_size;
    for (var sweep = 0u; sweep < 2u * sweeps; sweep = sweep + 1u) {
        // Red then black on the way down, black then red on the way back.
        let color = (sweep & 1u) ^ select(0u, 1u, sweep >= sweeps);
        let from_zero = sweep == 0u;
        for (var gid = li; gid < cells; gid = gid + 256u) {
            let idx = listed_cell(gid);
            // A tile's origin is a multiple of 8: its local x + y + z parity
            // is the cell's global checkerboard color.
            if ((gid ^ (gid >> 3u) ^ (gid >> 6u)) & 1u) != color {
                if from_zero {
                    out[idx] = 0.0;
                }
                continue;
            }
            ts_home(idx);
            if is_water(idx) {
                let s = stencil(idx, select(0u, 2u, from_zero));
                out[idx] = select(0.0, (s.sum - h2 * src[idx]) / s.diagonal, s.diagonal > 0.0);
            } else if from_zero {
                out[idx] = 0.0;
            }
        }
        storageBarrier();
    }
}

// rhs − L value in a water cell with an open face, else 0; rhs in `src`
// (0 with mode bit 0, which makes −L value), value in `aux`. With REDUCE
// (the operator product s = −L p with no bodies) each thread also folds
// value · result (p · s) into the workgroup's partial.
@compute @workgroup_size(256, 1, 1)
fn residual_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x);
    ts_home(idx);
    var result = 0.0;
    if is_water(idx) {
        let s = stencil(idx, 1u);
        if s.diagonal > 0.0 {
            let rhs = select(src[idx], 0.0, from_zero());
            result = rhs - (s.sum - s.diagonal * aux[idx]) / (u.cell_size * u.cell_size);
        }
    }
    out[idx] = result;
    let product = aux[idx] * result;
    if reduces() {
        fold_sum(li, listed_partial(gid.x), product);
    }
}

// The transfers are the trilinear prolongation and its transpose of McAdams,
// Sifakis & Teran 2010 (the form `scripts/mgpcg_reference.py` mirrors); the
// FLIP Fluids engine is PCG+MIC(0) and has no transfer form to port. Fine
// cell f's parent is f >> 1 (floor, negative cells included); it takes 3/4
// of its parent and 1/4 of the parent's neighbour on its side along each
// axis. Each workgroup is one half tile (8×8×4 cells); it finds the slots of
// the tiles its taps lie in, stages the taps in workgroup memory once, then
// every cell gathers from the stage in the same order with the same weights
// as a direct gather, so the sums are bitwise those of the gather kernels.
// A tap in a tile with no slot, or one whose parent ran out of slots, is 0.

// Fine taps of a coarse half tile: 2×8+2 by 2×8+2 by 2×4+2, lying in 4×4×3
// fine tiles.
const RESTRICT_FOOT: vec3<i32> = vec3<i32>(18, 18, 10);
const RESTRICT_FOOT_CELLS: u32 = 3240u;
const RESTRICT_TILES: vec3<i32> = vec3<i32>(4, 4, 3);
var<workgroup> fine_patch: array<f32, 3240>;
var<workgroup> fine_slots: array<u32, 48>;

// Coarse taps of a fine half tile: 8/2+2 by 8/2+2 by 4/2+2, lying in 2×2×2
// coarse tiles.
const PROLONG_FOOT: vec3<i32> = vec3<i32>(6, 6, 4);
const PROLONG_FOOT_CELLS: u32 = 144u;
var<workgroup> coarse_patch: array<f32, 144>;
var<workgroup> coarse_slots: array<u32, 8>;

// Fine tap 2c + d's share in coarse cell c along one axis, d in −1..=2: the
// prolongation weight read the other way.
fn tap_weight(d: i32) -> f32 {
    return select(0.25, 0.75, d == 0 || d == 1);
}

// Whether fine cell `at` keeps its right-hand side at the gradient's start
// (init_main): water with an open face.
fn open_water(at: u32) -> bool {
    if !is_water(at) {
        return false;
    }
    let p = ts_cell_coord(at);
    var diagonal = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        diagonal = diagonal + face_weight(p, a, -1) + face_weight(p, a, 1);
    }
    return diagonal > 0.0;
}

// One thread per coarse cell of the coarse level's active slots: the fine
// residual in `src` restricted by the transpose of prolongation over 8,
// masked to coarse water. The fine tiles come from the coarse slot's
// neighbour row and those slots' children rows, so a fine tile whose parent
// has no slot is never read, nor one the fine side's flags (from
// `below_base`) mark inactive. Mode bit 0 (RESTRICT_MASK) takes a fine tap
// only from open water, as init takes the right-hand side, so a solve started
// on a coarse level restricts what the fine one would have started from; bit
// 2 (RESTRICT_ADD) adds the sum to `out` instead of writing it and leaves
// non-water cells alone. The unlisted return precedes the barriers, whole.
@compute @workgroup_size(256, 1, 1)
fn restrict_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() || !listed(gid.x) {
        return;
    }
    let masked = (u.mode & 1u) != 0u;
    let adds = (u.mode & 4u) != 0u;
    let slot = listed_slot(gid.x);
    let tile = coarse_slot_tile(slot);
    let origin = tile * 8 + vec3<i32>(0, 0, 4 * i32((gid.x >> 8u) & 1u));
    let base = 2 * origin - vec3<i32>(1);
    let first = base >> vec3<u32>(3u);
    if li < 48u {
        let f = first + ts_grid_coord(li, RESTRICT_TILES);
        let d = (f >> vec3<u32>(1u)) - tile;
        var up = slot;
        if any(d != vec3<i32>(0)) {
            up = coarse_neighbour_slot(slot, ts_neighbour_index(d));
        }
        var child = WT_NONE;
        if up != WT_NONE {
            let b = vec3<u32>(f & vec3<i32>(1));
            child = children[8u * up + b.x + 2u * b.y + 4u * b.z];
        }
        if child != WT_NONE && flags[u.below_base + child] == 0u {
            child = WT_NONE;
        }
        fine_slots[li] = child;
    }
    workgroupBarrier();
    // The half tile's own first child (offset (1, 1, 1) from `first`) is the
    // home of open_water's face reads.
    let home = fine_slots[ts_grid_index(vec3<i32>(1), RESTRICT_TILES)];
    if home != WT_NONE {
        ts_home(home * 512u);
    }
    for (var k = li; k < RESTRICT_FOOT_CELLS; k = k + 256u) {
        let q = base + ts_grid_coord(k, RESTRICT_FOOT);
        let s = fine_slots[ts_grid_index((q >> vec3<u32>(3u)) - first, RESTRICT_TILES)];
        var v = 0.0;
        if s != WT_NONE {
            let at = s * 512u + ts_local(q);
            if !masked || open_water(at) {
                v = src[at];
            }
        }
        fine_patch[k] = v;
    }
    workgroupBarrier();
    let idx = ts_list_cell(slot, gid.x);
    if !(coarse_water[idx] > 0.5) {
        if !adds {
            out[idx] = 0.0;
        }
        return;
    }
    let p = tile * 8 + ts_local_coord(gid.x & 511u);
    let at = 2 * (p - origin) + vec3<i32>(1);
    var sum = 0.0;
    for (var dz = -1; dz <= 2; dz = dz + 1) {
        let wz = tap_weight(dz);
        for (var dy = -1; dy <= 2; dy = dy + 1) {
            let wy = wz * tap_weight(dy);
            for (var dx = -1; dx <= 2; dx = dx + 1) {
                let tap = fine_patch[ts_grid_index(at + vec3<i32>(dx, dy, dz), RESTRICT_FOOT)];
                sum = sum + wy * tap_weight(dx) * tap;
            }
        }
    }
    if adds {
        out[idx] = out[idx] + sum / 8.0;
    } else {
        out[idx] = sum / 8.0;
    }
}

// One thread per cell of every slot of the dispatched level: `out` to 0 in
// every held slot. The target of a prolongation chain, so a prolonged vector
// is 0 off the water.
@compute @workgroup_size(256, 1, 1)
fn zero_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() || stopped() {
        return;
    }
    let slot = gid.x >> 9u;
    if held_slot(slot) {
        out[ts_list_cell(slot, gid.x)] = 0.0;
    }
}

// One thread per fine cell of the fine level's active slots: in a water
// cell, e in `out` plus the coarse correction in `src` interpolated
// trilinearly. The coarse tiles come from the slot's parent and its
// neighbour row; a slot with no parent gets no correction. The unlisted
// return precedes the barriers, whole.
@compute @workgroup_size(256, 1, 1)
fn prolong_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() || !listed(gid.x) {
        return;
    }
    let slot = listed_slot(gid.x);
    let tile = ts_slot_tile(slot);
    let origin = tile * 8 + vec3<i32>(0, 0, 4 * i32((gid.x >> 8u) & 1u));
    let base = (origin >> vec3<u32>(1u)) - vec3<i32>(1);
    let first = base >> vec3<u32>(3u);
    let k = gid.x & 255u;
    if k < 8u {
        let up = parents[slot];
        let d = first + ts_grid_coord(k, vec3<i32>(2)) - (tile >> vec3<u32>(1u));
        var s = WT_NONE;
        if up != WT_NONE && all(abs(d) <= vec3<i32>(1)) {
            s = up;
            if any(d != vec3<i32>(0)) {
                s = coarse_neighbour_slot(up, ts_neighbour_index(d));
            }
        }
        coarse_slots[k] = s;
    }
    workgroupBarrier();
    if k < PROLONG_FOOT_CELLS {
        let q = base + ts_grid_coord(k, PROLONG_FOOT);
        let s = coarse_slots[ts_grid_index((q >> vec3<u32>(3u)) - first, vec3<i32>(2))];
        var v = 0.0;
        if s != WT_NONE {
            v = src[s * 512u + ts_local(q)];
        }
        coarse_patch[k] = v;
    }
    workgroupBarrier();
    let idx = ts_list_cell(slot, gid.x);
    if !is_water(idx) {
        return;
    }
    let f = tile * 8 + ts_local_coord(gid.x & 511u);
    let parent = f >> vec3<u32>(1u);
    let odd = (f & vec3<i32>(1)) == vec3<i32>(1);
    let other = select(parent - vec3<i32>(1), parent + vec3<i32>(1), odd);
    var sum = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let pick = vec3<bool>((corner & 1) != 0, (corner & 2) != 0, (corner & 4) != 0);
        let w = select(vec3<f32>(0.75), vec3<f32>(0.25), pick);
        let tap = coarse_patch[ts_grid_index(select(parent, other, pick) - base, PROLONG_FOOT)];
        sum = sum + w.x * w.y * w.z * tap;
    }
    out[idx] = out[idx] + sum;
}

// One thread per slot of the dispatched level (the fine side of a
// coarsening): the slot of its tile's parent in the coarse level, WT_NONE
// when the slot is free or the coarse level ran out of slots before its
// parent.
@compute @workgroup_size(256, 1, 1)
fn parents_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let s = gid.x;
    if s >= u.pool_tiles {
        return;
    }
    var up = WT_NONE;
    if ts_slot_entry(s) != WT_NONE {
        up = coarse_hash_slot(ts_slot_tile(s) >> vec3<u32>(1u));
    }
    parents[s] = up;
}

// A cell's kind in `water`: 1 water, 0 air, −1 solid. The fine level's mask
// (u.level 0) carries no solid kind: there a cell with every face closed is
// solid (a cell inside a body or a wall, or in a tile with no slot), any
// other non-water cell air.
const KIND_AIR: u32 = 0u;
const KIND_WATER: u32 = 1u;
const KIND_SOLID: u32 = 2u;

fn kind(p: vec3<i32>) -> u32 {
    let at = ts_cell_at(p);
    if water[at] > 0.5 {
        return KIND_WATER;
    }
    if water[at] < -0.5 {
        return KIND_SOLID;
    }
    if u.level != 0u {
        return KIND_AIR;
    }
    for (var a = 0; a < 3; a = a + 1) {
        if face_weight(p, a, -1) != 0.0 || face_weight(p, a, 1) != 0.0 {
            return KIND_AIR;
        }
    }
    return KIND_SOLID;
}

// The slot of the one tile of the level below that holds all eight children
// of coarse cell `local` of coarse slot `slot`: the children row's entry for
// the cell's octant of its tile.
fn child_slot(slot: u32, local: u32) -> u32 {
    let o = vec3<u32>(ts_local_coord(local) >> vec3<u32>(2u));
    return children[8u * slot + o.x + 2u * o.y + 4u * o.z];
}

// One thread per coarse cell of the coarse level's held slots (an indirect
// dispatch over its list): the cell-type coarsening of McAdams, Sifakis and
// Teran 2010 into `out` — air (0) when any fine child is air, solid (−1)
// when every one is solid, else water (1); children in a tile with no slot
// are solid. Touched (1) into `out2` when any child is touched in `aux` (the
// fine level's water, or the level below's touched).
@compute @workgroup_size(256, 1, 1)
fn coarsen_water_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let slot = held[gid.x >> 9u];
    let local = gid.x & 511u;
    let child = child_slot(slot, local);
    var any_air = false;
    var all_solid = true;
    var touched = false;
    if child != WT_NONE {
        ts_home(child * 512u);
        let p = 2 * (coarse_slot_tile(slot) * 8 + ts_local_coord(local));
        for (var c = 0; c < 8; c = c + 1) {
            let q = p + vec3<i32>(c & 1, (c >> 1u) & 1, (c >> 2u) & 1);
            let k = kind(q);
            any_air = any_air || k == KIND_AIR;
            all_solid = all_solid && k == KIND_SOLID;
            if aux[ts_cell_at(q)] > 0.5 {
                touched = true;
            }
        }
    }
    let idx = ts_list_cell(slot, gid.x);
    out[idx] = select(select(1.0, -1.0, all_solid), 0.0, any_air);
    out2[idx] = select(0.0, 1.0, touched);
}

// One thread per coarse face record of the coarse level's held slots: each
// coarse face's open fraction is the mean of the four fine faces it covers,
// all in the cell's child tile (0 when that tile has no slot); velocity 0.
@compute @workgroup_size(256, 1, 1)
fn coarsen_faces_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let slot = held[gid.x >> 9u];
    let local = gid.x & 511u;
    let child = child_slot(slot, local);
    var record = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    if child != WT_NONE {
        let p = 2 * (coarse_slot_tile(slot) * 8 + ts_local_coord(local));
        for (var a = 0; a < 3; a = a + 1) {
            var sum = 0.0;
            for (var k = 0; k < 4; k = k + 1) {
                var q = p;
                var bit = 0;
                for (var b = 0; b < 3; b = b + 1) {
                    if b != a {
                        q[b] = q[b] + ((k >> u32(bit)) & 1);
                        bit = bit + 1;
                    }
                }
                sum = sum + faces[child * 512u + ts_local(q)].weight[a];
            }
            record.weight[a] = 0.25 * sum;
        }
    }
    out_faces[ts_list_cell(slot, gid.x)] = record;
}

// 32 lanes per slot of this level: active (1) when the slot holds a tile
// and a touched cell (`water` binds the level's touched mask) lies in its
// tile grown by one cell, into flags from list_base; every held slot with
// `all_tiles`, 0 for a free slot.
@compute @workgroup_size(256, 1, 1)
fn classify_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    // Eight slots per workgroup, with 32 lanes sharing each tile's 10³ halo.
    // Lanes past the pool contribute zero and still reach every barrier.
    let s = gid.x >> 5u;
    let lane = gid.x & 31u;
    let valid = s < u.pool_tiles;
    let occupied = valid && ts_slot_entry(s) != WT_NONE;
    var lit = occupied && u.all_tiles != 0u;
    if occupied && !lit {
        ts_home(s * 512u);
        let origin = ts_cell_coord(s * 512u) - vec3<i32>(1);
        for (var k = lane; k < 1000u && !lit; k = k + 32u) {
            lit = is_water(ts_cell_at(origin + ts_grid_coord(k, vec3<i32>(10))));
        }
    }
    scan[li] = u32(lit);
    workgroupBarrier();
    for (var width = 16u; width > 0u; width = width >> 1u) {
        if lane < width {
            scan[li] = scan[li] | scan[li + width];
        }
        workgroupBarrier();
    }
    if lane == 0u && valid {
        flags[u.list_base + s] = scan[li];
    }
}

// One workgroup per level: the level's active slots in slot order into
// lists from list_base, and the level's gate triple into armed: two
// workgroups a slot. Each thread counts a run of slots, the workgroup scans
// the counts (integers, so the order is exact), and each thread writes its
// run's slots at its offset.
@compute @workgroup_size(256, 1, 1)
fn lists_main(@builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    let total = u.pool_tiles;
    let run = (total + 255u) / 256u;
    let first = min(li * run, total);
    let last = min(first + run, total);
    var mine = 0u;
    for (var t = first; t < last; t = t + 1u) {
        mine = mine + flags[u.list_base + t];
    }
    scan[li] = mine;
    workgroupBarrier();
    for (var width = 1u; width < 256u; width = width << 1u) {
        var add = 0u;
        if li >= width {
            add = scan[li - width];
        }
        workgroupBarrier();
        scan[li] = scan[li] + add;
        workgroupBarrier();
    }
    var at = scan[li] - mine;
    for (var t = first; t < last; t = t + 1u) {
        if flags[u.list_base + t] != 0u {
            lists[u.list_base + at] = t;
            at = at + 1u;
        }
    }
    if li == 255u {
        armed[3u * u.level] = 2u * scan[255];
        armed[3u * u.level + 1u] = 1u;
        armed[3u * u.level + 2u] = 1u;
    }
}

// Test-only: NaN into every cell of this level's inactive slots in `out`
// (a vector) and the level's two partials per inactive slot, so a solve
// that reads outside its active set shows in its pressure. Never named
// outside the proofs (gpu_flip_pressure_tests.rs).
@compute @workgroup_size(256, 1, 1)
fn poison_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slot = gid.x >> 9u;
    if slot >= u.pool_tiles || flags[u.list_base + slot] != 0u {
        return;
    }
    let nan = bitcast<f32>(0x7fc00000u);
    out[ts_list_cell(slot, gid.x)] = nan;
    if (gid.x & 511u) == 0u && u.color == 1u {
        partials[2u * slot] = nan;
        partials[2u * slot + 1u] = nan;
    }
}

// lentine_flux_main's lattices: dense boxes, x fastest, cell (i, j, k) at
// i + nx·(j + ny·k), a face grid padded to (n + 1)³ records. A standalone
// gather the step never dispatches (gpu_flip_lentine.rs), so it keeps its
// boxes.
struct LentineParams {
    nx: u32,
    ny: u32,
    nz: u32,
    cell_size: f32,
    cx: u32,
    cy: u32,
    cz: u32,
    pad: u32,
};

@group(0) @binding(27) var<uniform> lentine: LentineParams;

// Lentine, Zheng & Fedkiw (2010), sections 3.2--3.3:
// https://physbam.stanford.edu/papers/stanford2010-02.pdf
// Conservative outer-projection gather; the MG gather stays unchanged.
// faces: solid weights/volumes; src: fluid FaceSample words; aux: solid
// velocity FaceSample words. velocity.xyz stores fluid flux / coarse area;
// velocity.w stores the complete child solid source / coarse volume,
// including internal (c_low - c_high) v_s terms. weight.xyz is open area.
@compute @workgroup_size(256, 1, 1)
fn lentine_flux_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = vec3<i32>(i32(lentine.nx), i32(lentine.ny), i32(lentine.nz));
    let c = vec3<i32>(i32(lentine.cx), i32(lentine.cy), i32(lentine.cz));
    let m = c + vec3<i32>(1);
    let records = n + vec3<i32>(1);
    let idx = gid.x;
    if idx >= u32(m.x * m.y * m.z) {
        return;
    }
    let p = ts_grid_coord(idx, m);
    var record = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0; a < 3; a = a + 1) {
        var across = p;
        across[a] = 0;
        if !all(across < c) || p[a] == 0 || p[a] == c[a] {
            continue;
        }
        var area = 0.0;
        var flux = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            var q = 2 * p;
            var bit = 0;
            for (var b = 0; b < 3; b = b + 1) {
                if b != a {
                    q[b] = q[b] + ((k >> u32(bit)) & 1);
                    bit = bit + 1;
                }
            }
            var inside = q;
            inside[a] = 0;
            if all(inside < n) {
                let face = ts_grid_index(q, records);
                let w = faces[face].weight[a];
                area = area + w;
                flux = flux + w * src[8u * face + u32(a)];
            }
        }
        record.weight[a] = area * 0.25;
        record.velocity[a] = flux * 0.25;
    }
    if all(p < c) {
        var solid_source = 0.0;
        var volume = 0.0;
        for (var k = 0u; k < 8u; k = k + 1u) {
            let q = 2 * p + ts_grid_coord(k, vec3<i32>(2));
            if any(q >= n) || !(water[ts_grid_index(q, n)] > 0.5) {
                continue;
            }
            let open_volume = faces[ts_grid_index(q, records)].weight.w;
            volume = volume + open_volume;
            for (var a = 0; a < 3; a = a + 1) {
                for (var side = 0; side < 2; side = side + 1) {
                    var f = q;
                    f[a] = f[a] + side;
                    if f[a] == 0 || f[a] == n[a] {
                        continue;
                    }
                    let face = ts_grid_index(f, records);
                    let sign = f32(2 * side - 1);
                    solid_source = solid_source + sign *
                        (open_volume - faces[face].weight[a]) * aux[8u * face + u32(a)];
                }
            }
        }
        record.velocity.w = solid_source / (8.0 * lentine.cell_size);
        record.weight.w = volume * 0.125;
    }
    out_faces[idx] = record;
}

// The conjugate gradient's start, over every slot of the gradient's level
// (512 threads a slot in slot order, no list): r = f (`src`) on water with an
// open face, else 0, into `out`, and the pressure x into `out2`: 0, or with
// WARM (the warm start) the pressure `out2` already holds on water with an
// open face, a non-finite value counting as no guess. Off those cells both
// are 0, so outside the active slots, which the solve never writes, they are
// the solve's zero. A cold solve that stops before its first iteration
// leaves zero pressure; warm_main finishes a warm start. The ghost rows only
// add to a diagonal, so an open face is the whole test. With REDUCE each
// thread folds |r| into its slot's partial, for the start's |f|∞.
@compute @workgroup_size(256, 1, 1)
fn init_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    let slot = gid.x >> 9u;
    if slot >= u.pool_tiles {
        return;
    }
    var r = 0.0;
    if held_slot(slot) {
        let idx = ts_list_cell(slot, gid.x);
        ts_home(idx);
        var diagonal = 0.0;
        if is_water(idx) {
            let p = ts_cell_coord(idx);
            for (var a = 0; a < 3; a = a + 1) {
                diagonal = diagonal + face_weight(p, a, -1) + face_weight(p, a, 1);
            }
        }
        r = select(0.0, src[idx], diagonal > 0.0);
        out[idx] = r;
        var x = 0.0;
        if warm() && diagonal > 0.0 && finite(out2[idx]) {
            x = out2[idx];
        }
        out2[idx] = x;
    }
    if reduces() {
        fold_max(li, 2u * slot + ((gid.x >> 8u) & 1u), abs(r));
    }
}

// The warm start's residual, one thread per cell of the fine level's
// active slots, after the start's check: r = f − L x0 into `out2` on water
// with an open face (f in `src`, x0 in place in `out` as init kept it),
// residual_main's arithmetic, so x0 = 0 gives init's r bit for bit. A start
// the check stopped (|f|∞ under START_FLOOR: no divergence, no pressure)
// zeroes x instead. With REDUCE each thread folds |r| into its slot's
// partial, for the warm start's check.
@compute @workgroup_size(256, 1, 1)
fn warm_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() || !listed(gid.x) {
        return;
    }
    let no_divergence = progress[2] > 0.5;
    let idx = listed_cell(gid.x);
    ts_home(idx);
    var r = 0.0;
    if is_water(idx) {
        if no_divergence {
            out[idx] = 0.0;
        } else {
            let s = stencil(idx, 0u);
            if s.diagonal > 0.0 {
                r = src[idx] - (s.sum - s.diagonal * out[idx]) / (u.cell_size * u.cell_size);
                out2[idx] = r;
            }
        }
    }
    if reduces() {
        fold_max(li, listed_partial(gid.x), abs(r));
    }
}

// One workgroup per 256 cells of the active slots: its partial sum of
// src · aux, tree-reduced in a fixed order into its slot's partial. The
// bodies path's p · s, whose s is finished by the body product after the
// operator pass.
@compute @workgroup_size(256, 1, 1)
fn dot_partial_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x);
    fold_sum(li, listed_partial(gid.x), src[idx] * aux[idx]);
}

// The `color` partials in order into this round's scalar `slot` (0: rz,
// 1: p·s); the round is the completed-round count in progress[1].
@compute @workgroup_size(256, 1, 1)
fn dot_finalize_main(@builtin(local_invocation_index) li: u32) {
    if stopped() {
        return;
    }
    let total = total_of_partials(li, u.color);
    if li == 0u {
        scalars[2u * u32(progress[1]) + u.slot] = total;
    }
}

// Iteration k's scalars: rz at 2k, p·s at 2k + 1.
fn ratio(top: f32, bottom: f32) -> f32 {
    return select(0.0, top / bottom, abs(bottom) >= DIVISOR_FLOOR);
}

// p = z (`src`) + β p in `out`, β = rz_k / rz_(k−1); the first iteration
// takes p = z and never reads p.
@compute @workgroup_size(256, 1, 1)
fn direction_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x);
    let k = u32(progress[1]);
    if k == 0u {
        out[idx] = src[idx];
        return;
    }
    let beta = ratio(scalars[2u * k], scalars[2u * k - 2u]);
    out[idx] = src[idx] + beta * out[idx];
}

// x −= α p into `out`, r −= α s into `out2`, α = rz_k / (p·s)_k; p in `src`,
// s in `aux`. x starts where init left it: zero, or the warm start's guess.
// With REDUCE each thread folds |r| of its cell into the workgroup's
// partial, for the stop's |r|∞.
@compute @workgroup_size(256, 1, 1)
fn update_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x);
    let k = u32(progress[1]);
    let alpha = ratio(scalars[2u * k], scalars[2u * k + 1u]);
    out[idx] = out[idx] - alpha * src[idx];
    let r = out2[idx] - alpha * aux[idx];
    out2[idx] = r;
    if reduces() {
        fold_max(li, listed_partial(gid.x), abs(r));
    }
}

// The stop, ported from FLIP Fluids (pcgsolver.h solveWithAdditionalMatrix,
// pressuresolver.cpp; MIT, see THIRD_PARTY_NOTICES.md): the residual's
// infinity norm |r|∞ in the right-hand side's units (the divergence, 1/s).
// A solve stops before its first iteration when |f|∞ is under START_FLOOR,
// or, started warm, when the guess's |r0|∞ already meets the rule below;
// and after iteration k when |r|∞ ≤ min(tolerance · |f|∞, ACCEPTABLE). The
// rule is relative to |f|∞, never |r0|∞, so a better guess stops sooner.
// progress: [0] |f|∞, [1] iterations run, [2] 1 once stopped by the
// tolerance, [4 + k] |r|∞ after iteration k. Stopping zeroes every gate
// triple (`triples` of them) and the range entries of every round from
// `first` on, so the solve's later dispatches run no groups direct or
// replayed. The norm's partials come folded from init (the start), warm_main
// (the warm start) or update.
const START_FLOOR: f32 = 1e-9;
const ACCEPTABLE: f32 = 1.0;
// The chunked rounds' range entries follow the per-round ones, one an
// execute (gpu_flip_pressure.rs chunk entries; manifold_gpu template_chunks).
const CHUNK_ENTRIES: u32 = 2u * ROUNDS;

// Execute j's rounds before the last cut: one each for the first `singles`,
// then 1, 2, 4, … doubling to `chunk` (manifold_gpu::chunk_rounds).
fn chunk_rounds(j: u32, chunk: u32, singles: u32) -> u32 {
    return min(1u << min(j - min(j, singles), 31u), max(chunk, 1u));
}
fn stop(first: u32) {
    progress[2] = 1.0;
    for (var t = 0u; t < u.triples; t = t + 1u) {
        gate[3u * t] = 0u;
    }
    for (var r = first; r < ROUNDS; r = r + 1u) {
        ranges[4u * r + 1u] = 0u;
        ranges[4u * r + 3u] = 0u;
    }
    // Every chunk that starts at or after `first`; the chunk running this
    // stop already read its range, and its later rounds write nothing.
    var start = 0u;
    for (var j = 0u; j < ROUNDS; j = j + 1u) {
        if start >= first {
            ranges[2u * (CHUNK_ENTRIES + j) + 1u] = 0u;
        }
        start = start + chunk_rounds(j, u.slot, u.singles);
    }
}

// The `color` partials' max; mode 1 is the start (|f|∞), WARM the warm
// start's |r0|∞ (its test, after the start's; nothing is recorded), else
// the round progress[1] counts: its |r|∞, the count advanced, and the stop
// test.
@compute @workgroup_size(256, 1, 1)
fn check_main(@builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    if stopped() {
        return;
    }
    let norm = max_of_partials(li, u.color);
    if li != 0u {
        return;
    }
    if (u.mode & 1u) == 1u {
        progress[0] = norm;
        progress[1] = 0.0;
        progress[2] = 0.0;
        if u.tolerance >= 0.0 && norm < START_FLOOR {
            stop(0u);
        }
        return;
    }
    if warm() {
        if progress[2] < 0.5 && u.tolerance >= 0.0 && norm <= min(u.tolerance * progress[0], ACCEPTABLE) {
            stop(0u);
        }
        return;
    }
    let k = u32(progress[1]);
    progress[4u + k] = norm;
    progress[1] = f32(k + 1u);
    if u.tolerance >= 0.0 && norm <= min(u.tolerance * progress[0], ACCEPTABLE) {
        stop(k + 1u);
    }
}

// The tick's solver words, after the step's capped records: [0] pressure
// iterations, [1] density iterations, [2] solves that reached their cap
// without meeting the tolerance. Mode 1 clears them first; `slot` is 0 or 1.
@compute @workgroup_size(1, 1, 1)
fn tally_main() {
    if slot_inactive() {
        return;
    }
    if u.mode == 1u {
        tally[0] = 0u;
        tally[1] = 0u;
        tally[2] = 0u;
    }
    tally[u.slot] = tally[u.slot] + u32(progress[1]);
    if u.tolerance >= 0.0 && progress[2] < 0.5 {
        tally[2] = tally[2] + 1u;
    }
}

// Word i of a live gate: the first `armed_triples` triples from `armed`,
// then the body passes' two (gpu_flip_pressure.rs Slots): the impulse
// partial over the fine level's live workgroups and `bodies` bodies, and the
// finalize over the bodies.
fn armed_word(i: u32) -> u32 {
    let triple = i / 3u;
    let word = i % 3u;
    if triple < u.armed_triples {
        return armed[i];
    }
    if triple == u.armed_triples {
        return select(select(1u, u.bodies, word == 1u), armed[0], word == 0u);
    }
    return select(1u, u.bodies, word == 0u);
}

// Re-arms the gate at the start of a solve: every one of the `triples`
// triples, and every round's two range entries live at `color` and `slot`
// commands, so the solve's dispatches run until the stop zeroes them. An
// inactive clock slot arms all of it off, so its solve runs no groups at
// all. A dispatch, not a blit, so it is the solver's own labelled pass.
@compute @workgroup_size(64, 1, 1)
fn arm_main(@builtin(local_invocation_index) lane: u32) {
    let off = slot_inactive();
    for (var i = lane; i < 3u * u.triples; i += 64u) {
        gate[i] = select(armed_word(i), 0u, off);
    }
    for (var r = lane; r < ROUNDS; r += 64u) {
        ranges[4u * r] = 0u;
        ranges[4u * r + 1u] = select(u.color, 0u, off);
        ranges[4u * r + 2u] = 0u;
        ranges[4u * r + 3u] = select(u.slot, 0u, off);
    }
    // The chunked executes of `mode` rounds, `live` at most an execute,
    // `color` commands a round; the last cut to what remains.
    if lane == 0u {
        var start = 0u;
        for (var j = 0u; j < ROUNDS; j = j + 1u) {
            let rounds = select(min(chunk_rounds(j, u.live, u.singles), u.mode - start), 0u, start >= u.mode);
            ranges[2u * (CHUNK_ENTRIES + j)] = 0u;
            ranges[2u * (CHUNK_ENTRIES + j) + 1u] = select(rounds * u.color, 0u, off);
            start = start + rounds;
        }
    }
}
