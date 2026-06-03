// node.gain — fusable body fragment (freeze/fusion compiler, design doc §12).
//
// Convention: a PURE `fn body(...)` — own element in, own element out, no
// global accesses (purity-checked). The input color arrives as a `vec4<f32>`
// register, followed by the ambient fragment context `uv: vec2<f32>` (normalized
// center-of-texel) and `dims: vec2<f32>` (float canvas size), then the params in
// PARAMS declaration order (here just `gain`). Returns the transformed
// `vec4<f32>` with faithful alpha. Positional atoms (vignette, …) read uv/dims;
// pointwise color atoms like this one ignore them and the codegen's DCE drops
// the unused args.
//
// The fusion codegen namespaces this per stable NodeId and chains it with the
// next atom's body (`c = n1_body(c, uv, dims, merged.gain)`), and GENERATES this
// atom's standalone cs_main from the same body (single-source). RGB is scaled,
// alpha passes through — matches gain.wgsl exactly.
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, gain: f32) -> vec4<f32> {
    return vec4<f32>(c.rgb * gain, c.a);
}
