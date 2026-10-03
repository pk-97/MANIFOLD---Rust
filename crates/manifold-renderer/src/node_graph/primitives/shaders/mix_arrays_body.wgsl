// node.mix_arrays — fusable BUFFER body (freeze section 12), COINCIDENT 2-input.
// Display-time interpolation of two equal-capacity f32 arrays.
//
// Keep the explicit `a + (b - a) * amount` shape: it is the contract used by
// the CPU reference and preserves the same arithmetic in a fused region.
fn body(idx: u32, count: u32, e_a: f32, e_b: f32, amount: f32) -> f32 {
    let t = clamp(amount, 0.0, 1.0);
    return e_a + (e_b - e_a) * t;
}
