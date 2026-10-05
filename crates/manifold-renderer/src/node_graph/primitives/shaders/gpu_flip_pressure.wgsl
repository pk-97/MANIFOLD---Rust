// GPU FLIP's pressure solver (gpu_flip_pressure.rs): the multigrid-
// preconditioned conjugate gradient for L p = f on the water, every pass a
// separate entry point. Lattices are x-fastest, cell (i, j, k) at
// i + nx·(j + ny·k). A face grid is padded to (n + 1)³ records; weight[a] is
// the open fraction of a cell's low face along axis a, box walls 0.
//
// A level halves each side rounding up. An odd side's extra coarse half is a
// virtual solid cell: it never makes a coarse cell air, its faces count 0,
// and the transfers drop its row, so restriction stays the transpose of
// prolongation over 8 and the V-cycle stays symmetric
// (scripts/mgpcg_reference.py --symmetry).
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
// The CPU sizes every buffer for the lattice before it dispatches; each pass
// only checks its thread is inside the lattice.
//
// Every level's lattice passes run over its active tiles
// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 7 (the pressure solve over
// tiles)): 8³ tiles, a tile active when a touched cell lies in its box grown
// by one cell, touched meaning water on the fine level and any touched child
// on a coarse one (classify_main). The list of active tiles in tile order
// (lists_main) covers 512 cells per tile, in two workgroups. The smoother
// pairs adjacent x cells in each thread; other passes use one thread per cell.
// Every read of a vector is inside the active tiles: the stencil reads water
// neighbours only, restriction reads one cell past coarse water's children,
// prolongation one coarse cell past a fine water cell's parent. The vectors
// outside the active tiles are never read; the pressure and r are written
// everywhere by init, so they stay the dense solve's zero there. A folded
// partial is indexed by its tile, and the finalize and check passes take an
// inactive tile's partial as 0, so the sums run the same tree whichever
// tiles are active.

struct Params {
    // This level's cells: the fine level of a transfer or coarsening.
    nx: u32,
    ny: u32,
    nz: u32,
    // The sweep's color, or the dot product's partial count.
    color: u32,
    // The coarse level's cells.
    cx: u32,
    cy: u32,
    cz: u32,
    // Bit 0: smooth starts from zero; residual has no rhs (−L value); check
    // is the start; restrict masks its fine taps to open water. Bit 1
    // (REDUCE): the pass also folds its per-cell product into one partial
    // per workgroup. Bit 2: restrict adds into `out`.
    mode: u32,
    cell_size: f32,
    // The conjugate gradient iteration, or the scalar a dot product writes.
    slot: u32,
    // smooth and residual: 1 reads φ (the finest level's ghost rows).
    ghost: u32,
    // check: the stop's relative tolerance; below 0 the solve never stops early.
    tolerance: f32,
    // The dispatched level's first word in `flags` and `lists`.
    list_base: u32,
    // The dispatched level's gate triple: its group count is armed[3 · level].
    level: u32,
    // classify: 1 marks every tile active (the test-only oracle).
    all_tiles: u32,
    // Inside a round: the solve-live triple (stopped); 0 outside rounds.
    live: u32,
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
// lists_main from its active tile count, then the one-group triple.
@group(0) @binding(14) var<storage, read_write> armed: array<u32>;
// The replayed rounds' range entries, {location, length} pairs, two a round
// (gpu_flip_pressure.rs Gate): a round's recorded dispatches run when its
// length is its command count and skip when it is 0.
@group(0) @binding(15) var<storage, read_write> ranges: array<u32>;
// One partial per workgroup of a folded reduction (the fine lattice's group
// count, no cap): the finalize and check passes sum or max them in order.
@group(0) @binding(16) var<storage, read_write> partials: array<f32>;
// A level's operator rows, assembled once at prepare (rows_main): the sweep
// and residual passes read a cell's row instead of its six faces, six
// neighbours' water and φ. Non-water cells hold zero rows.
@group(0) @binding(17) var<storage, read> rows: array<Row>;
@group(0) @binding(18) var<storage, read_write> out_rows: array<Row>;
// Per level from its list_base, one word a tile: 1 when the tile is active
// (classify_main), and the active tiles in tile order (lists_main).
@group(0) @binding(19) var<storage, read_write> flags: array<u32>;
@group(0) @binding(20) var<storage, read_write> lists: array<u32>;
// The GPU FLIP clock's plan for the step this solve runs in (gpu_flip_clock.wgsl
// Plan; word 0 step_dt, word 11 live_mode). A live slot with no time to step
// is inactive: arm zeroes every gate triple and round, so the gated passes
// run no groups, and every plain pass returns. Zeros (live_mode 0) are
// always active: a solve outside a live clock.
@group(0) @binding(21) var<storage, read> slot_plan: array<u32>;

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
// A weight is 0 where the neighbour is not water or past the box, so the
// neighbour sum skips exactly what the face stencil skipped.
struct Row {
    lo: vec4<f32>,
    hi: vec4<f32>,
};

const DIVISOR_FLOOR: f32 = 1e-30;
// Rounds a solve may run: the solver's MAX_ITERATIONS.
const ROUNDS: u32 = 64u;
const REDUCE: u32 = 2u;

var<workgroup> sums: array<f32, 256>;
var<workgroup> scan: array<u32, 256>;

const TILE: i32 = 8;
// A thread of an active tile whose cell runs past the lattice (a partial
// edge tile).
const NO_CELL: u32 = 0xffffffffu;

fn tile_dims(n: vec3<i32>) -> vec3<i32> {
    return (n + vec3<i32>(TILE - 1)) / TILE;
}

fn tile_total(n: vec3<i32>) -> u32 {
    let t = tile_dims(n);
    return u32(t.x * t.y * t.z);
}

// Cell `local` (0..512) of `tile`, flattened in the lattice, or NO_CELL.
fn tile_cell(tile: u32, local: u32, n: vec3<i32>) -> u32 {
    let p = coords(tile, tile_dims(n)) * TILE + coords(local, vec3<i32>(TILE));
    if any(p >= n) {
        return NO_CELL;
    }
    return cell(p, n);
}

// Whether thread gid's workgroup is inside the dispatched level's active
// list. A recorded dispatch runs every tile's two workgroups; the ones past
// the list return before any barrier, whole.
fn listed(gid: u32) -> bool {
    return (gid >> 8u) < armed[3u * u.level];
}

// Thread gid's cell on lattice n through the dispatched level's list.
fn listed_cell(gid: u32, n: vec3<i32>) -> u32 {
    return tile_cell(lists[u.list_base + (gid >> 9u)], gid & 511u, n);
}

// Thread gid's partial slot: its tile's two, by workgroup.
fn listed_partial(gid: u32) -> u32 {
    return 2u * lists[u.list_base + (gid >> 9u)] + ((gid >> 8u) & 1u);
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

// Partial g of the gradient's level (its flags from list_base), two a tile:
// 0 for an inactive tile, whose slots the fold never wrote.
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

fn lattice() -> vec3<i32> {
    return vec3<i32>(i32(u.nx), i32(u.ny), i32(u.nz));
}

fn coarse_lattice() -> vec3<i32> {
    return vec3<i32>(i32(u.cx), i32(u.cy), i32(u.cz));
}

fn coords(idx: u32, n: vec3<i32>) -> vec3<i32> {
    return vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
}

fn cell(p: vec3<i32>, n: vec3<i32>) -> u32 {
    return u32(p.x + n.x * (p.y + n.y * p.z));
}

fn is_water(at: u32) -> bool {
    return water[at] > 0.5;
}

// The open fraction of the face between p and its neighbour one step along
// axis a in direction d, 0 past the box.
fn face_weight(p: vec3<i32>, a: i32, d: i32, n: vec3<i32>) -> f32 {
    var q = p;
    q[a] = p[a] + d;
    if q[a] < 0 || q[a] >= n[a] {
        return 0.0;
    }
    var face = p;
    face[a] = max(p[a], q[a]);
    return faces[cell(face, n + vec3<i32>(1))].weight[a];
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

// One thread per cell: its row of L from its faces, its neighbours' water
// and, with `ghost`, φ. The sums run in the face order −x, +x, −y, +y, −z,
// +z, the order the stencil summed in before the rows were assembled, so a
// solve on rows is bit for bit the solve that recomputed them. A cell that
// is not water, or has no open face, is a zero row.
@compute @workgroup_size(256, 1, 1)
fn rows_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    var row = Row(vec4<f32>(0.0), vec4<f32>(0.0));
    if is_water(idx) {
        let p = coords(idx, n);
        var ghost = 0.0;
        var plain = 0.0;
        for (var k = 0u; k < 6u; k = k + 1u) {
            let a = i32(k / 2u);
            let d = select(-1, 1, (k & 1u) == 1u);
            let w = face_weight(p, a, d, n);
            if w > 0.0 {
                ghost = ghost + w;
                plain = plain + w;
                var q = p;
                q[a] = p[a] + d;
                let at = cell(q, n);
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

// A zero weight must not read its neighbour (which may be outside the box).
// Keep the six additions in the original face order without a dynamic loop
// or dynamic vector-component indexing in each smoothing/operator pass.
fn stencil_add(sum: f32, source: u32, weight: f32, at: u32) -> f32 {
    if !(weight > 0.0) {
        return sum;
    }
    if source == 0u {
        return sum + weight * out[at];
    }
    return sum + weight * aux[at];
}

fn stencil(idx: u32, source: u32) -> Stencil {
    let row = rows[idx];
    var s = Stencil(select(row.hi.w, row.hi.z, u.ghost == 1u), 0.0);
    if source == 2u {
        return s;
    }
    s.sum = stencil_add(s.sum, source, row.lo.x, idx - 1u);
    s.sum = stencil_add(s.sum, source, row.lo.y, idx + 1u);
    s.sum = stencil_add(s.sum, source, row.lo.z, idx - u.nx);
    s.sum = stencil_add(s.sum, source, row.lo.w, idx + u.nx);
    s.sum = stencil_add(s.sum, source, row.hi.x, idx - u.nx * u.ny);
    s.sum = stencil_add(s.sum, source, row.hi.y, idx + u.nx * u.ny);
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
    let n = lattice();
    // Tile origins are multiples of 8 and pair.x is even, so y/z alone
    // determine which member has the requested global checkerboard color.
    let offset = u.color ^ (((pair >> 3u) ^ (pair >> 6u)) & 1u);
    let idx = listed_cell(pair + offset, n);
    let other = listed_cell(pair + (offset ^ 1u), n);
    var product = 0.0;
    if idx != NO_CELL {
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
        product = src[idx] * e;
    }
    var other_product = 0.0;
    if other != NO_CELL {
        if from_zero() {
            out[other] = 0.0;
        }
        if reduces() {
            other_product = src[other] * out[other];
        }
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

// rhs − L value in a water cell with an open face, else 0; rhs in `src`
// (0 with mode bit 0, which makes −L value), value in `aux`. With REDUCE
// (the operator product s = −L p with no bodies) each thread also folds
// value · result (p · s) into the workgroup's partial.
@compute @workgroup_size(256, 1, 1)
fn residual_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x, lattice());
    var product = 0.0;
    if idx != NO_CELL {
        var result = 0.0;
        if is_water(idx) {
            let s = stencil(idx, 1u);
            if s.diagonal > 0.0 {
                let rhs = select(src[idx], 0.0, from_zero());
                result = rhs - (s.sum - s.diagonal * aux[idx]) / (u.cell_size * u.cell_size);
            }
        }
        out[idx] = result;
        product = aux[idx] * result;
    }
    if reduces() {
        fold_sum(li, listed_partial(gid.x), product);
    }
}

// The share fine cell f takes from coarse cell c along one axis in
// prolongation: 3/4 from its parent, 1/4 from the parent's neighbour on its
// side, clamped to the coarse lattice.
fn transfer_weight(f: i32, c: i32, coarse: i32) -> f32 {
    let parent = f / 2;
    let other = clamp(select(parent - 1, parent + 1, f % 2 == 1), 0, coarse - 1);
    return select(0.0, 0.75, parent == c) + select(0.0, 0.25, other == c);
}

// The transfers are the trilinear prolongation and its transpose of McAdams,
// Sifakis & Teran 2010 (the form `scripts/mgpcg_reference.py` mirrors); the
// FLIP Fluids engine is PCG+MIC(0) and has no transfer form to port. Each
// workgroup is one half tile (8×8×4 cells); it stages the taps its cells read
// in workgroup memory once, then every cell gathers from the stage in the
// same order with the same weights as a direct gather, so the sums are
// bitwise those of the gather kernels. Entries past the lattice are never
// read (the gather skips them); they are zeroed so every slot is defined.

// Fine taps of a coarse half tile: 2×8+2 by 2×8+2 by 2×4+2.
const RESTRICT_FOOT: vec3<i32> = vec3<i32>(18, 18, 10);
const RESTRICT_FOOT_CELLS: u32 = 3240u;
var<workgroup> fine_patch: array<f32, 3240>;

// Coarse taps of a fine half tile: 8/2+2 by 8/2+2 by 4/2+2.
const PROLONG_FOOT: vec3<i32> = vec3<i32>(6, 6, 4);
const PROLONG_FOOT_CELLS: u32 = 144u;
var<workgroup> coarse_patch: array<f32, 144>;

// The first cell of thread gid's half tile on lattice n.
fn half_tile_origin(gid: u32, n: vec3<i32>) -> vec3<i32> {
    let tile = lists[u.list_base + (gid >> 9u)];
    let half = i32((gid >> 8u) & 1u);
    return coords(tile, tile_dims(n)) * TILE + vec3<i32>(0, 0, 4 * half);
}

// Whether fine cell `at` keeps its right-hand side at the gradient's start
// (init_main): water with an open face.
fn open_water(at: u32, n: vec3<i32>) -> bool {
    if !is_water(at) {
        return false;
    }
    let p = coords(at, n);
    var diagonal = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        diagonal = diagonal + face_weight(p, a, -1, n) + face_weight(p, a, 1, n);
    }
    return diagonal > 0.0;
}

// One thread per coarse cell of the coarse level's active tiles: the fine
// residual in `src` restricted by the transpose of prolongation over 8,
// masked to coarse water. Fine cells past an odd side are the virtual
// solid: not read. Mode bit 0 (RESTRICT_MASK) takes a fine tap only from
// open water, as init takes the right-hand side, so a solve started on a
// coarse level restricts what the fine one would have started from; bit 2
// (RESTRICT_ADD) adds the sum to `out` instead of writing it and leaves
// non-water cells alone. The unlisted return precedes the barrier, whole.
@compute @workgroup_size(256, 1, 1)
fn restrict_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() || !listed(gid.x) {
        return;
    }
    let n = lattice();
    let c = coarse_lattice();
    let masked = (u.mode & 1u) != 0u;
    let adds = (u.mode & 4u) != 0u;
    let origin = half_tile_origin(gid.x, c);
    let base = 2 * origin - vec3<i32>(1);
    for (var k = gid.x & 255u; k < RESTRICT_FOOT_CELLS; k = k + 256u) {
        let q = base + coords(k, RESTRICT_FOOT);
        var v = 0.0;
        if all(q >= vec3<i32>(0)) && all(q < n) {
            let at = cell(q, n);
            if !masked || open_water(at, n) {
                v = src[at];
            }
        }
        fine_patch[k] = v;
    }
    workgroupBarrier();
    let idx = listed_cell(gid.x, c);
    if idx == NO_CELL {
        return;
    }
    if !(coarse_water[idx] > 0.5) {
        if !adds {
            out[idx] = 0.0;
        }
        return;
    }
    let p = coords(idx, c);
    let at = 2 * (p - origin) + vec3<i32>(1);
    var sum = 0.0;
    for (var dz = -1; dz <= 2; dz = dz + 1) {
        let fz = 2 * p.z + dz;
        if fz < 0 || fz >= n.z {
            continue;
        }
        let wz = transfer_weight(fz, p.z, c.z);
        for (var dy = -1; dy <= 2; dy = dy + 1) {
            let fy = 2 * p.y + dy;
            if fy < 0 || fy >= n.y {
                continue;
            }
            let wy = wz * transfer_weight(fy, p.y, c.y);
            for (var dx = -1; dx <= 2; dx = dx + 1) {
                let fx = 2 * p.x + dx;
                if fx < 0 || fx >= n.x {
                    continue;
                }
                let tap = fine_patch[cell(at + vec3<i32>(dx, dy, dz), RESTRICT_FOOT)];
                sum = sum + wy * transfer_weight(fx, p.x, c.x) * tap;
            }
        }
    }
    if adds {
        out[idx] = out[idx] + sum / 8.0;
    } else {
        out[idx] = sum / 8.0;
    }
}

// One thread per cell of the lattice: `out` to 0. The target of a
// prolongation chain, so a prolonged vector is 0 off the water.
@compute @workgroup_size(256, 1, 1)
fn zero_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !slot_inactive() && !stopped() && gid.x < u.nx * u.ny * u.nz {
        out[gid.x] = 0.0;
    }
}

// One thread per fine cell: in a water cell, e in `out` plus the coarse
// correction in `src` interpolated trilinearly. A fine cell's parent is
// always inside the coarse lattice, odd sides included. The unlisted return
// precedes the barrier, whole.
@compute @workgroup_size(256, 1, 1)
fn prolong_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() || !listed(gid.x) {
        return;
    }
    let n = lattice();
    let c = coarse_lattice();
    let origin = half_tile_origin(gid.x, n);
    let base = origin / 2 - vec3<i32>(1);
    let k = gid.x & 255u;
    if k < PROLONG_FOOT_CELLS {
        let q = base + coords(k, PROLONG_FOOT);
        var v = 0.0;
        if all(q >= vec3<i32>(0)) && all(q < c) {
            v = src[cell(q, c)];
        }
        coarse_patch[k] = v;
    }
    workgroupBarrier();
    let idx = listed_cell(gid.x, n);
    if idx == NO_CELL || !is_water(idx) {
        return;
    }
    let f = coords(idx, n);
    let parent = f / 2;
    let odd = (f % vec3<i32>(2)) == vec3<i32>(1);
    let other = clamp(select(parent - vec3<i32>(1), parent + vec3<i32>(1), odd), vec3<i32>(0), c - vec3<i32>(1));
    var sum = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let pick = vec3<bool>((corner & 1) != 0, (corner & 2) != 0, (corner & 4) != 0);
        let w = select(vec3<f32>(0.75), vec3<f32>(0.25), pick);
        let tap = coarse_patch[cell(select(parent, other, pick) - base, PROLONG_FOOT)];
        sum = sum + w.x * w.y * w.z * tap;
    }
    out[idx] = out[idx] + sum;
}

// A cell's kind in `water`: 1 water, 0 air, −1 solid. The fine level's mask
// (u.level 0) carries no solid kind: there a cell with every face closed is
// solid (a cell inside a body or a wall), any other non-water cell air.
const KIND_AIR: u32 = 0u;
const KIND_WATER: u32 = 1u;
const KIND_SOLID: u32 = 2u;

fn kind(p: vec3<i32>, n: vec3<i32>) -> u32 {
    let at = cell(p, n);
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
        if face_weight(p, a, -1, n) != 0.0 || face_weight(p, a, 1, n) != 0.0 {
            return KIND_AIR;
        }
    }
    return KIND_SOLID;
}

// One thread per coarse cell: the cell-type coarsening of McAdams, Sifakis
// and Teran 2010 into `out` — air (0) when any fine child inside the fine
// lattice is air, solid (−1) when every one is solid, else water (1); a
// child past an odd side is the virtual solid. Touched (1) into `out2` when
// any child is touched in `aux` (the fine level's water, or the level
// above's touched).
@compute @workgroup_size(256, 1, 1)
fn coarsen_water_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    let c = coarse_lattice();
    let idx = gid.x;
    if idx >= u.cx * u.cy * u.cz {
        return;
    }
    let p = 2 * coords(idx, c);
    var any_air = false;
    var all_solid = true;
    var touched = false;
    for (var child = 0; child < 8; child = child + 1) {
        let q = p + vec3<i32>(child & 1, (child >> 1u) & 1, (child >> 2u) & 1);
        if all(q < n) {
            let k = kind(q, n);
            any_air = any_air || k == KIND_AIR;
            all_solid = all_solid && k == KIND_SOLID;
            if aux[cell(q, n)] > 0.5 {
                touched = true;
            }
        }
    }
    out[idx] = select(select(1.0, -1.0, all_solid), 0.0, any_air);
    out2[idx] = select(0.0, 1.0, touched);
}

// 32 lanes per tile of this level: active (1) when a touched cell (`water`
// binds the level's touched mask) lies in the tile's box grown by one cell,
// into flags from list_base; every tile with `all_tiles`.
@compute @workgroup_size(256, 1, 1)
fn classify_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    // Eight tiles per workgroup, with 32 lanes sharing each tile's 10³ halo.
    // Invalid edge tiles contribute zero and still reach every barrier.
    let t = gid.x >> 5u;
    let lane = gid.x & 31u;
    let valid = t < tile_total(n);
    var lit = valid && u.all_tiles != 0u;
    if valid && !lit {
        let origin = coords(t, tile_dims(n)) * TILE;
        for (var k = lane; k < 1000u && !lit; k = k + 32u) {
            let p = origin - vec3<i32>(1) + coords(k, vec3<i32>(10));
            if all(p >= vec3<i32>(0)) && all(p < n) {
                lit = is_water(cell(p, n));
            }
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
        flags[u.list_base + t] = scan[li];
    }
}

// One workgroup per level: the level's active tiles in tile order into
// lists from list_base, and the level's gate triple into armed: two
// workgroups a tile. Each thread counts a run of tiles, the workgroup scans
// the counts (integers, so the order is exact), and each thread writes its
// run's tiles at its offset. Level 0 also writes the one-group triple at
// `slot`.
@compute @workgroup_size(256, 1, 1)
fn lists_main(@builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    let total = tile_total(n);
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

// Test-only: NaN into every cell of this level's inactive tiles in `out`
// (a vector) and the level's two partials per inactive tile, so a solve
// that reads outside its active set shows in its pressure. Never named
// outside the proofs (gpu_flip_pressure_tests.rs).
@compute @workgroup_size(256, 1, 1)
fn poison_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let tile = gid.x >> 9u;
    if tile >= tile_total(n) || flags[u.list_base + tile] != 0u {
        return;
    }
    let nan = bitcast<f32>(0x7fc00000u);
    let idx = tile_cell(tile, gid.x & 511u, n);
    if idx != NO_CELL {
        out[idx] = nan;
    }
    if (gid.x & 511u) == 0u && u.color == 1u {
        partials[2u * tile] = nan;
        partials[2u * tile + 1u] = nan;
    }
}

// One thread per padded coarse face record: each coarse face's open fraction
// is the mean of the four fine faces it covers; a fine face past an odd side
// is the virtual solid's and counts 0. Box walls and the padding are 0,
// velocity 0.
@compute @workgroup_size(256, 1, 1)
fn coarsen_faces_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    let c = coarse_lattice();
    let m = c + vec3<i32>(1);
    let idx = gid.x;
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return;
    }
    var record = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let p = coords(idx, m);
    for (var a = 0; a < 3; a = a + 1) {
        var across = p;
        across[a] = 0;
        if !all(across < c) || p[a] == 0 || p[a] == c[a] {
            continue;
        }
        var sum = 0.0;
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
                sum = sum + faces[cell(q, n + vec3<i32>(1))].weight[a];
            }
        }
        record.weight[a] = 0.25 * sum;
    }
    out_faces[idx] = record;
}

// Lentine, Zheng & Fedkiw (2010), sections 3.2--3.3:
// https://physbam.stanford.edu/papers/stanford2010-02.pdf
// Conservative outer-projection gather; the MG gather stays unchanged.
// faces: solid weights/volumes; src: fluid FaceSample words; aux: solid
// velocity FaceSample words. velocity.xyz stores fluid flux / coarse area;
// velocity.w stores the complete child solid source / coarse volume,
// including internal (c_low - c_high) v_s terms. weight.xyz is open area.
@compute @workgroup_size(256, 1, 1)
fn lentine_flux_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let c = coarse_lattice();
    let m = c + vec3<i32>(1);
    let idx = gid.x;
    if idx >= u32(m.x * m.y * m.z) {
        return;
    }
    let p = coords(idx, m);
    let fm = n + vec3<i32>(1);
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
                let face = cell(q, fm);
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
            let q = 2 * p + coords(k, vec3<i32>(2));
            if any(q >= n) || !(water[cell(q, n)] > 0.5) {
                continue;
            }
            let open_volume = faces[cell(q, fm)].weight.w;
            volume = volume + open_volume;
            for (var a = 0; a < 3; a = a + 1) {
                for (var side = 0; side < 2; side = side + 1) {
                    var f = q;
                    f[a] = f[a] + side;
                    if f[a] == 0 || f[a] == n[a] {
                        continue;
                    }
                    let face = cell(f, fm);
                    let sign = f32(2 * side - 1);
                    solid_source = solid_source + sign *
                        (open_volume - faces[face].weight[a]) * aux[8u * face + u32(a)];
                }
            }
        }
        record.velocity.w = solid_source / (8.0 * u.cell_size);
        record.weight.w = volume * 0.125;
    }
    out_faces[idx] = record;
}

// The coarsest level's exact solve, e = −h² A⁻¹ rhs: the inverse in `src`
// (cells² row-major, from coarse_inverse.wgsl), rhs in `aux`, e to `out`.
@compute @workgroup_size(64, 1, 1)
fn coarse_solve_main(@builtin(local_invocation_index) i: u32) {
    if stopped() {
        return;
    }
    let cells = u.nx * u.ny * u.nz;
    if i >= cells {
        return;
    }
    var sum = 0.0;
    for (var j = 0u; j < cells; j = j + 1u) {
        sum = sum + src[i * cells + j] * aux[j];
    }
    out[i] = -u.cell_size * u.cell_size * sum;
}

// The conjugate gradient's start, over every tile (512 threads a tile in
// tile order, no list): r = f (`src`) on water with an open face, else 0,
// into `out`, and the pressure x = 0 into `out2`, so a solve that stops
// before its first iteration leaves zero pressure, and both are the dense
// solve's zero outside the active tiles, which the solve never writes. The
// ghost rows only add to a diagonal, so an open face is the whole test.
// With REDUCE each thread folds |r| into its tile's partial, for the
// start's |f|∞.
@compute @workgroup_size(256, 1, 1)
fn init_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if slot_inactive() {
        return;
    }
    let n = lattice();
    let tile = gid.x >> 9u;
    if tile >= tile_total(n) {
        return;
    }
    let idx = tile_cell(tile, gid.x & 511u, n);
    var r = 0.0;
    if idx != NO_CELL {
        var diagonal = 0.0;
        if is_water(idx) {
            let p = coords(idx, n);
            for (var a = 0; a < 3; a = a + 1) {
                diagonal = diagonal + face_weight(p, a, -1, n) + face_weight(p, a, 1, n);
            }
        }
        r = select(0.0, src[idx], diagonal > 0.0);
        out[idx] = r;
        out2[idx] = 0.0;
    }
    if reduces() {
        fold_max(li, 2u * tile + ((gid.x >> 8u) & 1u), abs(r));
    }
}

// One workgroup per 256 cells of the active tiles: its partial sum of
// src · aux, tree-reduced in a fixed order into its tile's partial. The
// bodies path's p · s, whose s is finished by the body product after the
// operator pass.
@compute @workgroup_size(256, 1, 1)
fn dot_partial_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x, lattice());
    var product = 0.0;
    if idx != NO_CELL {
        product = src[idx] * aux[idx];
    }
    fold_sum(li, listed_partial(gid.x), product);
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
    let idx = listed_cell(gid.x, lattice());
    if idx == NO_CELL {
        return;
    }
    let k = u32(progress[1]);
    if k == 0u {
        out[idx] = src[idx];
        return;
    }
    let beta = ratio(scalars[2u * k], scalars[2u * k - 2u]);
    out[idx] = src[idx] + beta * out[idx];
}

// x −= α p into `out`, r −= α s into `out2`, α = rz_k / (p·s)_k; p in `src`,
// s in `aux`. The first iteration starts x from zero and never reads it.
// With REDUCE each thread folds |r| of its cell into the workgroup's
// partial, for the stop's |r|∞.
@compute @workgroup_size(256, 1, 1)
fn update_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let idx = listed_cell(gid.x, lattice());
    var r = 0.0;
    if idx != NO_CELL {
        let k = u32(progress[1]);
        let alpha = ratio(scalars[2u * k], scalars[2u * k + 1u]);
        let x = select(out[idx], 0.0, k == 0u);
        out[idx] = x - alpha * src[idx];
        r = out2[idx] - alpha * aux[idx];
        out2[idx] = r;
    }
    if reduces() {
        fold_max(li, listed_partial(gid.x), abs(r));
    }
}

// The stop, ported from FLIP Fluids (pcgsolver.h solveWithAdditionalMatrix,
// pressuresolver.cpp; MIT, see THIRD_PARTY_NOTICES.md): the residual's
// infinity norm |r|∞ in the right-hand side's units (the divergence, 1/s).
// A solve stops before its first iteration when |f|∞ is under START_FLOOR,
// and after iteration k when |r|∞ ≤ min(tolerance · |f|∞, ACCEPTABLE).
// progress: [0] |f|∞, [1] iterations run, [2] 1 once stopped by the
// tolerance, [4 + k] |r|∞ after iteration k. Stopping zeroes every gate
// triple (`cx` of them) and the range entries of every round from `first`
// on, so the solve's later dispatches run no groups direct or replayed.
// The norm's partials come folded from init (the start) or update.
const START_FLOOR: f32 = 1e-9;
const ACCEPTABLE: f32 = 1.0;
const KEEP_RANGES: u32 = 8u;

fn stop(first: u32) {
    progress[2] = 1.0;
    for (var t = 0u; t < u.cx; t = t + 1u) {
        gate[3u * t] = 0u;
    }
    // KEEP_RANGES (the proofs' lever) leaves later rounds executing, as a
    // chunk does, so the guards must make them write nothing.
    if (u.mode & KEEP_RANGES) != 0u {
        return;
    }
    for (var r = first; r < ROUNDS; r = r + 1u) {
        ranges[4u * r + 1u] = 0u;
        ranges[4u * r + 3u] = 0u;
    }
}

// The `color` partials' max; mode 1 is the start (|f|∞), else the round
// progress[1] counts: its |r|∞, the count advanced, and the stop test.
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

// Word i of a live gate: the first `cy` triples from `armed`, then the body
// passes' two (gpu_flip_pressure.rs Slots): the impulse partial over the
// fine level's live workgroups and `cz` bodies, and the finalize over the
// bodies.
fn armed_word(i: u32) -> u32 {
    let triple = i / 3u;
    let word = i % 3u;
    if triple < u.cy {
        return armed[i];
    }
    if triple == u.cy {
        return select(select(1u, u.cz, word == 1u), armed[0], word == 0u);
    }
    return select(1u, u.cz, word == 0u);
}

// Re-arms the gate at the start of a solve: every one of the `cx` triples,
// and every round's two range entries live at `color` and `slot` commands,
// so the solve's dispatches run until the stop zeroes them. An inactive
// clock slot arms all of it off, so its solve runs no groups at all.
// A dispatch, not a blit, so it is the solver's own labelled pass.
@compute @workgroup_size(64, 1, 1)
fn arm_main(@builtin(local_invocation_index) lane: u32) {
    let off = slot_inactive();
    for (var i = lane; i < 3u * u.cx; i += 64u) {
        gate[i] = select(armed_word(i), 0u, off);
    }
    for (var r = lane; r < ROUNDS; r += 64u) {
        ranges[4u * r] = 0u;
        ranges[4u * r + 1u] = select(u.color, 0u, off);
        ranges[4u * r + 2u] = 0u;
        ranges[4u * r + 3u] = select(u.slot, 0u, off);
    }
}
