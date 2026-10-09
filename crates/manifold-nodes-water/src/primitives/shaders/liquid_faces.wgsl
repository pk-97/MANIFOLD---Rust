// FLIP's MAC trilinear (macvelocityfield.cpp:519, :551, :583) on the seam's
// face arrays (LIQUID_SOLVER_SEAM_DESIGN.md section 3.2), placed `pad` grid
// cells into the whitewater grid as the lifecycle places them in FLIP's own
// arrays, which hold zeros around them. Positions are in whitewater cells.
// Bodies read the arrays themselves: an include never names a buffer.
//
// Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

const LF_NONE: u32 = 0xffffffffu;

// Where component `axis`'s stencil starts at q: the faces of `axis` sit on
// cell boundaries along it and at cell centres across it.
fn lf_stencil(q: vec3<f32>, axis: u32) -> vec3<f32> {
    var s = q - vec3<f32>(0.5);
    s[axis] = q[axis];
    return s;
}

// The seam array index of the `axis` face at whitewater face coordinates f,
// or LF_NONE where FLIP's padded array holds 0.
fn lf_face_index(f: vec3<i32>, axis: u32, pad: vec3<i32>, face_cells: vec3<u32>) -> u32 {
    var dims = face_cells;
    dims[axis] = dims[axis] + 1u;
    let g = f - pad;
    if any(g < vec3<i32>(0)) || any(g >= vec3<i32>(dims)) {
        return LF_NONE;
    }
    let u = vec3<u32>(g);
    return u.x + dims.x * (u.y + dims.y * u.z);
}

// Cells from the whitewater grid's first cell to the face grid's, per axis.
fn lf_pad(cells: vec3<u32>, face_cells: vec3<u32>) -> vec3<i32> {
    return (vec3<i32>(cells) - vec3<i32>(face_cells)) / 2;
}
