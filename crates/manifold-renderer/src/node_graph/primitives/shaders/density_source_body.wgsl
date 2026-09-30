// node.density_source — fusable BUFFER body; `cell_ranges` (CellRange)
// gathered. One thread per cell c holding particles, crowding
// e = count / rest − 1. A cell whose six neighbours all hold particles or lie
// past the lattice (the tank's walls) is inside the water, where every cell
// should hold `rest`: its source is rate · e, so the solve spreads a crowded
// cell and closes a sparse one. A cell beside an empty cell is at the
// surface, where fewer particles only mean a part-full cell: its source is
// rate · max(e, 0). out = −source; an empty cell, or a lattice larger than
// `cell_ranges`, gives 0.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, rest: f32, rate: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > arrayLength(&buf_cell_ranges) || buf_cell_ranges[idx].count == 0u {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    var inside = true;
    for (var a = 0; a < 3; a = a + 1) {
        for (var side = -1; side <= 1; side = side + 2) {
            var q = p;
            q[a] = p[a] + side;
            if q[a] >= 0 && q[a] < n[a] && buf_cell_ranges[u32(q.x + n.x * (q.y + n.y * q.z))].count == 0u {
                inside = false;
            }
        }
    }
    let crowding = f32(buf_cell_ranges[idx].count) / rest - 1.0;
    return -rate * select(max(crowding, 0.0), crowding, inside);
}
