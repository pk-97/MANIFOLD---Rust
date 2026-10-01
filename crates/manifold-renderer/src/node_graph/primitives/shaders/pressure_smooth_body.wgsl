// node.pressure_smooth — fusable BUFFER body, GATHER. One thread per cell:
// one red-black Gauss-Seidel sweep of the weighted Poisson equation
// L p = rhs. Each face carries its open fraction w from `solid_faces`
// (node.solid_faces' face grid; box walls are 0). A water cell of the swept
// color ((i + j + k) mod 2 == color) becomes
// (Σ w · water neighbours' value − h² · rhs) / Σ w: air neighbours hold zero
// pressure. A water cell with no open face is out of the system and becomes
// 0. Every other cell keeps its value. `water`, `value` and `solid_faces`
// are gathered; a lattice longer than any of them gives 0.

fn body(idx: u32, count: u32, e_rhs: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32, color: i32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= cells || cells > min(arrayLength(&buf_water), arrayLength(&buf_value)) || faces > arrayLength(&buf_solid_faces) {
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
    var diagonal = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            // The face between p and q sits on the padded cell of the higher.
            var face = p;
            face[a] = max(p[a], q[a]);
            let w = buf_solid_faces[u32(face.x + m.x * (face.y + m.y * face.z))].face_weight[a];
            if q[a] >= 0 && q[a] < n[a] && w > 0.0 {
                diagonal = diagonal + w;
                let at = u32(q.x + n.x * (q.y + n.y * q.z));
                if buf_water[at] > 0.5 {
                    sum = sum + w * buf_value[at];
                }
            }
        }
    }
    if diagonal == 0.0 {
        return 0.0;
    }
    return (sum - cell_size * cell_size * e_rhs) / diagonal;
}
