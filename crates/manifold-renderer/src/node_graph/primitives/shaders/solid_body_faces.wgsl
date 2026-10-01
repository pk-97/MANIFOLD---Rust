// Shared by the atoms that couple bodies into the GPU FLIP pressure solve
// (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water)).
//
// A face record's three faces each name the body that owns them in the
// solid face velocity's w: Σ over the axes a of (b_a + 1) · 256^a, b_a the
// body (0 to body_count − 1) or −1 for none. Exact in f32 for 64 bodies.
// No node writes this owner code yet: BUG-6zj3 (step body owner code).
//
// A body's sums record (node.face_impulse_to_bodies), 16 floats per body:
// linear impulse (N·s), angular impulse about the centre of mass (N·m·s),
// velocity change (m/s), angular velocity change (rad/s), each a vec4 with
// w = 0.

// The body owning axis a's face, or −1.
fn solid_owner(code: f32, a: i32) -> i32 {
    return i32((u32(max(code, 0.0)) >> (8u * u32(a))) & 255u) - 1;
}

// Axis a's face of the record at p: the cell centre moved half a cell down
// along a.
fn solid_face_centre(lattice_min: vec3<f32>, p: vec3<i32>, a: i32, h: f32) -> vec3<f32> {
    var c = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(h), lattice_min);
    c[a] = fma(f32(p[a]), h, lattice_min[a]);
    return c;
}

// The face's velocity along a from a body's velocity change at r from its
// centre of mass: FLIP Fluids' rigid boundary basis, (dv + dω × r)[a].
fn solid_basis_dot(a: i32, r: vec3<f32>, dv: vec3<f32>, dw: vec3<f32>) -> f32 {
    let turn = vec3<f32>(fma(dw.y, r.z, -(dw.z * r.y)), fma(dw.z, r.x, -(dw.x * r.z)), fma(dw.x, r.y, -(dw.y * r.x)));
    return (dv + turn)[a];
}
