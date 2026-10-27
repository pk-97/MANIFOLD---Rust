// FLIP's MAC trilinear (macvelocityfield.cpp:519, :551, :583) on the liquid
// step's pooled faces. Positions are in whitewater cells
// (`ww_cell_position`); face f of axis a is world cell f's low face along a,
// FaceSample record ts_face(f). Bodies read the records themselves: an
// include never names a buffer.
//
// Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// Where component `axis`'s stencil starts at q: the faces of `axis` sit on
// cell boundaries along it and at cell centres across it.
fn lf_stencil(q: vec3<f32>, axis: u32) -> vec3<f32> {
    var s = q - vec3<f32>(0.5);
    s[axis] = q[axis];
    return s;
}

// The `axis` velocity of a pooled FaceSample: 0 where the face's weight is
// not positive, as FLIP holds 0 at faces with no liquid velocity.
fn lf_face_velocity(face_velocity: vec4<f32>, face_weight: vec4<f32>, axis: u32) -> f32 {
    return select(0.0, face_velocity[axis], face_weight[axis] > 0.0);
}
