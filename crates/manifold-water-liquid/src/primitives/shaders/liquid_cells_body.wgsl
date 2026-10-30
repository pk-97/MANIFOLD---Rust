var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.liquid_cells — fusable BUFFER body, GATHER. One thread per pool
// element of the liquid step's world tiles (`buf_tiles`, `pool_tiles` tiles),
// FLIP's emitter material grid (diffuseparticlesimulation.cpp:1655): solid
// (2) when the mean of its eight solid corners is below 0, else liquid (1)
// when its distance is below 0, else air (0). Cell c's corners are the solid
// nodes c..c+1, node n at the low corner of cell n. Then FLIP's shrink
// (:1667): a liquid cell with an air face neighbour becomes air. `distance`
// and `solid` are gathered through the table: a cell or node in a tile it
// does not hold reads the canonical element, air and not solid. A free slot
// and the canonical element are air.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn lc_kind(c: vec3<i32>) -> u32 {
    var solid = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        solid = solid + buf_solid[ts_cell_at(c + vec3<i32>(ww_corner(corner)))];
    }
    if 0.125 * solid < 0.0 {
        return 2u;
    }
    return select(0u, 1u, buf_distance[ts_cell_at(c)] < 0.0);
}

fn body(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> u32 {
    ww_pool_tiles = u32(pool_tiles);
    if idx >= ts_canon() {
        return 0u;
    }
    if ts_slot_entry(idx >> 9u) == WT_NONE {
        return 0u;
    }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    let own = lc_kind(c);
    if own != 1u {
        return own;
    }
    for (var face = 0u; face < 6u; face = face + 1u) {
        if lc_kind(c + ww_face_step(face)) == 0u {
            return 0u;
        }
    }
    return 1u;
}
