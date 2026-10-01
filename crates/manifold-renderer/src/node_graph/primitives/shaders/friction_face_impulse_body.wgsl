// node.friction_face_impulse — fusable BUFFER body, GATHER. One thread per
// padded cell of the face grid. node.constrain_solid_faces drags a cut
// face's velocity u toward its body's v_s by the friction f; the liquid on
// the face (density·cell_size³ times its open fraction w) loses
// f·(v_s − u) of velocity, so the body gains density·cell_size³·w·f·(u − v_s)
// of impulse along the face's axis. Only on inner cut faces (0 < w < 1) a
// body owns that touch the water (either side cell water > 0.5); every
// other face is zero. FLIP Fluids keeps no such reaction: it is ours
// (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water)).
// Velocity w carries the owner code on.
//
// ABI: `faces`, `solid_faces` and `solid_velocity` are read coincident,
// `water` gathered; a water lattice shorter than the lattice gives 0.

fn friction_face_impulse_wet(q: vec3<i32>, n: vec3<i32>) -> bool {
    return buf_water[u32(q.x + n.x * (q.y + n.y * q.z))] > 0.5;
}

fn body(
    idx: u32,
    count: u32,
    e_faces: Element,
    e_solid_faces: Element,
    e_solid_velocity: Element,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    density: f32,
) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) || u32(n.x) * u32(n.y) * u32(n.z) > arrayLength(&buf_water) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let code = e_solid_velocity.face_velocity.w;
    let mass = density * cell_size * cell_size * cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        let w = e_solid_faces.face_weight[a];
        if !all(other < n) || p[a] == 0 || p[a] == n[a] || solid_owner(code, a) < 0 || !(w > 0.0 && w < 1.0) {
            continue;
        }
        var lo = p;
        lo[a] = p[a] - 1;
        if !(friction_face_impulse_wet(lo, n) || friction_face_impulse_wet(p, n)) {
            continue;
        }
        let f = e_solid_velocity.face_weight[a];
        out.face_velocity[a] = mass * w * f * (e_faces.face_velocity[a] - e_solid_velocity.face_velocity[a]);
    }
    out.face_velocity.w = code;
    return out;
}
