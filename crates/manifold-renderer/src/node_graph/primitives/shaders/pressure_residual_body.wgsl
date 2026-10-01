// node.pressure_residual — fusable BUFFER body, GATHER. One thread per cell:
// in a water cell, rhs − L value, where L value is
// (Σ w · water neighbours' value − Σ w · value) / h² over the cell's faces,
// each weighted by its open fraction w from `solid_faces` (node.solid_faces'
// face grid; box walls are 0): air neighbours hold zero pressure. 0 in air
// and in a water cell with no open face, which is out of the system.
// `water`, `value` and `solid_faces` are gathered; a lattice longer than
// any of them gives 0.

fn body(idx: u32, count: u32, e_rhs: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= cells || cells > min(arrayLength(&buf_water), arrayLength(&buf_value)) || faces > arrayLength(&buf_solid_faces)
        || !(buf_water[idx] > 0.5) {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    let own = buf_value[idx];
    var sum = 0.0;
    var diagonal = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            var face = p;
            face[a] = max(p[a], q[a]);
            let w = buf_solid_faces[u32(face.x + m.x * (face.y + m.y * face.z))].face_weight[a];
            if q[a] >= 0 && q[a] < n[a] && w > 0.0 {
                diagonal = diagonal + w;
                sum = sum - w * own;
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
    return e_rhs - sum / (cell_size * cell_size);
}
