// The lattice storage every water kernel indexes through. Cells are the
// solver's cells; records are one past the cells on every axis: a cell's
// three min-side faces, the far boundary faces, and the solid distance's
// corners. Today both are dense boxes, x fastest, then y, then z; the tile
// pool later replaces these bodies, not their callers. Small dense grids of
// their own (the tile grid, coarse levels, a local loop) use `ts_grid_*`.
// Builds on `tile_grid.wgsl`, which the including shader composes first.
// The including shader defines `fn ts_cells() -> vec3<i32>`, the cell counts.

fn ts_records() -> vec3<i32> {
    return ts_cells() + vec3<i32>(1);
}

fn ts_cell_count() -> u32 {
    let n = ts_cells();
    return u32(n.x) * u32(n.y) * u32(n.z);
}

fn ts_record_count() -> u32 {
    let m = ts_records();
    return u32(m.x) * u32(m.y) * u32(m.z);
}

fn ts_cell_at(p: vec3<i32>) -> u32 {
    return ts_grid_index(p, ts_cells());
}

fn ts_cell_coord(idx: u32) -> vec3<i32> {
    return ts_grid_coord(idx, ts_cells());
}

fn ts_face(p: vec3<i32>) -> u32 {
    return ts_grid_index(p, ts_records());
}

fn ts_face_coord(idx: u32) -> vec3<i32> {
    return ts_grid_coord(idx, ts_records());
}

fn ts_tile_dims() -> vec3<i32> {
    return (ts_cells() + vec3<i32>(i32(TS_TILE) - 1)) / i32(TS_TILE);
}

fn ts_tile_count() -> u32 {
    let t = ts_tile_dims();
    return u32(t.x) * u32(t.y) * u32(t.z);
}

fn ts_origin(tile: u32) -> vec3<i32> {
    return ts_grid_coord(tile, ts_tile_dims()) * i32(TS_TILE);
}

// Thread `gid` of a list dispatched at 512 threads a tile: the cell at local
// index gid & 511 of `tile`, or TS_NONE past the lattice (a partial edge tile).
fn ts_list_cell(tile: u32, gid: u32) -> u32 {
    let n = ts_cells();
    let cell = ts_origin(tile) + ts_grid_coord(gid & 511u, vec3<i32>(i32(TS_TILE)));
    if any(cell >= n) {
        return TS_NONE;
    }
    return ts_cell_at(cell);
}
