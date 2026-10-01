// node.face_divergence — fusable BUFFER body, GATHER. One thread per cell:
// in a water cell (water > 0.5), the net outflow through its six faces over
// cell_size, (u(i+1) − u(i) + v(j+1) − v(j) + w(k+1) − w(k)) / h; 0 in air.
// `faces` is gathered through buf_faces; a face grid shorter than the
// lattice's gives 0.

fn body(idx: u32, count: u32, e_water: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || !(e_water > 0.5) || u32(m.x) * u32(m.y) * u32(m.z) > arrayLength(&buf_faces) {
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
        outflow = outflow + buf_faces[u32(q.x + m.x * (q.y + m.y * q.z))].face_velocity[a] - here[a];
    }
    return outflow / cell_size;
}
