// node.gain — fusable body fragment (freeze/fusion compiler, design doc §12).
//
// Convention: a PURE `fn body(...)` — own element in, own element out, no
// global accesses (purity-checked). Input color arrives as a `vec4<f32>`
// register; params follow as plain scalar/vec args in PARAMS declaration order
// (here just `gain`); returns the transformed `vec4<f32>` with faithful alpha.
//
// The fusion codegen namespaces this per stable NodeId and chains it with the
// next atom's body (`c = n1_body(c, merged.gain)`), and GENERATES this atom's
// standalone cs_main from the same body (single-source). RGB is scaled, alpha
// passes through — matches gain.wgsl exactly.
fn body(c: vec4<f32>, gain: f32) -> vec4<f32> {
    return vec4<f32>(c.rgb * gain, c.a);
}
