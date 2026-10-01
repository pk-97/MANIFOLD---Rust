// node.subtract_pressure — fusable BUFFER body, GATHER. One thread per padded
// cell of the face grid. A box wall face (index 0 or nodes along its axis)
// keeps its velocity and is valid: the solve took it as given, and
// node.face_gravity left only the part leaving the wall. A closed inner face
// (open fraction w = 0 in `solid_faces`, node.solid_faces' face grid) keeps
// its velocity and is valid, as FLIP Fluids'
// PressureSolver::_applyPressureToVelocityField leaves it, for
// node.constrain_solid_faces to give it the solid's. An open inner face
// between two water cells loses the pressure step (p_upper − p_lower) / h;
// with water on one side only, the air side's pressure is the ghost value
// clamp(φ_air / (φ_water + 1e-9), −25, 25) · p_water, φ_water taken at most
// −0.005h and φ_air at least 0: exactly the ratio node.pressure_smooth
// read, so the projection leaves only the solve's residual. Either way the
// face is valid (weight 1); between two air cells it keeps its velocity and
// is invalid (weight 0), for node.extend_faces to fill. Faces past the
// lattice give zeros. `pressure`, `water` and `phi` are gathered; a lattice
// longer than any of them gives zeros.
//
// Ported from FLIP Fluids pressuresolver.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md

fn body(idx: u32, count: u32, e_faces: Element, e_solid_faces: Element, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let lengths = min(min(arrayLength(&buf_pressure), arrayLength(&buf_water)), arrayLength(&buf_phi));
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) || cells > lengths {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let surface = -0.005 * cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) {
            continue;
        }
        if p[a] == 0 || p[a] == n[a] {
            out.face_velocity[a] = e_faces.face_velocity[a];
            out.face_weight[a] = 1.0;
            continue;
        }
        var below = p;
        below[a] = p[a] - 1;
        let upper = u32(p.x + n.x * (p.y + n.y * p.z));
        let lower = u32(below.x + n.x * (below.y + n.y * below.z));
        out.face_velocity[a] = e_faces.face_velocity[a];
        let wet_upper = buf_water[upper] > 0.5;
        let wet_lower = buf_water[lower] > 0.5;
        if !(e_solid_faces.face_weight[a] > 0.0) {
            out.face_weight[a] = 1.0;
        } else if wet_upper || wet_lower {
            var p_upper = buf_pressure[upper];
            var p_lower = buf_pressure[lower];
            if !wet_upper {
                p_upper = clamp(max(buf_phi[upper], 0.0) / (min(buf_phi[lower], surface) + 1e-9), -25.0, 25.0) * p_lower;
            } else if !wet_lower {
                p_lower = clamp(max(buf_phi[lower], 0.0) / (min(buf_phi[upper], surface) + 1e-9), -25.0, 25.0) * p_upper;
            }
            out.face_velocity[a] = e_faces.face_velocity[a] - (p_upper - p_lower) / cell_size;
            out.face_weight[a] = 1.0;
        }
    }
    return out;
}
