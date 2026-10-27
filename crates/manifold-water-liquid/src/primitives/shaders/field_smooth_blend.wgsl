// node.field_smooth_blend — one step of the water field's smoothing over the
// nodes of a world-tile pool (`buf_tiles`, `pool_tiles` tiles), one workgroup
// per slot and one more for the canonical element. Element p is node p.
//
// The step smooths `prior` with the [1, 2, 1] / 4 filter along x, then y,
// then z, blends the result back toward `prior` by the node's weight, and
// holds it within a leash of `reference`:
//   out = prior                                         where |reference| >= band
//   out = clamp(prior + w · (smoothed − prior), reference ± leash)   elsewhere
// with band = band_cells cells and leash = smoothness · FIELD_LEASH_CELLS
// cells. Each filter pass sums its taps in the order −1, 0, +1.
//
// The workgroup stages its tile and one node round it (10³ nodes) through its
// 27 neighbour tiles. A node in a tile the table does not hold reads the
// canonical element of `prior` and leaves every filter pass at the far value,
// FIELD_FAR_CELLS (3) cells, as a pass over the whole pool would. A free slot
// and the canonical element hold the far value.

struct Params {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
// The staged region: the tile and one node round it, 10 a side.
const FS_SIDE: u32 = 10u;
const FS_REGION: u32 = 1000u;

var<workgroup> fs_held: u32;
// The slots of the 27 tiles round this one, by neighbour index.
var<workgroup> fs_near: array<u32, 27>;
// `prior` over the region, then the x pass over x 1..8, then the y pass over
// x and y 1..8; the z pass and the blend run on the tile's own nodes.
var<workgroup> fs_prior: array<f32, 1000>;
var<workgroup> fs_x: array<f32, 1000>;
var<workgroup> fs_y: array<f32, 1000>;

fn fs_coord(i: u32) -> vec3<u32> {
    return vec3<u32>(i % FS_SIDE, (i / FS_SIDE) % FS_SIDE, i / (FS_SIDE * FS_SIDE));
}

// The slot of the tile region node r lies in, WT_NONE when it is not held.
fn fs_slot(r: vec3<u32>) -> u32 {
    let d = select(vec3<i32>(0), vec3<i32>(-1), r == vec3<u32>(0u))
        + select(vec3<i32>(0), vec3<i32>(1), r == vec3<u32>(FS_SIDE - 1u));
    return fs_near[ts_neighbour_index(d)];
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
    let low = ts_slot_tile(slot) * 8 - vec3<i32>(1);
    let canon = prior[ts_canon()];
    for (var i = lane; i < FS_REGION; i = i + FS_THREADS) {
        let r = fs_coord(i);
        let s = fs_slot(r);
        var v = canon;
        if s != WT_NONE {
            v = prior[s * 512u + ts_local(low + vec3<i32>(r))];
        }
        fs_prior[i] = v;
    }
    workgroupBarrier();
    for (var i = lane; i < FS_REGION; i = i + FS_THREADS) {
        let r = fs_coord(i);
        if r.x == 0u || r.x == FS_SIDE - 1u {
            continue;
        }
        var v = far;
        if fs_slot(r) != WT_NONE {
            v = 0.25 * fs_prior[i - 1u];
            v = v + 0.5 * fs_prior[i];
            v = v + 0.25 * fs_prior[i + 1u];
        }
        fs_x[i] = v;
    }
    workgroupBarrier();
    for (var i = lane; i < FS_REGION; i = i + FS_THREADS) {
        let r = fs_coord(i);
        if r.x == 0u || r.x == FS_SIDE - 1u || r.y == 0u || r.y == FS_SIDE - 1u {
            continue;
        }
        var v = far;
        if fs_slot(r) != WT_NONE {
            v = 0.25 * fs_x[i - FS_SIDE];
            v = v + 0.5 * fs_x[i];
            v = v + 0.25 * fs_x[i + FS_SIDE];
        }
        fs_y[i] = v;
    }
    workgroupBarrier();
    let band = params.band_cells * h;
    let leash = params.smoothness * FS_LEASH_CELLS * h;
    for (var local = lane; local < 512u; local = local + FS_THREADS) {
        let r = vec3<u32>(local & 7u, (local >> 3u) & 7u, local >> 6u) + vec3<u32>(1u);
        let i = r.x + FS_SIDE * (r.y + FS_SIDE * r.z);
        var smoothed = 0.25 * fs_y[i - FS_SIDE * FS_SIDE];
        smoothed = smoothed + 0.5 * fs_y[i];
        smoothed = smoothed + 0.25 * fs_y[i + FS_SIDE * FS_SIDE];
        let e = slot * 512u + local;
        let before = fs_prior[i];
        let anchor = reference[e];
        var out = before;
        if abs(anchor) < band {
            out = clamp(before + weights[e] * (smoothed - before), anchor - leash, anchor + leash);
        }
        field[e] = out;
    }
}
