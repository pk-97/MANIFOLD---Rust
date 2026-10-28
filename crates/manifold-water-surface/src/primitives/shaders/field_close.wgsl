// node.field_close — one half of a morphological closing of the liquid's
// level set over the nodes of a world-tile pool (`buf_tiles`, `pool_tiles`
// tiles), one workgroup per slot and one more for the canonical element.
// Element p is node p.
//
// With `widest` 0 each node takes the least value of the 3³ nodes round it
// (the liquid grown by a node), with `widest` 1 the greatest (shrunk by a
// node); the node runs the grow, then the shrink on what it grew. Holes and
// gaps through the liquid up to two nodes across close, and every surface
// the grow leaves open comes back where it was. Each pass is separable: the
// extreme along x, then y, then z.
//
// The workgroup stages its tile and one node round it (10³ nodes) through its
// 27 neighbour tiles. A node in a tile the table does not hold takes no part:
// it stages as the far value, FIELD_FAR_CELLS (3) cells, while growing and
// its negation while shrinking, so it is never the extreme. A free slot and
// the canonical element hold the far value.

struct Params {
    cell_size: f32,
    pool_tiles: u32,
    entries: u32,
    // 0 takes the least value round each node, 1 the greatest.
    widest: u32,
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
// The staged region: the tile and one node round it, 10 a side.
const FC_SIDE: u32 = 10u;
const FC_REGION: u32 = 1000u;

var<workgroup> fc_held: u32;
// The slots of the 27 tiles round this one, by neighbour index.
var<workgroup> fc_near: array<u32, 27>;
// `src` over the region, then the x pass over x 1..8, then the y pass over x
// and y 1..8; the z pass runs on the tile's own nodes.
var<workgroup> fc_src: array<f32, 1000>;
var<workgroup> fc_x: array<f32, 1000>;
var<workgroup> fc_y: array<f32, 1000>;

fn fc_coord(i: u32) -> vec3<u32> {
    return vec3<u32>(i % FC_SIDE, (i / FC_SIDE) % FC_SIDE, i / (FC_SIDE * FC_SIDE));
}

// The slot of the tile region node r lies in, WT_NONE when it is not held.
fn fc_slot(r: vec3<u32>) -> u32 {
    let d = select(vec3<i32>(0), vec3<i32>(-1), r == vec3<u32>(0u))
        + select(vec3<i32>(0), vec3<i32>(1), r == vec3<u32>(FC_SIDE - 1u));
    return fc_near[ts_neighbour_index(d)];
}

// The extreme of three neighbouring values: the least while growing, the
// greatest while shrinking.
fn fc_pick(a: f32, b: f32, c: f32) -> f32 {
    if params.widest == 1u {
        return max(max(a, b), c);
    }
    return min(min(a, b), c);
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
    let low = ts_slot_tile(slot) * 8 - vec3<i32>(1);
    for (var i = lane; i < FC_REGION; i = i + FC_THREADS) {
        let r = fc_coord(i);
        let s = fc_slot(r);
        var v = absent;
        if s != WT_NONE {
            v = src[s * 512u + ts_local(low + vec3<i32>(r))];
        }
        fc_src[i] = v;
    }
    workgroupBarrier();
    for (var i = lane; i < FC_REGION; i = i + FC_THREADS) {
        let r = fc_coord(i);
        if r.x == 0u || r.x == FC_SIDE - 1u {
            continue;
        }
        fc_x[i] = fc_pick(fc_src[i - 1u], fc_src[i], fc_src[i + 1u]);
    }
    workgroupBarrier();
    for (var i = lane; i < FC_REGION; i = i + FC_THREADS) {
        let r = fc_coord(i);
        if r.x == 0u || r.x == FC_SIDE - 1u || r.y == 0u || r.y == FC_SIDE - 1u {
            continue;
        }
        fc_y[i] = fc_pick(fc_x[i - FC_SIDE], fc_x[i], fc_x[i + FC_SIDE]);
    }
    workgroupBarrier();
    for (var local = lane; local < 512u; local = local + FC_THREADS) {
        let r = vec3<u32>(local & 7u, (local >> 3u) & 7u, local >> 6u) + vec3<u32>(1u);
        let i = r.x + FC_SIDE * (r.y + FC_SIDE * r.z);
        field[slot * 512u + local] = fc_pick(
            fc_y[i - FC_SIDE * FC_SIDE], fc_y[i], fc_y[i + FC_SIDE * FC_SIDE],
        );
    }
}
