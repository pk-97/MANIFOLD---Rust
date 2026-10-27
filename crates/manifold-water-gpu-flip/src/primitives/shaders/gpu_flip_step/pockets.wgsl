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

// The gate word holding the held slots, the bound of the sweeps' lines.
const POCKET_HELD: u32 = 11u;

// Eight-byte aligned {first command, command count} for sweeps and setup.
const POCKET_RANGE: u32 = 12u;

const POCKET_ROUND_ARGS: u32 = 14u;

// The pocket count's word among the solver words `capped` is bound at.
const POCKET_WORD: u32 = 3u;

// The sums' removed total starts at word 0; the per-label words follow it.
const POCKET_TOTAL: u32 = 0u;

const POCKET_SUM_HEAD: u32 = 2u;

// Lines a tile starts along one axis: one per cell of a tile face.
const POCKET_LINES: u32 = 64u;

const NO_NEIGHBOUR: u32 = 0xffffffffu;

// Thread `gid` of a list dispatched at 512 threads a slot: the pool index of
// its cell. Arithmetic on the list alone, so a coarse level's list works
// beside the fine table.
fn pocket_list_cell(gid: u32) -> u32 {
    return ts_list_cell(tiles_by_ring[gid >> 9u], gid);
}

fn pocket_linked(c: vec3<i32>, d: vec3<i32>, a: i32) -> bool {
    // c and d are neighbours along a; the face between is the higher's low face.
    var f = c;
    f[a] = max(c[a], d[a]);
    return open_at(f, a) >= POCKET_LINK;
}

// Why water cell p touches air, as the seed decides it: the neighbour's pool
// index, the face's open fraction bits, and axis * 2 + (1 on the high side);
// z is 0xffffffff when it does not. A face open to a neighbour that holds no
// water is the only way: an Open Bounds face is no part of it, since water
// beyond its plane is removed and the cells beyond it hold none. The caller
// has set p's tile as its home.
fn pocket_seed_reason(p: vec3<i32>) -> vec4<u32> {
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            let near = ts_cell_at(d);
            if !(water[near] > 0.5) && pocket_linked(p, d, a) {
                var f = p;
                f[a] = max(p[a], d[a]);
                return vec4<u32>(near, bitcast<u32>(open_at(f, a)), u32(2 * a) + select(0u, 1u, s > 0), 0u);
            }
        }
    }
    return vec4<u32>(NO_NEIGHBOUR, 0u, NO_NEIGHBOUR, 0u);
}

// One thread per cell of every held slot: dry, sealed, or water touching
// air. Every held cell is written, so a tile that left the cell set holds no
// pocket from an earlier step.
@compute @workgroup_size(256)
fn pocket_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    ts_home(idx);
    let p = ts_cell_coord(idx);
    if !(water[idx] > 0.5) {
        pocket[idx] = POCKET_DRY;
        return;
    }
    // A label is the pool index of the pocket's lowest cell.
    pocket_label[idx] = idx;
    pocket[idx] = select(POCKET_SEALED, POCKET_AIR, pocket_seed_reason(p).z != NO_NEIGHBOUR);
}

// One thread: the first round runs, and the sweeps' bound is the held
// slots (the triple over every held slot, ring_max in, 512 threads a slot in
// 256-thread groups). In an inactive clock slot no round runs: every sweep's
// size is 0 and the rounds return.
@compute @workgroup_size(1)
fn pocket_start() {
    pocket_gate[POCKET_UNRESOLVED] = 0u;
    pocket_gate[POCKET_HELD] = tile_args[3u * u.ring_max] / 2u;
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
// cell. A sweep thread is one line of one held slot, 64 a slot, 256 a group.
@compute @workgroup_size(1)
fn pocket_round() {
    if !clock_active() { return; }
    let go = pocket_gate[POCKET_CHANGED] != 0u;
    pocket_gate[POCKET_RANGE] = 0u;
    pocket_gate[POCKET_RANGE + 1u] = select(0u, 4u, go);
    pocket_gate[POCKET_ROUND_ARGS] = select(0u, 1u, go);
    let groups = (pocket_gate[POCKET_HELD] * POCKET_LINES + 255u) / 256u;
    for (var a = 0u; a < 3u; a = a + 1u) {
        pocket_gate[3u * a] = select(0u, groups, go);
        pocket_gate[3u * a + 1u] = 1u;
        pocket_gate[3u * a + 2u] = 1u;
    }
    pocket_gate[POCKET_CHANGED] = 0u;
}

// Cell c from its line neighbour b: sealed water linked to water reaching
// air reaches air too; linked sealed water takes the lower label.
fn pocket_step(c: vec3<i32>, b: vec3<i32>, a: i32) -> bool {
    let at = ts_cell_at(c);
    let near = ts_cell_at(b);
    if pocket[at] != POCKET_SEALED || pocket[near] == POCKET_DRY || !pocket_linked(c, b, a) {
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

// One thread per line along axis a, 64 per held slot: the slot starts its
// lines when its tile on the low side of the axis is absent, and the line
// then walks through the neighbour rows, forward then back, until it leaves
// the held tiles. So a run of held tiles along the axis is swept whole by one
// thread a line, and each cell has one thread that writes it.
fn pocket_sweep(t: u32, a: i32) {
    if t / POCKET_LINES >= pocket_gate[POCKET_HELD] {
        return;
    }
    let first = tiles_by_ring[t / POCKET_LINES];
    var along = vec3<i32>(0);
    along[a] = 1;
    if ts_neighbour_slot(first, ts_neighbour_index(-along)) != WT_NONE {
        return;
    }
    // The line's place in each tile, on the two other axes.
    let line = t % POCKET_LINES;
    var local = vec3<i32>(0);
    local[(a + 1) % 3] = i32(line & 7u);
    local[(a + 2) % 3] = i32(line >> 3u);
    var changed = false;
    var slot = first;
    var last = first;
    for (var walked = 0u; walked < ts_pool_tiles(); walked = walked + 1u) {
        ts_home(slot * 512u);
        for (var i = 0; i < 8; i = i + 1) {
            local[a] = i;
            let c = ts_home_tile * 8 + local;
            changed = pocket_step(c, c - along, a) || changed;
        }
        last = slot;
        slot = ts_neighbour_slot(slot, ts_neighbour_index(along));
        if slot == WT_NONE {
            break;
        }
    }
    slot = last;
    for (var walked = 0u; walked < ts_pool_tiles(); walked = walked + 1u) {
        ts_home(slot * 512u);
        for (var i = 7; i >= 0; i = i - 1) {
            local[a] = i;
            let c = ts_home_tile * 8 + local;
            changed = pocket_step(c, c + along, a) || changed;
        }
        slot = ts_neighbour_slot(slot, ts_neighbour_index(-along));
        if slot == WT_NONE {
            break;
        }
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

// A sealed cell with a linked neighbour of the given state. The caller has
// set c's tile as its home.
fn pocket_neighbour(c: vec3<i32>, state: u32) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            if pocket[ts_cell_at(d)] == state && pocket_linked(c, d, a) {
                return true;
            }
        }
    }
    return false;
}

// One thread per cell of the cell set C, after the last round: a sealed cell
// still linked to one that reaches air means the spread hit its cap
// unfinished. Sealed water is always in C, so no other tile can hold it.
@compute @workgroup_size(256)
fn pocket_check(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    if pocket[idx] != POCKET_SEALED {
        return;
    }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    var unfinished = pocket_neighbour(c, POCKET_AIR);
    // A pocket still split between two labels: each part's mean is removed
    // apart, which keeps the solve consistent but spreads unevenly.
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            let near = ts_cell_at(d);
            if pocket[near] == POCKET_SEALED && pocket_label[near] != pocket_label[idx] && pocket_linked(c, d, a) {
                unfinished = true;
            }
        }
    }
    if unfinished {
        pocket_gate[POCKET_UNRESOLVED] = 1u;
    }
}

// One thread per cell of every held slot, `water` to `cell_out`: the
// solves' water, less each sealed pocket's leader cell. With no air a
// pocket's L is singular, and in f32 its residual never reaches the stop;
// the leader then stands as air at p = 0, an identity row, and its
// neighbours see it as a pressure-0 neighbour as the subtract pass does.
@compute @workgroup_size(256)
fn pocket_pin(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    let leader = pocket[idx] == POCKET_SEALED && pocket_label[idx] == idx;
    cell_out[idx] = select(water[idx], 0.0, leader);
}

// One thread per cell of a list (the cell set C, or a coarse level's slots):
// the sums of the cell's label start at 0. A label is a pool index of the
// list's own pool.
@compute @workgroup_size(256)
fn pocket_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = POCKET_SUM_HEAD + 3u * pocket_list_cell(gid.x);
    atomicStore(&pocket_sum[at], 0u);
    atomicStore(&pocket_sum[at + 1u], 0u);
    atomicStore(&pocket_sum[at + 2u], 0u);
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

// One thread per cell of a list, the right-hand side in `cell_out`: each
// sealed cell adds itself to its pocket. A workgroup first folds the cells
// sharing its first sealed cell's label, so a large pocket is not one
// contended word. The list is the cell set C for the fine lattice, a coarse
// level's slots for a level (its state, label and right-hand side bound in
// place of the fine ones).
@compute @workgroup_size(256)
fn pocket_accumulate(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    if !clock_active() { return; }
    if lane == 0u {
        group_label = 0xffffffffu;
        atomicStore(&group_sum[0], 0u);
        atomicStore(&group_sum[1], 0u);
        atomicStore(&group_sum[2], 0u);
    }
    workgroupBarrier();
    let idx = pocket_list_cell(gid.x);
    let sealed = pocket[idx] == POCKET_SEALED;
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
            let at = POCKET_SUM_HEAD + 3u * label;
            pocket_add(at, q);
            atomicAdd(&pocket_sum[at + 2u], 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u && shared_label != 0xffffffffu {
        let at = POCKET_SUM_HEAD + 3u * shared_label;
        pocket_add(at, vec2<u32>(atomicLoad(&group_sum[0]), atomicLoad(&group_sum[1])));
        atomicAdd(&pocket_sum[at + 2u], atomicLoad(&group_sum[2]));
    }
}

// One thread per cell of a list: a sealed cell's right-hand side less its
// pocket's mean, so each pocket sums to 0 and its pure-Neumann solve has a
// solution. The leader cell adds what its pocket lost to the removed total.
// The same lists and arrays as pocket_accumulate.
@compute @workgroup_size(256)
fn pocket_remove(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    if pocket[idx] != POCKET_SEALED {
        return;
    }
    let label = pocket_label[idx];
    let at = POCKET_SUM_HEAD + 3u * label;
    let low = atomicLoad(&pocket_sum[at]);
    let high = atomicLoad(&pocket_sum[at + 1u]);
    let count = atomicLoad(&pocket_sum[at + 2u]);
    let q = pocket_fixed(cell_out[idx]);
    cell_out[idx] = pocket_value(q.x, q.y) - pocket_value(low, high) / f32(count);
    if idx == label {
        let total = pocket_value(low, high);
        pocket_add(POCKET_TOTAL, pocket_fixed(abs(total)));
    }
}

// One thread, `capped` bound at the solver words: the volume rate removed
// from this step's pockets, h³ times the removed sum, added to `word`. The
// removed total goes back to 0 for the next solve.
fn pocket_flux(word: u32) {
    let total = pocket_value(atomicLoad(&pocket_sum[POCKET_TOTAL]), atomicLoad(&pocket_sum[POCKET_TOTAL + 1u]));
    atomicStore(&pocket_sum[POCKET_TOTAL], 0u);
    atomicStore(&pocket_sum[POCKET_TOTAL + 1u], 0u);
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
// unchanged. The level is a world-tiled lattice of its own (its pool, table
// and list), so its pockets are pooled fields indexed by the level's pool
// index.
@group(0) @binding(39) var<storage, read_write> pocket_coarse: array<u32>;

@group(0) @binding(40) var<storage, read_write> pocket_coarse_label: array<u32>;

// By fine label (a fine pool index): the lowest level cell of that pocket.
@group(0) @binding(41) var<storage, read_write> pocket_leader: array<atomic<u32>>;

// The level's table, read for nothing but its slots' tiles; the fine table
// stays the one `ts_*` reads. `entries` is the level table's H.
struct PocketLevel {
    entries: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(63) var<uniform> pocket_level: PocketLevel;

@group(0) @binding(64) var<storage, read> pocket_level_table: array<u32>;

const NO_LEADER: u32 = 0xffffffffu;

// The tile a held slot of the level holds (its slot row, as ts_slot_tile
// reads the fine table's).
fn pocket_level_tile(slot: u32) -> vec3<i32> {
    let row = 2u * pocket_level.entries + WT_SLOT_WORDS * slot;
    return bitcast<vec3<i32>>(vec3<u32>(pocket_level_table[row], pocket_level_table[row + 1u], pocket_level_table[row + 2u]));
}

// One thread per cell of the cell set C: no pocket has a leader yet.
@compute @workgroup_size(256)
fn pocket_leader_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    atomicStore(&pocket_leader[pocket_list_cell(gid.x)], NO_LEADER);
}

// A fine cell the solver coarsens as solid (gpu_flip_pressure.wgsl kind):
// not water, every face closed. It is no part of any pocket.
fn cell_solid(p: vec3<i32>) -> bool {
    if water[ts_cell_at(p)] > 0.5 {
        return false;
    }
    for (var a = 0; a < 3; a = a + 1) {
        var above = p;
        above[a] = p[a] + 1;
        if open_at(p, a) != 0.0 || open_at(above, a) != 0.0 {
            return false;
        }
    }
    return true;
}

// One thread per cell of every slot of the level (the level's list is
// bound): sealed with its children's fine label when every in-lattice child
// that is not solid is sealed under that one label, dry otherwise. The
// solver's level cell is water with solid children inside it; they carry no
// label and never break the seal. The block of children is read through the
// fine table, with its first cell's tile as the home.
@compute @workgroup_size(256)
fn pocket_coarsen(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    let side = 1 << u.solve_level;
    let base = (pocket_level_tile(idx >> 9u) * 8 + ts_local_coord(idx & 511u)) * side;
    ts_home(ts_cell_at(base));
    var sealed = true;
    var label = NO_LEADER;
    for (var z = 0; z < side && sealed; z = z + 1) {
        for (var y = 0; y < side && sealed; y = y + 1) {
            for (var x = 0; x < side && sealed; x = x + 1) {
                let p = base + vec3<i32>(x, y, z);
                if cell_solid(p) {
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

// One thread per cell of every slot of the level: a sealed cell takes its
// pocket's leader.
@compute @workgroup_size(256)
fn pocket_relabel(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    if pocket_coarse[idx] != POCKET_SEALED {
        return;
    }
    pocket_coarse_label[idx] = atomicLoad(&pocket_leader[pocket_coarse_label[idx]]);
}

// One thread per cell of the cell set C: an isolated cell is sealed water in
// a region of more than one cell (a lone sealed cell keeps its solid
// velocity, as the engine skips a group of one). Each of its six faces
// takes solid velocity 0, in place on `faces_out`. The high faces are the
// next cells' records; where that tile is not held there is no record to
// write.
@compute @workgroup_size(256)
fn pocket_condition(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = pocket_list_cell(gid.x);
    if pocket[idx] != POCKET_SEALED {
        return;
    }
    ts_home(idx);
    let p = ts_cell_coord(idx);
    if !pocket_neighbour(p, POCKET_SEALED) {
        return;
    }
    for (var a = 0; a < 3; a = a + 1) {
        faces_out[idx].face_velocity[a] = 0.0;
        var above = p;
        above[a] = p[a] + 1;
        let high = ts_face(above);
        if high != ts_canon() {
            faces_out[high].face_velocity[a] = 0.0;
        }
    }
}

// One thread, `capped` bound at the solver words: steps this tick whose
// spread hit its cap unfinished.
@compute @workgroup_size(1)
fn pocket_tally() {
    if !clock_active() { return; }
    let unresolved = pocket_gate[POCKET_UNRESOLVED];
    if u.step_in_tick == 0 {
        capped[POCKET_WORD] = unresolved;
    } else {
        capped[POCKET_WORD] = capped[POCKET_WORD] + unresolved;
    }
}
