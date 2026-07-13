// node.hypercube_points — fusable BUFFER body (freeze §12, buffer domain),
// SOURCE. Emit the 16 corner vertices of a 4D hypercube into an
// Array<Vec4Vertex>, with a continuous `dimension` control that collapses
// higher axes toward zero. Matches hypercube_vertices.wgsl bit-for-bit.
//
// ABI (buffer standalone codegen): no array inputs → body(idx, count,
// <params>) returns the Vec4Vertex written to buf_vertices[idx]. Vec4Vertex's
// Channels signature is 4 paired f32 scalars (x, y, z, w — see
// generators::mesh_common::VEC4_VERTEX_SPECS), so the codegen synthesizes
//   struct Element { x: f32, y: f32, z: f32, w: f32 }
// `dispatch_count` (= vert_capacity, fixed at 16 via array_output_capacity)
// is the wrapper guard; the idx >= 16u check below reproduces the hand
// kernel's explicit padding-slot zero-fill for any capacity beyond 16.
fn body(idx: u32, count: u32, dimension: f32) -> Element {
    if idx >= 16u {
        return Element(0.0, 0.0, 0.0, 0.0);
    }
    let k = 0.125;
    let sx = select(-1.0, 1.0, (idx & 1u) != 0u);
    let sy = select(-1.0, 1.0, (idx & 2u) != 0u);
    let sz = select(-1.0, 1.0, (idx & 4u) != 0u);
    let sw = select(-1.0, 1.0, (idx & 8u) != 0u);
    // present-fraction per axis: clamp(dimension - axisIndex, 0, 1).
    let px = clamp(dimension - 0.0, 0.0, 1.0);
    let py = clamp(dimension - 1.0, 0.0, 1.0);
    let pz = clamp(dimension - 2.0, 0.0, 1.0);
    let pw = clamp(dimension - 3.0, 0.0, 1.0);
    return Element(sx * k * px, sy * k * py, sz * k * pz, sw * k * pw);
}
