// node.posterize — fusable body fragment (freeze/fusion compiler, design §12).
//
// Convention (see gain_body.wgsl): a PURE `fn body(...)` — own element in, own
// element out. The `levels` param follows the color register (PARAMS order).
//
// Quantize each RGB channel to `levels` discrete steps as round(c*(N-1))/(N-1)
// — endpoints preserved, N=2 gives pure black/white per channel. `levels`
// floors to >= 2. Alpha pass-through. The fusion codegen generates this atom's
// standalone cs_main from the same body (single-source) — matches
// posterize.wgsl exactly (the parity oracle).
fn body(c: vec4<f32>, levels: f32) -> vec4<f32> {
    let n = max(floor(levels), 2.0);
    let steps = n - 1.0;
    let q = round(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * steps) / steps;
    return vec4<f32>(q, c.a);
}
