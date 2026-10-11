// node.crossing_distance — fusable BUFFER body, COINCIDENT crossings, GATHER
// solid. One thread per whitewater cell: the distance from the cell centre
// to the tangent plane at its nearest crossing (to the crossing itself when
// it has no normal), in metres, signed by the level at the centre and held
// to 4 cells. The plane leaves an error second-order in how far the crossing
// sits to the side of the true nearest point. Then FLIP's post-process (particlelevelset.cpp:170): a
// cell whose centre is in a solid (the mean of its eight solid corners
// below 0) and whose distance is under half a cell reads −½ cell, so liquid
// runs into walls; a distance within 0.005 cell of 0 moves out to it, keeping
// its side. A grid past the solid array gives 4 cells.
//
// Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(
    idx: u32,
    count: u32,
    e_crossings: Element,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
) -> f32 {
    let h = cell_size;
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 4.0 * h;
    }
    let cells = nodes - vec3<u32>(1u);
    if idx >= cells.x * cells.y * cells.z || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) {
        return 4.0 * h;
    }
    let c = ww_cell(idx, cells);
    let centre = vec3<f32>(c) + vec3<f32>(0.5);
    let offset = centre - e_crossings.crossing;
    let n = e_crossings.normal;
    let reach = select(length(offset), abs(dot(offset, n)), dot(n, n) > 0.5) * h;
    var d = select(1.0, -1.0, e_crossings.level < 0.0) * min(reach, 4.0 * h);
    var solid = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let n = c + ww_corner(corner);
        solid = solid + buf_solid[n.x + nodes.x * (n.y + nodes.y * n.z)];
    }
    if d < 0.5 * h && 0.125 * solid < 0.0 {
        d = -0.5 * h;
    }
    let eps = 0.005 * h;
    if abs(d) < eps {
        d = select(-eps, eps, d > 0.0);
    }
    return d;
}
