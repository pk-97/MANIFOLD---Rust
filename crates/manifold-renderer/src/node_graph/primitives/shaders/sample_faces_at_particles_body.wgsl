// node.sample_faces_at_particles — fusable BUFFER body, COINCIDENT
// particles, GATHER faces. A live particle's velocity becomes FLIP's MAC
// trilinear of the seam's face arrays at its position
// (macvelocityfield.cpp:635): each component from the eight faces of its
// staggered stencil, a face outside the face grid reading 0, and 0 on every
// axis outside the whitewater grid. Position, radius and id pass through; a
// slot with radius 0 passes whole.
//
// Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn sf_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn sf_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let still = Element(e_particles.position_radius, vec3<f32>(0.0), e_particles.id);
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) {
        return still;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return still;
    }
    let q = ww_grid_position(
        e_particles.position_radius.xyz,
        vec3<f32>(center_x, center_y, center_z),
        vec3<f32>(size_x, size_y, size_z),
        cells,
    );
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return still;
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = sf_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * sf_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return Element(e_particles.position_radius, v, e_particles.id);
}
