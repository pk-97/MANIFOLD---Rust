// node.extend_faces — fusable BUFFER body, GATHER. One thread per padded
// cell of the face grid. A valid face (weight > 0) is copied unchanged. An
// invalid one takes the mean velocity of the valid faces of the same
// component among its six grid neighbours and becomes valid (weight 1); with
// none it stays as it was. Reads only the input layer, so the result does not
// depend on thread order. Faces past the lattice give zeros. `faces` is
// gathered through buf_faces; a grid shorter than the lattice's gives zeros.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let padded = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= padded || padded > arrayLength(&buf_faces) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let here = buf_faces[idx];
    for (var a = 0; a < 3; a = a + 1) {
        // Faces of component a span 0..=n on axis a and 0..n on the others.
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        if any(p > top) {
            continue;
        }
        out.face_velocity[a] = here.face_velocity[a];
        out.face_weight[a] = here.face_weight[a];
        if here.face_weight[a] > 0.0 {
            continue;
        }
        var sum = 0.0;
        var hits = 0.0;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                if q[b] < 0 || q[b] > top[b] {
                    continue;
                }
                let neighbour = buf_faces[u32(q.x + m.x * (q.y + m.y * q.z))];
                if neighbour.face_weight[a] > 0.0 {
                    sum = sum + neighbour.face_velocity[a];
                    hits = hits + 1.0;
                }
            }
        }
        if hits > 0.0 {
            out.face_velocity[a] = sum / hits;
            out.face_weight[a] = 1.0;
        }
    }
    return out;
}
