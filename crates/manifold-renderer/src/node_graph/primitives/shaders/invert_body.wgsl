// node.invert — fusable body fragment (freeze/fusion compiler, design §12).
//
// Convention (see gain_body.wgsl): a PURE `fn body(...)` — own element in, own
// element out, no global accesses. The input color arrives as a `vec4<f32>`
// register; params follow in PARAMS declaration order (here just `intensity`).
//
// Inverts RGB, preserves alpha, blends back against the source by intensity.
// The fusion codegen generates this atom's standalone cs_main from the same
// body (single-source) — matches invert.wgsl exactly (the parity oracle).
fn body(c: vec4<f32>, intensity: f32) -> vec4<f32> {
    let inverted = vec4<f32>(1.0 - c.r, 1.0 - c.g, 1.0 - c.b, c.a);
    return mix(c, inverted, intensity);
}
