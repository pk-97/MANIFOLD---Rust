var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.smooth_lattice — fusable BUFFER body, GATHER. One thread per pool
// element of the display's world tiles (`buf_tiles`, `pool_tiles` tiles):
// `passes` rounds of the [1, 2, 1] / 4 filter along one axis, as one
// (2·passes + 1)-tap binomial gather. Chained over x, y and z it equals the
// full 3D binomial blur, because the weights are a product of per-axis rows.
// `levelset` (f32) is a node array gathered through the table: element
// ts_cell_at(p) holds node p, and a tap in a tile the table does not hold
// reads the canonical element. The canonical element and a free slot hold the
// canonical level set; with no passes a held node copies its input.

// Binomial rows C(2p, k) / 4^p for p = 1, 2, 3, packed at offsets 0, 3, 8.
fn smooth_weight(passes: i32, d: i32) -> f32 {
    var rows = array<f32, 15>(
        0.25, 0.5, 0.25,
        0.0625, 0.25, 0.375, 0.25, 0.0625,
        0.015625, 0.09375, 0.234375, 0.3125, 0.234375, 0.09375, 0.015625,
    );
    let offset = select(select(8, 3, passes == 2), 0, passes == 1);
    return rows[offset + passes + d];
}

fn body(idx: u32, count: u32, cell_size: f32, passes: f32, axis: i32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    // FIELD_FAR_CELLS (scene_object) cells: the level set where no tile is stored.
    if idx >= ts_canon() || ts_slot_entry(idx >> 9u) == WT_NONE { return 3.0 * cell_size; }
    let value = buf_levelset[idx];
    let p = i32(clamp(round(passes), 0.0, 3.0));
    if p == 0 {
        return value;
    }
    ts_home(idx);
    let node = ts_cell_coord(idx);
    let a = clamp(axis, 0, 2);
    var sum = 0.0;
    for (var d = -p; d <= p; d = d + 1) {
        var q = node;
        q[a] = node[a] + d;
        sum = sum + smooth_weight(p, d) * buf_levelset[ts_cell_at(q)];
    }
    return sum;
}
