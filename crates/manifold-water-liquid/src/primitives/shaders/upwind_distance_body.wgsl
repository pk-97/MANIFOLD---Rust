var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// FLIP Fluids LevelSetSolver::_stepSolverThreadUpwind, MIT; see notices.
// One thread per element of the pooled cell arrays `levelset` and `valid`
// (the step's world tiles, `buf_tiles`). A cell in a tile the table does not
// hold reads the canonical element as its neighbour. The canonical element,
// a free slot and an invalid cell return the canonical or their own input.
fn body(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if idx >= ts_canon() { return buf_levelset[ts_canon()]; }
    if ts_slot_entry(idx >> 9u) == WT_NONE { return buf_levelset[ts_canon()]; }
    let d = buf_levelset[idx];
    if buf_valid[idx] == 0u { return d; }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    var positive = 0.0;
    var negative = 0.0;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        var e = vec3<i32>(0);
        e[axis] = 1;
        let a = (d-buf_levelset[ts_cell_at(c - e)]) / cell_size;
        let b = (buf_levelset[ts_cell_at(c + e)]-d) / cell_size;
        positive = positive + max(a,0.0)*max(a,0.0) + min(b,0.0)*min(b,0.0);
        negative = negative + min(a,0.0)*min(a,0.0) + max(b,0.0)*max(b,0.0);
    }
    let sign = d / sqrt(d*d + cell_size*cell_size);
    return d - 0.5*cell_size*max(sign,0.0)*(sqrt(positive)-1.0)
        - 0.5*cell_size*min(sign,0.0)*(sqrt(negative)-1.0);
}
