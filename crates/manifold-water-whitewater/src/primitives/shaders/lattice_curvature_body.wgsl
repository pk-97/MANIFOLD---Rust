var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.lattice_curvature — fusable BUFFER body, GATHER. One thread per
// pool element of the liquid step's world tiles (`buf_tiles`, `pool_tiles`
// tiles), FLIP's curvature grid (particlelevelset.cpp:692, :728): a cell
// whose distance and six face neighbours' distances all lie within 2 cells of
// 0 is known (1) and holds the mean curvature of the distance by central
// differences, in 1/m, clamped to ±1 cell⁻¹ (0 where the gradient vanishes);
// any other cell is unknown with 0. `distance` is gathered through the
// table: a neighbour in a tile it does not hold reads the canonical
// distance, which is no cell of the surface band. A free slot and the
// canonical element are unknown.
//
// Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn lk_phi(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn body(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    let unknown = Element(0.0, 0.0);
    if idx >= ts_canon() {
        return unknown;
    }
    if ts_slot_entry(idx >> 9u) == WT_NONE {
        return unknown;
    }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    let h = cell_size;
    let band = 2.0 * h;
    let p = lk_phi(c);
    if !(abs(p) < band) {
        return unknown;
    }
    for (var face = 0u; face < 6u; face = face + 1u) {
        if !(abs(lk_phi(c + ww_face_step(face))) < band) {
            return unknown;
        }
    }
    let ex = vec3<i32>(1, 0, 0);
    let ey = vec3<i32>(0, 1, 0);
    let ez = vec3<i32>(0, 0, 1);
    let x = 0.5 * (lk_phi(c + ex) - lk_phi(c - ex));
    let y = 0.5 * (lk_phi(c + ey) - lk_phi(c - ey));
    let z = 0.5 * (lk_phi(c + ez) - lk_phi(c - ez));
    let xx = lk_phi(c + ex) - 2.0 * p + lk_phi(c - ex);
    let yy = lk_phi(c + ey) - 2.0 * p + lk_phi(c - ey);
    let zz = lk_phi(c + ez) - 2.0 * p + lk_phi(c - ez);
    let xy = 0.25 * (lk_phi(c + ex + ey) - lk_phi(c - ex + ey) - lk_phi(c + ex - ey) + lk_phi(c - ex - ey));
    let xz = 0.25 * (lk_phi(c + ex + ez) - lk_phi(c - ex + ez) - lk_phi(c + ex - ez) + lk_phi(c - ex - ez));
    let yz = 0.25 * (lk_phi(c + ey + ez) - lk_phi(c - ey + ez) - lk_phi(c + ey - ez) + lk_phi(c - ey - ez));
    let g = x * x + y * y + z * z;
    let denominator = sqrt(g * g * g);
    if denominator < 1e-9 {
        return Element(0.0, 1.0);
    }
    let k = ((xx * (y * y + z * z) + yy * (x * x + z * z) + zz * (x * x + y * y)
        - 2.0 * xy * x * y - 2.0 * xz * x * z - 2.0 * yz * y * z) / denominator) / h;
    return Element(clamp(k, -1.0 / h, 1.0 / h), 1.0);
}
