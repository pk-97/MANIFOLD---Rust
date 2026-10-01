// Ported from FLIP Fluids rigidboundaryvelocity.cpp and pressuresolver.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.pressure_face_impulse — fusable BUFFER body, GATHER. One thread per
// padded cell of the face grid. On each inner face a body owns (velocity w of
// `solid_velocity`, solid_body_faces.wgsl), the impulse the pressure puts on
// the body through that face along its axis: ρh²·((c_lo − w)·p_lo −
// (c_hi − w)·p_hi), w the face's open fraction, c each side's open volume
// and p each side's pressure (0 outside the water). This is the engine's
// forcePerPressure (−h²·C·basis, C written as w − c) times the pressure,
// our p being dt·P/ρ. Every other face is zero; velocity w carries the
// owners on.
//
// ABI: `pressure`, `water` and `solid_faces` are gathered, `solid_velocity`
// is read coincident. Arrays shorter than the lattice give 0.

fn pressure_face_impulse_p(q: vec3<i32>, n: vec3<i32>) -> f32 {
    let cell = u32(q.x + n.x * (q.y + n.y * q.z));
    if !(buf_water[cell] > 0.5) {
        return 0.0;
    }
    return buf_pressure[cell];
}

fn body(
    idx: u32,
    count: u32,
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
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= faces || faces > arrayLength(&buf_solid_faces) || cells > min(arrayLength(&buf_pressure), arrayLength(&buf_water)) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let code = e_solid_velocity.face_velocity.w;
    let scale = density * cell_size * cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) || p[a] == 0 || p[a] == n[a] || solid_owner(code, a) < 0 {
            continue;
        }
        var lo = p;
        lo[a] = p[a] - 1;
        let w = buf_solid_faces[idx].face_weight[a];
        let c_hi = buf_solid_faces[idx].face_weight.w;
        let c_lo = buf_solid_faces[u32(lo.x + m.x * (lo.y + m.y * lo.z))].face_weight.w;
        out.face_velocity[a] = fma(scale * (c_lo - w), pressure_face_impulse_p(lo, n), -(scale * (c_hi - w) * pressure_face_impulse_p(p, n)));
    }
    out.face_velocity.w = code;
    return out;
}
