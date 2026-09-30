// node.face_gravity — fusable BUFFER body. One thread per padded cell of the
// face grid: each face that exists gains gravity × step_dt along its normal,
// except the box walls (face index 0 or nodes along its axis), whose velocity
// is 0. Weights pass through; faces past the lattice give zeros.

fn body(
    idx: u32,
    count: u32,
    e_faces: Element,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    step_dt: f32,
) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let push = vec3<f32>(gravity_x, gravity_y, gravity_z) * step_dt;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) {
            continue;
        }
        out.face_weight[a] = e_faces.face_weight[a];
        if p[a] > 0 && p[a] < n[a] {
            out.face_velocity[a] = e_faces.face_velocity[a] + push[a];
        }
    }
    return out;
}
