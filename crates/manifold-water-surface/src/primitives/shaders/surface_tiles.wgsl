// Surface tiles: what happens around the display's world-tile build
// (`manifold_water_liquid::world_tiles`), composed after the read side
// (`manifold_node_engine::scene::world_tiles::READ_SIDE`).
//
// The build bins one buffer of liquid particle records: this frame's
// particles, foam and spray (copied in, `head` records), then one keep record
// per slot of the pool. A keep record is a point at the middle of a tile that
// water touched within `keep_seconds`, so the next build still holds that tile
// and its slot (wetness lives in slots, and a wet tile must keep its own).
//
// Passes, a barrier between each; `st_reset` runs only on a fresh start:
//   st_reset   every slot untouched, every keep record empty
//   (the build)
//   st_carry   a slot that holds another tile than last frame, or was free,
//              forgets when water last touched it; the extent starts empty
//   st_touch   every live point of this frame stamps the display time on the
//              slot of its tile (all stamps equal, so order never matters)
//   st_keep    every slot writes its keep record for the next build, and the
//              held tiles widen the extent
//   st_extent  the extent in metres

struct Params {
    // The lattice origin (−h/2 on every axis) and 1 / h, as the build bins.
    origin: vec3<f32>,
    inv_cell: f32,
    // P, and the records before the keep records.
    pool_tiles: u32,
    head: u32,
    // The display time, and how long a touched tile stays held.
    time: f32,
    keep_seconds: f32,
    // h, the display cell size.
    cell: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

struct Point {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> tiles: array<u32>;
// The records the build bins: the frame's points, then P keep records.
@group(0) @binding(2) var<storage, read_write> points: array<Point>;
// Per slot, the display time (f32 bits) water last touched its tile.
@group(0) @binding(3) var<storage, read_write> touched: array<atomic<u32>>;
// Last frame's slot rows of the table: a tile (x, y, z) and an entry per slot.
@group(0) @binding(4) var<storage, read> last_rows: array<u32>;
// The held tiles' lowest and highest tile coordinates.
@group(0) @binding(5) var<storage, read_write> bounds: array<atomic<i32>>;
// min xyz, 0, max xyz, 0, in metres.
@group(0) @binding(6) var<storage, read_write> extent: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// No stamp: a slot water has not touched since it took its tile.
const ST_NEVER: u32 = 0xffffffffu;
// Past every tile coordinate.
const ST_FAR: i32 = 1073741823;

fn st_empty_record() -> Point {
    return Point(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
}

@compute @workgroup_size(256)
fn st_reset(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s >= params.pool_tiles {
        return;
    }
    atomicStore(&touched[s], ST_NEVER);
    points[params.head + s] = st_empty_record();
}

@compute @workgroup_size(256)
fn st_carry(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s == 0u {
        for (var k = 0u; k < 3u; k = k + 1u) {
            atomicStore(&bounds[k], ST_FAR);
            atomicStore(&bounds[3u + k], -ST_FAR);
        }
    }
    if s >= params.pool_tiles {
        return;
    }
    let now = 2u * ts_table_entries() + WT_SLOT_WORDS * s;
    if tiles[now + 3u] == WT_NONE {
        atomicStore(&touched[s], ST_NEVER);
        return;
    }
    let before = WT_SLOT_WORDS * s;
    let same = last_rows[before + 3u] != WT_NONE
        && last_rows[before] == tiles[now]
        && last_rows[before + 1u] == tiles[now + 1u]
        && last_rows[before + 2u] == tiles[now + 2u];
    if !same {
        atomicStore(&touched[s], ST_NEVER);
    }
}

@compute @workgroup_size(256)
fn st_touch(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.head {
        return;
    }
    let p = points[i].position_radius;
    if !(p.w > 0.0) {
        return;
    }
    let cell = ts_world_cell(p.xyz, params.origin, params.inv_cell);
    let slot = ts_hash_slot(cell >> vec3<u32>(3u));
    if slot != WT_NONE {
        atomicStore(&touched[slot], bitcast<u32>(params.time));
    }
}

@compute @workgroup_size(256)
fn st_keep(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s >= params.pool_tiles {
        return;
    }
    var record = st_empty_record();
    if ts_slot_entry(s) != WT_NONE {
        let tile = ts_slot_tile(s);
        atomicMin(&bounds[0], tile.x);
        atomicMin(&bounds[1], tile.y);
        atomicMin(&bounds[2], tile.z);
        atomicMax(&bounds[3], tile.x);
        atomicMax(&bounds[4], tile.y);
        atomicMax(&bounds[5], tile.z);
        let at = atomicLoad(&touched[s]);
        if at != ST_NEVER && params.time - bitcast<f32>(at) <= params.keep_seconds {
            // The middle of tile t is cell 8t + 3.5, centred on (8t + 3.5)·h.
            let middle = (vec3<f32>(tile) * 8.0 + vec3<f32>(3.5)) * params.cell;
            record.position_radius = vec4<f32>(middle, params.cell);
        }
    }
    points[params.head + s] = record;
}

// Node p sits at (p − ½)·h, so the held tiles' nodes run from the low corner
// of the lowest tile to the high face of the highest. No tile held: min above
// max.
@compute @workgroup_size(1)
fn st_extent() {
    let lo = vec3<i32>(atomicLoad(&bounds[0]), atomicLoad(&bounds[1]), atomicLoad(&bounds[2]));
    let hi = vec3<i32>(atomicLoad(&bounds[3]), atomicLoad(&bounds[4]), atomicLoad(&bounds[5]));
    var low = vec3<f32>(1.0e30);
    var high = vec3<f32>(-1.0e30);
    if all(lo <= hi) {
        low = (vec3<f32>(lo) * 8.0 - vec3<f32>(0.5)) * params.cell;
        high = (vec3<f32>(hi + vec3<i32>(1)) * 8.0 - vec3<f32>(0.5)) * params.cell;
    }
    extent[0] = low.x;
    extent[1] = low.y;
    extent[2] = low.z;
    extent[3] = 0.0;
    extent[4] = high.x;
    extent[5] = high.y;
    extent[6] = high.z;
    extent[7] = 0.0;
}
