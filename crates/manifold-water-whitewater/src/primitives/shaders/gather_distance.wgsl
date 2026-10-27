// Whitewater step: the pooled phi into the stage's dense face-cells lattice.
// One thread per cell of the lattice, x fastest: cell c of the destination is
// the step's phi at world cell cell_origin + c read through the world-tile
// table, so a cell whose tile is not stored reads the pool's canonical value.
// Composed after the world-tile read side (`READ_SIDE`).

struct GatherParams {
    cells_x: u32,
    cells_y: u32,
    cells_z: u32,
    pool_tiles: u32,
    // The world cell of the lattice's first cell.
    cell_origin_x: i32,
    cell_origin_y: i32,
    cell_origin_z: i32,
    // H of the table: the power of two at or above 4 * pool_tiles.
    table_entries: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: GatherParams;
@group(0) @binding(1) var<storage, read> ts_table: array<u32>;
@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

fn ts_pool_tiles() -> u32 {
    return params.pool_tiles;
}

fn ts_table_entries() -> u32 {
    return params.table_entries;
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
    let cells = vec3<u32>(params.cells_x, params.cells_y, params.cells_z);
    let c = vec3<i32>(
        i32(i % cells.x),
        i32((i / cells.x) % cells.y),
        i32(i / (cells.x * cells.y)),
    );
    let origin = vec3<i32>(params.cell_origin_x, params.cell_origin_y, params.cell_origin_z);
    dst[i] = src[ts_cell_at(origin + c)];
}
