// Surface tiles: what happens around the display's world-tile build
// (`manifold_water_liquid::world_tiles`), composed after the read side
// (`manifold_node_engine::scene::world_tiles::READ_SIDE`) and the water's
// (the same read side renamed sim_ts_ / SIM_WT_).
//
// The build bins one buffer of records: this frame's particles, foam and
// spray (copied in, `interior_at` records), then GPU FLIP Narrow Band's
// interior records. A tile is held while one of them touches it or the ring
// round it, and let go the frame none does. An interior record holds the
// display tiles over grid water no particle marks.
//
// Passes, a barrier between each; `st_interior` runs only with an interior:
//   st_interior     one record an octant of each water slot
//   (the build)
//   st_clear_bounds the held tiles' bounds start empty
//   st_bounds       every held slot widens the bounds by its tile
//   st_extent       the extent in metres

struct Params {
    // The lattice origin (-h/2 on every axis) and 1 / h, as the build bins.
    origin: vec3<f32>,
    inv_cell: f32,
    // P, and h, the display cell size.
    pool_tiles: u32,
    cell: f32,
    // The water's pool and cell size.
    sim_pool_tiles: u32,
    sim_cell: f32,
    // The first interior record: the records before it are the frame's points.
    interior_at: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Point {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> tiles: array<u32>;
// The records the build bins: the frame's points, then the interior's.
@group(0) @binding(2) var<storage, read_write> points: array<Point>;
// The held tiles' lowest and highest tile coordinates.
@group(0) @binding(5) var<storage, read_write> bounds: array<atomic<i32>>;
// min xyz, 0, max xyz, 0, in metres.
@group(0) @binding(6) var<storage, read_write> extent: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// The step's grid interior (negative under water) on its own table.
@group(0) @binding(7) var<storage, read> interior: array<f32>;
@group(0) @binding(8) var<storage, read> interior_tiles: array<u32>;

fn sim_ts_pool_tiles() -> u32 { return params.sim_pool_tiles; }
fn sim_ts_table_word(i: u32) -> u32 { return interior_tiles[i]; }

// Past every tile coordinate.
const ST_FAR: i32 = 1073741823;

fn st_empty_record() -> Point {
    return Point(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
}

@compute @workgroup_size(1)
fn st_clear_bounds() {
    for (var k = 0u; k < 3u; k = k + 1u) {
        atomicStore(&bounds[k], ST_FAR);
        atomicStore(&bounds[3u + k], -ST_FAR);
    }
}

@compute @workgroup_size(256)
fn st_bounds(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s >= params.pool_tiles {
        return;
    }
    if ts_slot_entry(s) != WT_NONE {
        let tile = ts_slot_tile(s);
        atomicMin(&bounds[0], tile.x);
        atomicMin(&bounds[1], tile.y);
        atomicMin(&bounds[2], tile.z);
        atomicMax(&bounds[3], tile.x);
        atomicMax(&bounds[4], tile.y);
        atomicMax(&bounds[5], tile.z);
    }
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

// Record i is octant i % 8 of water slot i / 8: a point at the octant's middle
// when any of its 64 cells holds grid water, else empty. Water cell c is
// centred on c·h_water, so an octant's middle is 1.5 cells past its first.
// An octant is at most 16 display cells a side (the scale is at most 4), so
// its middle's tile and the ring round it cover every display tile it
// touches.
@compute @workgroup_size(256)
fn st_interior(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= 8u * params.sim_pool_tiles {
        return;
    }
    let slot = i >> 3u;
    var record = st_empty_record();
    if sim_ts_slot_entry(slot) != SIM_WT_NONE {
        let octant = 4u * vec3<u32>(i & 1u, (i >> 1u) & 1u, (i >> 2u) & 1u);
        var wet = false;
        for (var k = 0u; k < 64u; k = k + 1u) {
            let local = octant + vec3<u32>(k & 3u, (k >> 2u) & 3u, k >> 4u);
            if interior[slot * 512u + local.x + 8u * local.y + 64u * local.z] < 0.0 {
                wet = true;
                break;
            }
        }
        if wet {
            let first = sim_ts_slot_tile(slot) * 8 + vec3<i32>(octant);
            let middle = (vec3<f32>(first) + vec3<f32>(1.5)) * params.sim_cell;
            record.position_radius = vec4<f32>(middle, params.cell);
        }
    }
    points[params.interior_at + i] = record;
}
