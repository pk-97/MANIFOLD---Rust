// Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.constrain_solid_faces — fusable BUFFER body, pointwise. One thread
// per padded cell of the face grid, as FLIP Fluids'
// FluidSimulation::_constrainVelocityFieldThread: on an inner face, open
// fraction w from `solid_faces` and the solid's velocity v_s and friction f
// from `solid_velocity` (node.solid_face_velocity), a closed face (w = 0)
// takes v_s, a cut face (0 < w < 1) takes f·v_s + (1 − f)·u, an open face
// keeps u. Box wall faces and the weights (validity) pass through.

fn body(idx: u32, count: u32, e_faces: Element, e_solid_faces: Element, e_solid_velocity: Element, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    var out = e_faces;
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
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) || p[a] == 0 || p[a] == n[a] {
            continue;
        }
        let w = e_solid_faces.face_weight[a];
        let solid = e_solid_velocity.face_velocity[a];
        if !(w > 0.0) {
            out.face_velocity[a] = solid;
        } else if w < 1.0 {
            let f = e_solid_velocity.face_weight[a];
            out.face_velocity[a] = f * solid + (1.0 - f) * e_faces.face_velocity[a];
        }
    }
    return out;
}
