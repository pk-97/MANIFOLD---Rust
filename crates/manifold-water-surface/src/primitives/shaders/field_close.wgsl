// node.field_close — one half of a morphological closing of the liquid's
// level set over the nodes of a world-tile pool (`buf_tiles`, `pool_tiles`
// tiles), one workgroup per slot and one more for the canonical element.
// Element p is node p.
//
// With `widest` 0 each node takes the least value of the nodes within a
// simulation cell of it (`stride` display nodes each way, a cube): the
// liquid grown by a simulation cell. With `widest` 1 it takes the greatest
// (shrunk by one). The node runs the grow, then the shrink on what it grew.
// Holes and gaps through the liquid up to two simulation cells across close
// at every Surface Detail, and every surface the grow leaves open comes back
// where it was. Each pass is separable: the extreme along x, then y, then z.
//
// The workgroup stages its tile and `stride` nodes round it through its 27
// neighbour tiles, then takes the extremes one row of the staged region per
// thread, in place. A node in a tile the table does not hold takes no part:
// it stages as the far value, FIELD_FAR_CELLS (3) display cells, while
// growing and its negation while shrinking, so it is never the extreme. A
// free slot and the canonical element hold the far value.

struct Params {
    cell_size: f32,
    pool_tiles: u32,
    entries: u32,
    // 0 takes the least value round each node, 1 the greatest.
    widest: u32,
    // Display nodes per simulation cell along an axis, 1 to 3.
    stride: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> field: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.entries; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

const FC_THREADS: u32 = 256u;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const FC_FAR_CELLS: f32 = 3.0;

var<workgroup> fc_held: u32;
// The slots of the 27 tiles round this one, by neighbour index.
var<workgroup> fc_near: array<u32, 27>;
// `src` over the staged region, side 8 + 2 · stride, x fastest; each axis
// pass overwrites the rows it reduces.
var<workgroup> fc_region: array<f32, 2744>;

fn fc_side() -> u32 { return 8u + 2u * params.stride; }

fn fc_index(r: vec3<u32>) -> u32 {
    let side = fc_side();
    return r.x + side * (r.y + side * r.z);
}

// The slot of the tile region node r lies in, WT_NONE when it is not held.
fn fc_slot(r: vec3<u32>) -> u32 {
    let s = params.stride;
    let d = select(vec3<i32>(0), vec3<i32>(-1), r < vec3<u32>(s))
        + select(vec3<i32>(0), vec3<i32>(1), r >= vec3<u32>(s + 8u));
    return fc_near[ts_neighbour_index(d)];
}

// The extreme of two values: the least while growing, the greatest while
// shrinking.
fn fc_pick(a: f32, b: f32) -> f32 {
    if params.widest == 1u {
        return max(a, b);
    }
    return min(a, b);
}

// One axis pass over a staged row, in place: `start` is the row's first
// node and `dir` the unit step along the axis; the tile's own nodes along it
// take the extreme of the nodes within `stride` of them, in the order −s..s.
fn fc_reduce_row(start: vec3<u32>, dir: vec3<u32>) {
    let side = fc_side();
    let s = params.stride;
    var row: array<f32, 14>;
    for (var j = 0u; j < side; j = j + 1u) {
        row[j] = fc_region[fc_index(start + j * dir)];
    }
    for (var j = s; j < s + 8u; j = j + 1u) {
        var v = row[j - s];
        for (var k = j - s + 1u; k <= j + s; k = k + 1u) {
            v = fc_pick(v, row[k]);
        }
        fc_region[fc_index(start + j * dir)] = v;
    }
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    let far = FC_FAR_CELLS * params.cell_size;
    let absent = select(far, -far, params.widest == 1u);
    if lane == 0u {
        fc_held = u32(slot < params.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        if slot == params.pool_tiles {
            field[ts_canon()] = far;
        }
    }
    if workgroupUniformLoad(&fc_held) == 0u {
        if slot < params.pool_tiles {
            field[slot * 512u + lane] = far;
            field[slot * 512u + lane + FC_THREADS] = far;
        }
        return;
    }
    if lane < WT_NEIGHBOURS {
        fc_near[lane] = ts_neighbour_slot(slot, lane);
    }
    workgroupBarrier();
    let s = params.stride;
    let side = fc_side();
    let low = ts_slot_tile(slot) * 8 - vec3<i32>(i32(s));
    for (var i = lane; i < side * side * side; i = i + FC_THREADS) {
        let r = vec3<u32>(i % side, (i / side) % side, i / (side * side));
        let n = fc_slot(r);
        var v = absent;
        if n != WT_NONE {
            v = src[n * 512u + ts_local(low + vec3<i32>(r))];
        }
        fc_region[i] = v;
    }
    workgroupBarrier();
    // x: every row of the region.
    for (var i = lane; i < side * side; i = i + FC_THREADS) {
        fc_reduce_row(vec3<u32>(0u, i % side, i / side), vec3<u32>(1u, 0u, 0u));
    }
    workgroupBarrier();
    // y: the rows over the tile's own x, every z.
    for (var i = lane; i < 8u * side; i = i + FC_THREADS) {
        fc_reduce_row(vec3<u32>(s + i % 8u, 0u, i / 8u), vec3<u32>(0u, 1u, 0u));
    }
    workgroupBarrier();
    // z: the rows over the tile's own x and y.
    if lane < 64u {
        fc_reduce_row(vec3<u32>(s + (lane & 7u), s + (lane >> 3u), 0u), vec3<u32>(0u, 0u, 1u));
    }
    workgroupBarrier();
    for (var local = lane; local < 512u; local = local + FC_THREADS) {
        let r = vec3<u32>(local & 7u, (local >> 3u) & 7u, local >> 6u) + vec3<u32>(s);
        field[slot * 512u + local] = fc_region[fc_index(r)];
    }
}
