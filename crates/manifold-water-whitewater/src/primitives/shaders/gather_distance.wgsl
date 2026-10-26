// Whitewater step: the pooled phi into the stage's dense face-cells lattice.
// One thread per cell of the lattice, x fastest: cell p of the destination is
// the step's phi at p read through the slot table, so a cell whose tile is
// not stored reads the pool's canonical value. Composed after `TILE_STORAGE`.

struct GatherParams {
    cells_x: u32,
    cells_y: u32,
    cells_z: u32,
    pool_tiles: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: GatherParams;
@group(0) @binding(1) var<storage, read> ts_table: array<u32>;
@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

fn ts_cells() -> vec3<i32> {
    return vec3<i32>(i32(params.cells_x), i32(params.cells_y), i32(params.cells_z));
}

fn ts_pool_tiles() -> u32 {
    return params.pool_tiles;
}

fn ts_table_word(i: u32) -> u32 {
    return ts_table[i];
}

@compute @workgroup_size(256)
fn gather_distance(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    dst[i] = src[ts_cell_at(ts_box_cell(i))];
}
