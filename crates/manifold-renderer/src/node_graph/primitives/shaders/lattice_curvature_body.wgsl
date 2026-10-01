// node.lattice_curvature — fusable BUFFER body, GATHER. One thread per
// whitewater cell, FLIP's curvature grid (particlelevelset.cpp:692, :728):
// a cell off the grid border whose distance and six face neighbours'
// distances all lie within 2 cells of 0 is known (1) and holds the mean
// curvature of the distance by central differences, in 1/m, clamped to ±1
// cell⁻¹ (0 where the gradient vanishes); any other cell is unknown with 0.
// `distance` is gathered; a grid past its array gives unknown.
//
// Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn lk_phi(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> Element {
    let unknown = Element(0.0, 0.0);
    let h = cell_size;
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return unknown;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if idx >= total || total > arrayLength(&buf_distance) {
        return unknown;
    }
    let u = ww_cell(idx, cells);
    if ww_on_border(u, cells) {
        return unknown;
    }
    let band = 2.0 * h;
    let c = vec3<i32>(u);
    let p = lk_phi(c, cells);
    if !(abs(p) < band) {
        return unknown;
    }
    for (var face = 0u; face < 6u; face = face + 1u) {
        if !(abs(lk_phi(c + ww_face_step(face), cells)) < band) {
            return unknown;
        }
    }
    let ex = vec3<i32>(1, 0, 0);
    let ey = vec3<i32>(0, 1, 0);
    let ez = vec3<i32>(0, 0, 1);
    let x = 0.5 * (lk_phi(c + ex, cells) - lk_phi(c - ex, cells));
    let y = 0.5 * (lk_phi(c + ey, cells) - lk_phi(c - ey, cells));
    let z = 0.5 * (lk_phi(c + ez, cells) - lk_phi(c - ez, cells));
    let xx = lk_phi(c + ex, cells) - 2.0 * p + lk_phi(c - ex, cells);
    let yy = lk_phi(c + ey, cells) - 2.0 * p + lk_phi(c - ey, cells);
    let zz = lk_phi(c + ez, cells) - 2.0 * p + lk_phi(c - ez, cells);
    let xy = 0.25 * (lk_phi(c + ex + ey, cells) - lk_phi(c - ex + ey, cells) - lk_phi(c + ex - ey, cells) + lk_phi(c - ex - ey, cells));
    let xz = 0.25 * (lk_phi(c + ex + ez, cells) - lk_phi(c - ex + ez, cells) - lk_phi(c + ex - ez, cells) + lk_phi(c - ex - ez, cells));
    let yz = 0.25 * (lk_phi(c + ey + ez, cells) - lk_phi(c - ey + ez, cells) - lk_phi(c + ey - ez, cells) + lk_phi(c - ey - ez, cells));
    let g = x * x + y * y + z * z;
    let denominator = sqrt(g * g * g);
    if denominator < 1e-9 {
        return Element(0.0, 1.0);
    }
    let k = ((xx * (y * y + z * z) + yy * (x * x + z * z) + zz * (x * x + y * y)
        - 2.0 * xy * x * y - 2.0 * xz * x * z - 2.0 * yz * y * z) / denominator) / h;
    return Element(clamp(k, -1.0 / h, 1.0 / h), 1.0);
}
