// node.liquid_cells — fusable BUFFER body, GATHER. One thread per whitewater
// cell, FLIP's emitter material grid (diffuseparticlesimulation.cpp:1655):
// solid (2) when the mean of its eight solid corners is below 0, else liquid
// (1) when its distance is below 0, else air (0). Then FLIP's shrink
// (:1667): a liquid cell with an air face neighbour becomes air. `distance`
// and `solid` are gathered; a grid past either array gives air.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn lc_kind(c: vec3<u32>, cells: vec3<u32>, nodes: vec3<u32>) -> u32 {
    var solid = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let n = c + ww_corner(corner);
        solid = solid + buf_solid[n.x + nodes.x * (n.y + nodes.y * n.z)];
    }
    if 0.125 * solid < 0.0 {
        return 2u;
    }
    return select(0u, 1u, buf_distance[ww_cell_index(c, cells)] < 0.0);
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> u32 {
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0u;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if idx >= total || total > arrayLength(&buf_distance) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) {
        return 0u;
    }
    let c = ww_cell(idx, cells);
    let own = lc_kind(c, cells, nodes);
    if own != 1u {
        return own;
    }
    for (var face = 0u; face < 6u; face = face + 1u) {
        let n = vec3<i32>(c) + ww_face_step(face);
        if any(n < vec3<i32>(0)) || any(n >= vec3<i32>(cells)) {
            continue;
        }
        if lc_kind(vec3<u32>(n), cells, nodes) == 0u {
            return 0u;
        }
    }
    return 1u;
}
