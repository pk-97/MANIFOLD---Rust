// node.face_divergence — fusable BUFFER body, GATHER. One thread per cell:
// in a water cell (water > 0.5), the net outflow through its six faces over
// cell_size, each inner face's velocity times its open fraction w from
// `solid_faces` (node.solid_faces' face grid), as FLIP Fluids'
// PressureSolver::_calculateNegativeDivergenceVector takes it:
// (w u(i+1) − w u(i) + w v(j+1) − w v(j) + w w(k+1) − w w(k)) / h; 0 in air.
// A box wall face counts whole: it holds only the part leaving the wall.
// `faces` and `solid_faces` are gathered; a face grid shorter than the
// lattice's gives 0.

fn face_divergence_open(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 1.0;
    }
    return buf_solid_faces[u32(f.x + m.x * (f.y + m.y * f.z))].face_weight[a];
}

fn body(idx: u32, count: u32, e_water: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= cells || !(e_water > 0.5) || faces > min(arrayLength(&buf_faces), arrayLength(&buf_solid_faces)) {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    let here = buf_faces[u32(p.x + m.x * (p.y + m.y * p.z))].face_velocity;
    var outflow = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        var q = p;
        q[a] = p[a] + 1;
        let upper = face_divergence_open(q, a, n, m) * buf_faces[u32(q.x + m.x * (q.y + m.y * q.z))].face_velocity[a];
        outflow = outflow + upper - face_divergence_open(p, a, n, m) * here[a];
    }
    return outflow / cell_size;
}
