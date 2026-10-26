// Fixed point for the pocket sums: integer adds give the same total in any
// order, so the solves stay the same on every run.
const POCKET_SCALE: f32 = 65536.0;

const TWO_32: f32 = 4294967296.0;

const POCKET_DRY: u32 = 0u;

const POCKET_SEALED: u32 = 1u;

const POCKET_AIR: u32 = 2u;

const POCKET_LINK: f32 = 1e-6;

const POCKET_CHANGED: u32 = 9u;

const POCKET_UNRESOLVED: u32 = 10u;

// Eight-byte aligned {first command, command count} for sweeps and setup.
const POCKET_RANGE: u32 = 12u;

const POCKET_ROUND_ARGS: u32 = 14u;

// The pocket count's word among the solver words `capped` is bound at.
const POCKET_WORD: u32 = 3u;

fn pocket_linked(c: vec3<i32>, d: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> bool {
    // c and d are neighbours along a; the face between is the higher's low face.
    var f = c;
    f[a] = max(c[a], d[a]);
    return open_at(f, a, n, m) >= POCKET_LINK;
}

// One thread per cell: dry, sealed, or water touching air.
@compute @workgroup_size(256)
fn pocket_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_cell_count() {
        return;
    }
    if !(water[idx] > 0.5) {
        pocket[idx] = POCKET_DRY;
        return;
    }
    pocket_label[idx] = idx;
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let p = ts_cell_coord(idx);
    var air = false;
    for (var a = 0; a < 3; a = a + 1) {
        let low_open = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high_open = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if (low_open && p[a] == 0) || (high_open && p[a] == n[a] - 1) {
            air = true;
        }
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[ts_cell_at(d)] > 0.5) && pocket_linked(p, d, a, n, m) {
                air = true;
            }
        }
    }
    pocket[idx] = select(POCKET_SEALED, POCKET_AIR, air);
}

// One thread: the first round runs. In an inactive clock slot no round
// runs: every sweep's size is 0 and the rounds return.
@compute @workgroup_size(1)
fn pocket_start() {
    pocket_gate[POCKET_UNRESOLVED] = 0u;
    pocket_gate[POCKET_RANGE] = 0u;
    pocket_gate[POCKET_RANGE + 1u] = 0u;
    pocket_gate[POCKET_ROUND_ARGS] = 0u;
    pocket_gate[POCKET_ROUND_ARGS + 1u] = 1u;
    pocket_gate[POCKET_ROUND_ARGS + 2u] = 1u;
    if !clock_active() {
        pocket_gate[POCKET_CHANGED] = 0u;
        for (var a = 0u; a < 3u; a = a + 1u) {
            pocket_gate[3u * a] = 0u;
        }
        return;
    }
    pocket_gate[POCKET_CHANGED] = 1u;
}

// One thread: this round's sweeps run only when the last round changed a
// cell.
@compute @workgroup_size(1)
fn pocket_round() {
    if !clock_active() { return; }
    let go = pocket_gate[POCKET_CHANGED] != 0u;
    pocket_gate[POCKET_RANGE] = 0u;
    pocket_gate[POCKET_RANGE + 1u] = select(0u, 4u, go);
    pocket_gate[POCKET_ROUND_ARGS] = select(0u, 1u, go);
    let n = u.n;
    let lines = vec3<u32>(n.y * n.z, n.z * n.x, n.x * n.y);
    for (var a = 0u; a < 3u; a = a + 1u) {
        pocket_gate[3u * a] = select(0u, (lines[a] + 255u) / 256u, go);
        pocket_gate[3u * a + 1u] = 1u;
        pocket_gate[3u * a + 2u] = 1u;
    }
    pocket_gate[POCKET_CHANGED] = 0u;
}

// Cell c from its line neighbour b: sealed water linked to water reaching
// air reaches air too; linked sealed water takes the lower label.
fn pocket_step(c: vec3<i32>, b: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> bool {
    let at = ts_cell_at(c);
    let near = ts_cell_at(b);
    if pocket[at] != POCKET_SEALED || pocket[near] == POCKET_DRY || !pocket_linked(c, b, a, n, m) {
        return false;
    }
    if pocket[near] == POCKET_AIR {
        pocket[at] = POCKET_AIR;
        return true;
    }
    if pocket_label[near] < pocket_label[at] {
        pocket_label[at] = pocket_label[near];
        return true;
    }
    return false;
}

// One thread per line along axis a, forward then back; each line is its
// thread's alone.
fn pocket_sweep(t: u32, a: i32) {
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let axes = cross_axes(a);
    if t >= u32(n[axes.x] * n[axes.y]) {
        return;
    }
    var p = vec3<i32>(0);
    p[axes.x] = i32(t % u32(n[axes.x]));
    p[axes.y] = i32(t / u32(n[axes.x]));
    var changed = false;
    for (var i = 1; i < n[a]; i = i + 1) {
        var c = p;
        c[a] = i;
        var b = p;
        b[a] = i - 1;
        changed = pocket_step(c, b, a, n, m) || changed;
    }
    for (var i = n[a] - 2; i >= 0; i = i - 1) {
        var c = p;
        c[a] = i;
        var b = p;
        b[a] = i + 1;
        changed = pocket_step(c, b, a, n, m) || changed;
    }
    if changed {
        pocket_gate[POCKET_CHANGED] = 1u;
    }
}

@compute @workgroup_size(256)
fn pocket_sweep_x(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 0);
}

@compute @workgroup_size(256)
fn pocket_sweep_y(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 1);
}

@compute @workgroup_size(256)
fn pocket_sweep_z(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 2);
}

// A sealed cell with a linked neighbour of the given state.
fn pocket_neighbour(c: vec3<i32>, state: u32, n: vec3<i32>, m: vec3<i32>) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            if d[a] >= 0 && d[a] < n[a] && pocket[ts_cell_at(d)] == state && pocket_linked(c, d, a, n, m) {
                return true;
            }
        }
    }
    return false;
}

// One thread per cell, after the last round: a sealed cell still linked to
// one that reaches air means the spread hit its cap unfinished.
@compute @workgroup_size(256)
fn pocket_check(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_cell_count() {
        return;
    }
    let n = ts_cells();
    let c = ts_cell_coord(idx);
    if pocket[idx] != POCKET_SEALED {
        return;
    }
    let m = n + vec3<i32>(1);
    var unfinished = pocket_neighbour(c, POCKET_AIR, n, m);
    // A pocket still split between two labels: each part's mean is removed
    // apart, which keeps the solve consistent but spreads unevenly.
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            if d[a] >= 0 && d[a] < n[a] && pocket[ts_cell_at(d)] == POCKET_SEALED && pocket_label[ts_cell_at(d)] != pocket_label[idx] && pocket_linked(c, d, a, n, m) {
                unfinished = true;
            }
        }
    }
    if unfinished {
        pocket_gate[POCKET_UNRESOLVED] = 1u;
    }
}

// One thread per cell, `water` to `cell_out`: the solves' water, less each
// sealed pocket's leader cell. With no air a pocket's L is singular, and in
// f32 its residual never reaches the stop; the leader then stands as air at
// p = 0, an identity row, and its neighbours see it as a pressure-0
// neighbour as the subtract pass does.
@compute @workgroup_size(256)
fn pocket_pin(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_cell_count() {
        return;
    }
    let leader = pocket[idx] == POCKET_SEALED && pocket_label[idx] == idx;
    cell_out[idx] = select(water[idx], 0.0, leader);
}

// One thread per word: the pocket sums start at 0.
@compute @workgroup_size(256)
fn pocket_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if gid.x < 3u * ts_cell_count() + 2u {
        atomicStore(&pocket_sum[gid.x], 0u);
    }
}

// A cell's right-hand side in the sums' fixed point, as (low, high) of a
// 64-bit two's-complement integer. The value the solve then sees is this
// rounded one, so a pocket sums to exactly its removed mean.
fn pocket_fixed(x: f32) -> vec2<u32> {
    let v = round(x * POCKET_SCALE);
    // Split the magnitude, which is exact for an integral f32, then negate.
    let m = abs(v);
    let mh = floor(m / TWO_32);
    let q = vec2<u32>(u32(m - mh * TWO_32), u32(mh));
    return select(q, pocket_negate(q), v < 0.0);
}

fn pocket_negate(q: vec2<u32>) -> vec2<u32> {
    let low = ~q.x + 1u;
    return vec2<u32>(low, ~q.y + select(0u, 1u, low == 0u));
}

// Decoded through the magnitude, so a small negative keeps its precision.
fn pocket_value(low: u32, high: u32) -> f32 {
    let negative = (high & 0x80000000u) != 0u;
    let m = select(vec2<u32>(low, high), pocket_negate(vec2<u32>(low, high)), negative);
    let magnitude = (f32(m.y) * TWO_32 + f32(m.x)) / POCKET_SCALE;
    return select(magnitude, -magnitude, negative);
}

// Adds (low, high) at word w with the carry; the total is exact in any order.
fn pocket_add(w: u32, q: vec2<u32>) {
    let old = atomicAdd(&pocket_sum[w], q.x);
    let carry = select(0u, 1u, old + q.x < old);
    atomicAdd(&pocket_sum[w + 1u], q.y + carry);
}

var<workgroup> group_label: u32;

var<workgroup> group_sum: array<atomic<u32>, 3>;

// One thread per cell, the right-hand side in `cell_out`: each sealed cell
// adds itself to its pocket. A workgroup first folds the cells sharing its
// first sealed cell's label, so a large pocket is not one contended word.
@compute @workgroup_size(256)
fn pocket_accumulate(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    if !clock_active() { return; }
    let idx = gid.x;
    if lane == 0u {
        group_label = 0xffffffffu;
        atomicStore(&group_sum[0], 0u);
        atomicStore(&group_sum[1], 0u);
        atomicStore(&group_sum[2], 0u);
    }
    workgroupBarrier();
    let sealed = idx < ts_cell_count() && pocket[idx] == POCKET_SEALED;
    var label = 0u;
    if sealed {
        label = pocket_label[idx];
        // Any one sealed lane's label wins; the rest go straight to memory.
        group_label = label;
    }
    workgroupBarrier();
    let shared_label = workgroupUniformLoad(&group_label);
    if sealed {
        let q = pocket_fixed(cell_out[idx]);
        if label == shared_label {
            let old = atomicAdd(&group_sum[0], q.x);
            let carry = select(0u, 1u, old + q.x < old);
            atomicAdd(&group_sum[1], q.y + carry);
            atomicAdd(&group_sum[2], 1u);
        } else {
            pocket_add(3u * label, q);
            atomicAdd(&pocket_sum[3u * label + 2u], 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u && shared_label != 0xffffffffu {
        pocket_add(3u * shared_label, vec2<u32>(atomicLoad(&group_sum[0]), atomicLoad(&group_sum[1])));
        atomicAdd(&pocket_sum[3u * shared_label + 2u], atomicLoad(&group_sum[2]));
    }
}

// One thread per cell: a sealed cell's right-hand side less its pocket's
// mean, so each pocket sums to 0 and its pure-Neumann solve has a solution.
// The leader cell adds what its pocket lost to the removed total.
@compute @workgroup_size(256)
fn pocket_remove(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_cell_count() || pocket[idx] != POCKET_SEALED {
        return;
    }
    let label = pocket_label[idx];
    let low = atomicLoad(&pocket_sum[3u * label]);
    let high = atomicLoad(&pocket_sum[3u * label + 1u]);
    let count = atomicLoad(&pocket_sum[3u * label + 2u]);
    let q = pocket_fixed(cell_out[idx]);
    cell_out[idx] = pocket_value(q.x, q.y) - pocket_value(low, high) / f32(count);
    if idx == label {
        let total = pocket_value(low, high);
        pocket_add(3u * ts_cell_count(), pocket_fixed(abs(total)));
    }
}

// One thread, `capped` bound at the solver words: the volume rate removed
// from this step's pockets, h³ times the removed sum, added to `word`.
fn pocket_flux(word: u32) {
    let at = 3u * ts_cell_count();
    let total = pocket_value(atomicLoad(&pocket_sum[at]), atomicLoad(&pocket_sum[at + 1u]));
    let h = u.cell_size;
    let removed = total * h * h * h;
    if u.step_in_tick == 0 {
        capped[word] = bitcast<u32>(removed);
    } else {
        capped[word] = bitcast<u32>(bitcast<f32>(capped[word]) + removed);
    }
}

@compute @workgroup_size(1)
fn pocket_flux_pressure() {
    if !clock_active() { return; }
    // The density word too: a tick without the density projection reads 0.
    if u.step_in_tick == 0 {
        capped[5u] = 0u;
    }
    pocket_flux(4u);
}

@compute @workgroup_size(1)
fn pocket_flux_density() {
    if !clock_active() { return; }
    pocket_flux(5u);
}

// Solve Level (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11): the pockets at
// the solve's level. A level cell is sealed when every fine cell under it
// is sealed with one label; its label is the lowest level cell of that
// pocket, so the pocket passes run over the level's right-hand side
// unchanged.
@group(0) @binding(39) var<storage, read_write> pocket_coarse: array<u32>;

@group(0) @binding(40) var<storage, read_write> pocket_coarse_label: array<u32>;

// By fine label: the lowest level cell of that pocket.
@group(0) @binding(41) var<storage, read_write> pocket_leader: array<atomic<u32>>;

const NO_LEADER: u32 = 0xffffffffu;

// The lattice `k` halvings down, each rounding up: the solver's levels.
fn level_lattice(k: u32) -> vec3<i32> {
    var n = ts_cells();
    for (var j = 0u; j < k; j = j + 1u) {
        n = (n + 1) / 2;
    }
    return n;
}

fn level_total(k: u32) -> u32 {
    let c = level_lattice(k);
    return u32(c.x * c.y * c.z);
}

// One thread per fine cell: no pocket has a leader yet.
@compute @workgroup_size(256)
fn pocket_leader_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if gid.x < ts_cell_count() {
        atomicStore(&pocket_leader[gid.x], NO_LEADER);
    }
}

// A fine cell the solver coarsens as solid (gpu_flip_pressure.wgsl kind):
// not water, every face closed. It is no part of any pocket.
fn cell_solid(p: vec3<i32>, n: vec3<i32>, m: vec3<i32>) -> bool {
    if water[ts_cell_at(p)] > 0.5 {
        return false;
    }
    for (var a = 0; a < 3; a = a + 1) {
        var above = p;
        above[a] = p[a] + 1;
        if open_at(p, a, n, m) != 0.0 || open_at(above, a, n, m) != 0.0 {
            return false;
        }
    }
    return true;
}

// One thread per level cell: sealed with its children's fine label when
// every in-lattice child that is not solid is sealed under that one label,
// dry otherwise. The solver's level cell is water with solid children
// inside it; they carry no label and never break the seal.
@compute @workgroup_size(256)
fn pocket_coarsen(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    let k = u.solve_level;
    if idx >= level_total(k) {
        return;
    }
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let c = level_lattice(k);
    let side = 1 << k;
    let base = ts_grid_coord(idx, c) * side;
    var sealed = true;
    var label = NO_LEADER;
    for (var z = 0; z < side && sealed; z = z + 1) {
        for (var y = 0; y < side && sealed; y = y + 1) {
            for (var x = 0; x < side && sealed; x = x + 1) {
                let p = base + vec3<i32>(x, y, z);
                if any(p >= n) || cell_solid(p, n, m) {
                    continue;
                }
                let f = ts_cell_at(p);
                if pocket[f] != POCKET_SEALED {
                    sealed = false;
                } else if label == NO_LEADER {
                    label = pocket_label[f];
                } else if pocket_label[f] != label {
                    sealed = false;
                }
            }
        }
    }
    // A level cell of solid children only is dry: it has no label to seal under.
    sealed = sealed && label != NO_LEADER;
    pocket_coarse[idx] = select(POCKET_DRY, POCKET_SEALED, sealed);
    pocket_coarse_label[idx] = label;
    if sealed {
        atomicMin(&pocket_leader[label], idx);
    }
}

// One thread per level cell: a sealed cell takes its pocket's leader.
@compute @workgroup_size(256)
fn pocket_relabel(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= level_total(u.solve_level) || pocket_coarse[idx] != POCKET_SEALED {
        return;
    }
    pocket_coarse_label[idx] = atomicLoad(&pocket_leader[pocket_coarse_label[idx]]);
}

// A cell of a sealed region of more than one cell; a lone sealed cell keeps
// its solid velocity, as the engine skips a group of one.
fn pocket_isolated(c: vec3<i32>, n: vec3<i32>, m: vec3<i32>) -> bool {
    return pocket[ts_cell_at(c)] == POCKET_SEALED && pocket_neighbour(c, POCKET_SEALED, n, m);
}

// One thread per face record, in place on the solid velocity in
// `faces_out`: each of the six faces of an isolated cell takes velocity 0.
@compute @workgroup_size(256)
fn pocket_condition(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_record_count() {
        return;
    }
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let p = ts_face_coord(idx);
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) {
            continue;
        }
        var below = p;
        below[a] = p[a] - 1;
        if (p[a] < n[a] && pocket_isolated(p, n, m)) || (p[a] > 0 && pocket_isolated(below, n, m)) {
            faces_out[idx].face_velocity[a] = 0.0;
        }
    }
}

// The solver words holding the step's dry, sealed and air cell counts.
const POCKET_COUNT_WORD: u32 = 7u;

var<workgroup> pocket_counts: array<atomic<u32>, 3>;

var<workgroup> pocket_first_seed: atomic<u32>;

var<workgroup> pocket_dry_floor: atomic<u32>;

const POCKET_DRY_FLOOR_WORD: u32 = 16u;

// A floor cell (y = 0) reading dry with water in every in-box face neighbour:
// a hole under the water, not its edge.
fn dry_floor_hole(idx: u32) -> bool {
    let n = ts_cells();
    let p = ts_cell_coord(idx);
    if p.y != 0 || water[idx] > 0.5 {
        return false;
    }
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[ts_cell_at(d)] > 0.5) {
                return false;
            }
        }
    }
    return true;
}

const POCKET_SEED_WORD: u32 = 10u;

// Why water cell idx touches air, as pocket_seed decides it: the neighbour's
// index (0xffffffff for an open box face), the face's open fraction bits,
// and axis * 2 + (1 on the high side); x is 0xffffffff when it does not.
fn pocket_seed_reason(idx: u32) -> vec4<u32> {
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let p = ts_cell_coord(idx);
    for (var a = 0; a < 3; a = a + 1) {
        let low_open = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high_open = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if low_open && p[a] == 0 {
            return vec4<u32>(0xffffffffu, 0u, u32(2 * a), 0u);
        }
        if high_open && p[a] == n[a] - 1 {
            return vec4<u32>(0xffffffffu, 0u, u32(2 * a + 1), 0u);
        }
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[ts_cell_at(d)] > 0.5) && pocket_linked(p, d, a, n, m) {
                var f = p;
                f[a] = max(p[a], d[a]);
                return vec4<u32>(ts_cell_at(d), bitcast<u32>(open_at(f, a, n, m)), u32(2 * a) + select(0u, 1u, s > 0), 0u);
            }
        }
    }
    return vec4<u32>(0xffffffffu, 0u, 0xffffffffu, 0u);
}

// One workgroup, `capped` bound at the solver words: steps this tick whose
// spread hit its cap unfinished, and the step's dry, sealed and air cells.
@compute @workgroup_size(256)
fn pocket_tally(@builtin(local_invocation_index) lane: u32) {
    if !clock_active() { return; }
    if lane < 3u {
        atomicStore(&pocket_counts[lane], 0u);
    }
    if lane == 0u {
        atomicStore(&pocket_first_seed, 0xffffffffu);
        atomicStore(&pocket_dry_floor, 0u);
    }
    workgroupBarrier();
    var counts = vec3<u32>(0u);
    for (var idx = lane; idx < ts_cell_count(); idx = idx + 256u) {
        counts[min(pocket[idx], 2u)] += 1u;
        if pocket[idx] == POCKET_AIR && pocket_seed_reason(idx).z != 0xffffffffu {
            atomicMin(&pocket_first_seed, idx);
        }
        if dry_floor_hole(idx) {
            atomicAdd(&pocket_dry_floor, 1u);
        }
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        atomicAdd(&pocket_counts[k], counts[k]);
    }
    workgroupBarrier();
    if lane != 0u {
        return;
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        capped[POCKET_COUNT_WORD + k] = atomicLoad(&pocket_counts[k]);
    }
    capped[POCKET_DRY_FLOOR_WORD] = atomicLoad(&pocket_dry_floor);
    let seed = atomicLoad(&pocket_first_seed);
    capped[POCKET_SEED_WORD] = seed;
    if seed != 0xffffffffu {
        let why = pocket_seed_reason(seed);
        capped[POCKET_SEED_WORD + 1u] = why.x;
        capped[POCKET_SEED_WORD + 2u] = why.y;
        capped[POCKET_SEED_WORD + 3u] = why.z;
        if why.x != 0xffffffffu {
            capped[POCKET_SEED_WORD + 4u] = bitcast<u32>(phi[why.x]);
            capped[POCKET_SEED_WORD + 5u] = ranges[why.x].count;
        }
    }
    let unresolved = pocket_gate[POCKET_UNRESOLVED];
    if u.step_in_tick == 0 {
        capped[POCKET_WORD] = unresolved;
    } else {
        capped[POCKET_WORD] = capped[POCKET_WORD] + unresolved;
    }
}
