// The lattice storage every water kernel indexes through. Cells are the
// solver's cells; records are one past the cells on every axis: a cell's
// three min-side faces, the far boundary faces, and the solid distance's
// corners. A pooled field is stored per 8^3 tile: tile slot s holds its 512
// cells at s * 512 + local(p), and the last 8 elements are the canonical
// block, the field's value wherever no tile is stored. A record lives with
// its cell; a record past the lattice on some axis belongs to no cell and
// always reads canonical. Arrays that stay dense boxes (the sort's ranges,
// the solid corners, coarse levels) use `ts_dense_cell`, `ts_corner` or
// `ts_grid_*`.
// Builds on `tile_grid.wgsl`, which the including shader composes first.
// The including shader defines `fn ts_cells() -> vec3<i32>` (the cell
// counts), `fn ts_pool_tiles() -> u32` (the pool's capacity P in tiles) and
// `fn ts_table_word(i: u32) -> u32`, the slot table read from whatever buffer
// it binds: words [0, T) are `slot_of` (a slot or TS_NONE per box tile),
// words [T, T + P) are `tile_of` (the box tile a slot holds or TS_NONE),
// T = `ts_tile_count()`. These helpers name no buffer.

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

// Elements of every pooled field.
fn ts_pool_len() -> u32 {
    return ts_pool_tiles() * 512u + 8u;
}

// The canonical record `mask` (see `ts_wall_mask`) of a pooled field.
fn ts_canon(mask: u32) -> u32 {
    return ts_pool_tiles() * 512u + mask;
}

fn ts_slot(tile: u32) -> u32 {
    return ts_table_word(tile);
}

// The cell's position inside its tile.
fn ts_local(p: vec3<i32>) -> u32 {
    return ts_grid_index(p & vec3<i32>(7), vec3<i32>(8));
}

// The box tile holding cell p (p inside the lattice).
fn ts_tile_of_cell(p: vec3<i32>) -> u32 {
    return ts_grid_index(p >> vec3<u32>(3u), ts_tile_dims());
}

// Bit a set when face a of record p exists (the other two coordinates inside
// the cells) and lies on a box wall (p[a] is 0 or n[a]).
fn ts_wall_mask(p: vec3<i32>) -> u32 {
    let n = ts_cells();
    var mask = 0u;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if all(other < n) && (p[a] == 0 || p[a] == n[a]) {
            mask = mask | (1u << u32(a));
        }
    }
    return mask;
}

// Cell p inside the lattice and its tile stored.
fn ts_cell_stored(p: vec3<i32>) -> bool {
    return all(p >= vec3<i32>(0)) && all(p < ts_cells()) && ts_slot(ts_tile_of_cell(p)) != TS_NONE;
}

// Record p stored: it is some cell's own record and that tile is stored.
fn ts_record_stored(p: vec3<i32>) -> bool {
    return ts_cell_stored(p);
}

// The pooled index of cell p (inside the lattice): its element, or the
// canonical cell when its tile is not stored.
fn ts_cell_at(p: vec3<i32>) -> u32 {
    let slot = ts_slot(ts_tile_of_cell(p));
    if slot == TS_NONE {
        return ts_canon(0u);
    }
    return slot * 512u + ts_local(p);
}

// The pooled index of record p (0..=n per axis): cell p's element when p is
// a stored cell, else the canonical record of its wall mask.
fn ts_face(p: vec3<i32>) -> u32 {
    if all(p < ts_cells()) {
        let slot = ts_slot(ts_tile_of_cell(p));
        if slot != TS_NONE {
            return slot * 512u + ts_local(p);
        }
    }
    return ts_canon(ts_wall_mask(p));
}

// The cell a pool index holds (idx is never canonical).
fn ts_cell_coord(idx: u32) -> vec3<i32> {
    return ts_origin(ts_table_word(ts_tile_count() + (idx >> 9u))) + ts_grid_coord(idx & 511u, vec3<i32>(i32(TS_TILE)));
}

// A stored record's index is its cell's.
fn ts_face_coord(idx: u32) -> vec3<i32> {
    return ts_cell_coord(idx);
}

// Thread `gid` of a list dispatched at 512 threads a tile: the pool index of
// the cell at local index gid & 511 of `tile`, or TS_NONE past the lattice
// (a partial edge tile).
fn ts_list_cell(tile: u32, gid: u32) -> u32 {
    let n = ts_cells();
    let cell = ts_origin(tile) + ts_grid_coord(gid & 511u, vec3<i32>(i32(TS_TILE)));
    if any(cell >= n) {
        return TS_NONE;
    }
    return ts_cell_at(cell);
}

// The cell of thread `gid` of a pass over the box's cells.
fn ts_box_cell(gid: u32) -> vec3<i32> {
    return ts_grid_coord(gid, ts_cells());
}

// The record of thread `gid` of a pass over the box's records.
fn ts_box_record(gid: u32) -> vec3<i32> {
    return ts_grid_coord(gid, ts_records());
}

// Cell p's place in an array that stays a dense box of cells.
fn ts_dense_cell(p: vec3<i32>) -> u32 {
    return ts_grid_index(p, ts_cells());
}

// Corner p's place in a dense corner lattice.
fn ts_corner(p: vec3<i32>) -> u32 {
    return ts_grid_index(p, ts_records());
}
