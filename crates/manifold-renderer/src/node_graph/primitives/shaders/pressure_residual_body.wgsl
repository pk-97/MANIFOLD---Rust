// node.pressure_residual — fusable BUFFER body, GATHER. One thread per cell:
// in a water cell, rhs − L value, where L value is (Σ water neighbours'
// value − (neighbours inside the box) · value) / h²: air neighbours hold
// zero pressure, the box walls are closed. 0 in air. `water` and `value`
// are gathered through buf_water and buf_value; a lattice longer than either
// gives 0.

fn body(idx: u32, count: u32, e_rhs: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > min(arrayLength(&buf_water), arrayLength(&buf_value)) || !(buf_water[idx] > 0.5) {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    let own = buf_value[idx];
    var sum = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            if q[a] >= 0 && q[a] < n[a] {
                let at = u32(q.x + n.x * (q.y + n.y * q.z));
                sum = sum - own;
                if buf_water[at] > 0.5 {
                    sum = sum + buf_value[at];
                }
            }
        }
    }
    return e_rhs - sum / (cell_size * cell_size);
}
