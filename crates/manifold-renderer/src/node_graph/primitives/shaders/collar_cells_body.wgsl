// node.collar_cells — fusable BUFFER body, GATHER. One thread per cell: 1
// when the cell is air and a face neighbour inside the lattice is water.
// Cells past the lattice, and a lattice longer than `water`, give 0.
// `water` is gathered through buf_water.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> u32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > arrayLength(&buf_water) || buf_water[idx] > 0.5 {
        return 0u;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            if q[a] >= 0 && q[a] < n[a] && buf_water[u32(q.x + n.x * (q.y + n.y * q.z))] > 0.5 {
                return 1u;
            }
        }
    }
    return 0u;
}
