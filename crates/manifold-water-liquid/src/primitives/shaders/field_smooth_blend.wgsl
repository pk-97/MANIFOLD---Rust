// node.field_smooth_blend — one step of the water field's smoothing over the
// nodes of a world-tile pool (`buf_tiles`, `pool_tiles` tiles), one workgroup
// per slot and one more for the canonical element. Element p is node p.
//
// The step smooths `prior` along x, then y, then z, blends the result back
// toward `prior` by the node's weight, and holds it within a leash of
// `reference`:
//   out = prior                                         where |reference| >= band
//   out = clamp(prior + w · (smoothed − prior), reference ± leash)   elsewhere
// with band = band_cells and leash = smoothness · FIELD_LEASH_CELLS, both in
// simulation cells, so every Surface Detail smooths the same water.
//
// Each axis pass is the [1, 2, 1] / 4 filter between neighbouring nodes. With
// `stride` display nodes per simulation cell above 1, that pass is followed
// by the same filter between nodes a simulation cell apart, so the smoothing
// reaches as far in metres at every Surface Detail; the first pass keeps
// patterns finer than the stride out of the second. Each filter sums its
// taps in the order −1, 0, +1.
//
// The workgroup stages its tile and `halo` nodes round it (1 at stride 1,
// stride + 1 above) through its 27 neighbour tiles, then runs each filter
// one node per thread, from one workgroup array into the other: each pass
// writes only the box of nodes the next one reads, so the boxes shrink to
// the tile as the passes go and the two arrays fit the widest halo. A node in
// a tile the table does not hold reads the canonical element of `prior`, and
// each filter leaves it at the far value, FIELD_FAR_CELLS (3) display cells,
// as a pass over the whole pool would. A free slot and the canonical element
// hold the far value.

struct Params {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
    // Display nodes per simulation cell along an axis, 1 to 3.
    stride: u32,
    sim_cell_size: f32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> prior: array<f32>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
@group(0) @binding(3) var<storage, read> reference: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> field: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.entries; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

const FS_THREADS: u32 = 256u;
// field_build::FIELD_LEASH_CELLS.
const FS_LEASH_CELLS: f32 = 0.5;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const FS_FAR_CELLS: f32 = 3.0;
var<workgroup> fs_held: u32;
// The slots of the 27 tiles round this one, by neighbour index.
var<workgroup> fs_near: array<u32, 27>;
// The passes' boxes, alternately: `prior` over the staged region (side 8 + 2
// · halo) in fs_a, the x pass between neighbours over all but the region's
// two end columns in fs_b, and each later box smaller. The host sizes both
// for the stride its pipeline runs (16³ and 14 · 16² at stride 3), so a
// finer stride holds no workgroup memory it does not use.
const FS_A_LEN: u32 = 4096u;
const FS_B_LEN: u32 = 3584u;
var<workgroup> fs_a: array<f32, FS_A_LEN>;
var<workgroup> fs_b: array<f32, FS_B_LEN>;

// A box of the staged region: `lo` its first node and `ext` its size, x
// fastest.
struct FsBox {
    lo: vec3<u32>,
    ext: vec3<u32>,
}

// The staged region's halo and side.
fn fs_halo() -> u32 { return select(params.stride + 1u, 1u, params.stride == 1u); }
fn fs_side() -> u32 { return 8u + 2u * fs_halo(); }

fn fs_at(b: FsBox, r: vec3<u32>) -> u32 {
    let q = r - b.lo;
    return q.x + b.ext.x * (q.y + b.ext.y * q.z);
}

fn fs_node(b: FsBox, i: u32) -> vec3<u32> {
    return b.lo + vec3<u32>(i % b.ext.x, (i / b.ext.x) % b.ext.y, i / (b.ext.x * b.ext.y));
}

fn fs_load(in_a: bool, b: FsBox, r: vec3<u32>) -> f32 {
    if in_a {
        return fs_a[fs_at(b, r)];
    }
    return fs_b[fs_at(b, r)];
}

// The slot of the tile region node r lies in, WT_NONE when it is not held.
fn fs_slot(r: vec3<u32>) -> u32 {
    let halo = fs_halo();
    let d = select(vec3<i32>(0), vec3<i32>(-1), r < vec3<u32>(halo))
        + select(vec3<i32>(0), vec3<i32>(1), r >= vec3<u32>(halo + 8u));
    return fs_near[ts_neighbour_index(d)];
}

// One [1, 2, 1] / 4 pass along `dir` between nodes `step` apart, from `src`
// (in fs_a when `from_a`) over `dst` into the other array. The taps sum in
// the order −step, 0, +step.
fn fs_pass(from_a: bool, src: FsBox, dst: FsBox, dir: vec3<u32>, step: u32, far: f32, lane: u32) {
    let d = dir * step;
    for (var i = lane; i < dst.ext.x * dst.ext.y * dst.ext.z; i = i + FS_THREADS) {
        let r = fs_node(dst, i);
        var v = far;
        if fs_slot(r) != WT_NONE {
            v = 0.25 * fs_load(from_a, src, r - d);
            v = v + 0.5 * fs_load(from_a, src, r);
            v = v + 0.25 * fs_load(from_a, src, r + d);
        }
        if from_a {
            fs_b[fs_at(dst, r)] = v;
        } else {
            fs_a[fs_at(dst, r)] = v;
        }
    }
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    let h = params.cell_size;
    let far = FS_FAR_CELLS * h;
    if lane == 0u {
        fs_held = u32(slot < params.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        if slot == params.pool_tiles {
            field[ts_canon()] = far;
        }
    }
    if workgroupUniformLoad(&fs_held) == 0u {
        if slot < params.pool_tiles {
            field[slot * 512u + lane] = far;
            field[slot * 512u + lane + FS_THREADS] = far;
        }
        return;
    }
    if lane < WT_NEIGHBOURS {
        fs_near[lane] = ts_neighbour_slot(slot, lane);
    }
    workgroupBarrier();
    let s = params.stride;
    let halo = fs_halo();
    let side = fs_side();
    let low = ts_slot_tile(slot) * 8 - vec3<i32>(i32(halo));
    let canon = prior[ts_canon()];
    for (var i = lane; i < side * side * side; i = i + FS_THREADS) {
        let r = vec3<u32>(i % side, (i / side) % side, i / (side * side));
        let n = fs_slot(r);
        var v = canon;
        if n != WT_NONE {
            v = prior[n * 512u + ts_local(low + vec3<i32>(r))];
        }
        fs_a[i] = v;
    }
    workgroupBarrier();
    // Each pass reads `src` and writes `dst` in the other array. The pass
    // between neighbours covers every node the stride pass after it reads.
    var src = FsBox(vec3<u32>(0u), vec3<u32>(side));
    var in_a = true;
    var dst = FsBox(vec3<u32>(1u, 0u, 0u), vec3<u32>(side - 2u, side, side));
    fs_pass(in_a, src, dst, vec3<u32>(1u, 0u, 0u), 1u, far, lane);
    workgroupBarrier();
    src = dst;
    in_a = !in_a;
    if s > 1u {
        dst = FsBox(vec3<u32>(halo, 0u, 0u), vec3<u32>(8u, side, side));
        fs_pass(in_a, src, dst, vec3<u32>(1u, 0u, 0u), s, far, lane);
        workgroupBarrier();
        src = dst;
        in_a = !in_a;
    }
    dst = FsBox(vec3<u32>(halo, 1u, 0u), vec3<u32>(8u, side - 2u, side));
    fs_pass(in_a, src, dst, vec3<u32>(0u, 1u, 0u), 1u, far, lane);
    workgroupBarrier();
    src = dst;
    in_a = !in_a;
    if s > 1u {
        dst = FsBox(vec3<u32>(halo, halo, 0u), vec3<u32>(8u, 8u, side));
        fs_pass(in_a, src, dst, vec3<u32>(0u, 1u, 0u), s, far, lane);
        workgroupBarrier();
        src = dst;
        in_a = !in_a;
    }
    dst = FsBox(vec3<u32>(halo, halo, 1u), vec3<u32>(8u, 8u, side - 2u));
    fs_pass(in_a, src, dst, vec3<u32>(0u, 0u, 1u), 1u, far, lane);
    workgroupBarrier();
    src = dst;
    in_a = !in_a;
    if s > 1u {
        dst = FsBox(vec3<u32>(halo), vec3<u32>(8u));
        fs_pass(in_a, src, dst, vec3<u32>(0u, 0u, 1u), s, far, lane);
        workgroupBarrier();
        src = dst;
        in_a = !in_a;
    }
    let band = params.band_cells * params.sim_cell_size;
    let leash = params.smoothness * FS_LEASH_CELLS * params.sim_cell_size;
    for (var local = lane; local < 512u; local = local + FS_THREADS) {
        let r = vec3<u32>(local & 7u, (local >> 3u) & 7u, local >> 6u) + vec3<u32>(halo);
        let smoothed = fs_load(in_a, src, r);
        let e = slot * 512u + local;
        let before = prior[e];
        let anchor = reference[e];
        var out = before;
        if abs(anchor) < band {
            out = clamp(before + weights[e] * (smoothed - before), anchor - leash, anchor + leash);
        }
        field[e] = out;
    }
}
