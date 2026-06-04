// node.lerp_instance_fields — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT 2-input. Elementwise lerp of two InstanceTransforms:
// out = (1 - t) * a + t * b, on both pos_scale and rot. Matches
// lerp_instance_fields.wgsl bit-for-bit — uses the SAME explicit
// `a*inv_t + b*t` form (NOT mix(), which can fma and differ by a ULP).
//
// ABI (buffer standalone codegen): both `a` and `b` (InstanceTransform) are
// coincident, so the wrapper pre-reads `e_a = buf_a[idx]` and `e_b = buf_b[idx]`
// and passes them; the body returns the blended element written to buf_out[idx].
// The codegen synthesizes `struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }`
// from InstanceTransform's Channels signature (a/b/out all share it).
fn body(idx: u32, count: u32, e_a: Element, e_b: Element, t: f32) -> Element {
    let inv_t = 1.0 - t;
    return Element(
        e_a.pos_scale * inv_t + e_b.pos_scale * t,
        e_a.rot * inv_t + e_b.rot * t,
    );
}
