// node.subtract_pressure — fusable BUFFER body, GATHER. One thread per padded
// cell of the face grid. A box wall face (index 0 or nodes along its axis)
// is 0 and valid. An inner face with water on either side loses the pressure
// step (p_upper − p_lower) / h and is valid (weight 1); between two air
// cells it keeps its velocity and is invalid (weight 0), for
// node.extend_faces to fill. Faces past the lattice give zeros. `pressure`
// and `water` are gathered through buf_pressure and buf_water; a lattice
// longer than either gives zeros.

fn body(idx: u32, count: u32, e_faces: Element, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size_in: f32) -> Element {
    let separating = cell_size_in < 0.0;
    let all_walls = cell_size_in < -500.0;
    let cell_size = select(abs(cell_size_in), abs(cell_size_in) - 1000.0, all_walls);
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) || cells > min(arrayLength(&buf_pressure), arrayLength(&buf_water)) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) {
            continue;
        }
        if p[a] == 0 || p[a] == n[a] {
            out.face_weight[a] = 1.0;
            continue;
        }
        var below = p;
        below[a] = p[a] - 1;
        let upper = u32(p.x + n.x * (p.y + n.y * p.z));
        let lower = u32(below.x + n.x * (below.y + n.y * below.z));
        out.face_velocity[a] = e_faces.face_velocity[a];
        if buf_water[upper] > 0.5 || buf_water[lower] > 0.5 {
            var pu = buf_pressure[upper];
            var pl = buf_pressure[lower];
            let wall_u = any(p == vec3<i32>(0)) || any(p == n - vec3<i32>(1));
            let wall_l = any(below == vec3<i32>(0)) || any(below == n - vec3<i32>(1));
            if separating && (p.y == n.y - 1 || (all_walls && wall_u)) { pu = max(pu, 0.0); }
            if separating && (below.y == n.y - 1 || (all_walls && wall_l)) { pl = max(pl, 0.0); }
            out.face_velocity[a] = e_faces.face_velocity[a] - (pu - pl) / cell_size;
            out.face_weight[a] = 1.0;
        }
    }
    return out;
}
