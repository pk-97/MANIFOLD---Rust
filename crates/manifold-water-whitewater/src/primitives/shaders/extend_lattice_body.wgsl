var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.extend_lattice — fusable BUFFER body, GATHER. One thread per pool
// element of the liquid step's world tiles (`buf_tiles`, `pool_tiles` tiles),
// one layer of FLIP's extrapolation (gridutils.h,
// extrapolateGridWithObserver): a known cell keeps its value; an unknown cell
// with a known face neighbour becomes known, holding the mean over its known
// face neighbours. A neighbour in a tile the table does not hold reads the
// canonical element, unknown. Reads only the input layer, so passes chain.
// A free slot and the canonical element are unknown.
//
// Ported from FLIP Fluids gridutils.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    let unknown = Element(0.0, 0.0);
    if idx >= ts_canon() {
        return unknown;
    }
    if ts_slot_entry(idx >> 9u) == WT_NONE {
        return unknown;
    }
    let own = buf_values[idx];
    if own.known > 0.0 {
        return own;
    }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    var sum = 0.0;
    var hits = 0.0;
    for (var face = 0u; face < 6u; face = face + 1u) {
        let other = buf_values[ts_cell_at(c + ww_face_step(face))];
        if other.known > 0.0 {
            sum = sum + other.value;
            hits = hits + 1.0;
        }
    }
    if hits == 0.0 {
        return own;
    }
    return Element(sum / hits, 1.0);
}
