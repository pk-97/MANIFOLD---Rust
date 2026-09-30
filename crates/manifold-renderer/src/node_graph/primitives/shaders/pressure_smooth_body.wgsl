// node.pressure_smooth — fusable BUFFER body, GATHER. One thread per cell:
// one red-black Gauss-Seidel sweep of the masked Poisson equation L p = rhs.
// A water cell of the swept color ((i + j + k) mod 2 == color) becomes
// (Σ water neighbours' value − h² · rhs) / (its neighbours inside the box):
// air neighbours hold zero pressure, the box walls are closed. Every other
// cell keeps its value. `water` and `value` are gathered through buf_water
// and buf_value; a lattice longer than either gives 0.

fn body(idx: u32, count: u32, e_rhs: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32, color: i32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > min(arrayLength(&buf_water), arrayLength(&buf_value)) {
        return 0.0;
    }
    let own = buf_value[idx];
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    if !(buf_water[idx] > 0.5) || (p.x + p.y + p.z) % 2 != color {
        return own;
    }
    var sum = 0.0;
    var inside = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            if q[a] >= 0 && q[a] < n[a] {
                inside = inside + 1.0;
                let at = u32(q.x + n.x * (q.y + n.y * q.z));
                if buf_water[at] > 0.5 {
                    sum = sum + buf_value[at];
                }
            }
        }
    }
    if inside == 0.0 {
        return own;
    }
    return (sum - cell_size * cell_size * e_rhs) / inside;
}
