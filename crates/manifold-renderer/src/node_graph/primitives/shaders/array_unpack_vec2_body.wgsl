// node.array_unpack_vec2 — BUFFER body (freeze §12, buffer domain), MULTI-OUTPUT.
// Splits a coincident [f32;2] element into its two scalar components, one per
// output array. Matches array_unpack_vec2.wgsl.
//
// ABI: body(idx, count, e_in) -> BufferOutputs. `in` ([f32;2]) coincident →
// e_in (Element {x, y}); the two outputs x / y (Array(f32)) are returned as a
// BufferOutputs struct the wrapper writes to buf_x[idx] / buf_y[idx]. idx/count
// are the ambient dispatch context (unused — DCE drops them).
fn body(idx: u32, count: u32, e_in: Element) -> BufferOutputs {
    return BufferOutputs(e_in.x, e_in.y);
}
