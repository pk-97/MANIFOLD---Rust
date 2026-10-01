// node.coarsen_water — fusable BUFFER body, GATHER. One thread per coarse
// cell: 1 when all eight fine cells it covers are water (> 0.5), else 0, so
// any air below makes the coarse cell air. `fine` is gathered through
// buf_fine; a fine lattice longer than it gives 0.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = 2 * n;
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || 8u * cells > arrayLength(&buf_fine) {
        return 0.0;
    }
    let p = 2 * vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    for (var child = 0; child < 8; child = child + 1) {
        let q = p + vec3<i32>(child & 1, (child >> 1u) & 1, (child >> 2u) & 1);
        if !(buf_fine[u32(q.x + m.x * (q.y + m.y * q.z))] > 0.5) {
            return 0.0;
        }
    }
    return 1.0;
}
