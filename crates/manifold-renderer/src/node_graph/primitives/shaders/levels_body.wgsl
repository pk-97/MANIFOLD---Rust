// node.levels — fusable body fragment (freeze/fusion compiler, design §12).
//
// Convention (see gain_body.wgsl): a PURE `fn body(...)` — own element in, own
// element out. Params follow the color register in PARAMS declaration order
// (scale, offset, lo, hi, gamma).
//
// Per-channel affine (scale, offset) → clamp(lo, hi) → gamma, alpha
// pass-through. `max(_, 0)` before pow guards the undefined pow(negative) case
// (a misconfigured lo < 0 with a fractional gamma still produces defined
// output). The fusion codegen generates this atom's standalone cs_main from the
// same body (single-source) — matches levels.wgsl exactly (the parity oracle).
fn body(c: vec4<f32>, scale: f32, offset: f32, lo: f32, hi: f32, gamma: f32) -> vec4<f32> {
    let scaled = c.rgb * scale + vec3<f32>(offset);
    let clamped = clamp(scaled, vec3<f32>(lo), vec3<f32>(hi));
    let powered = pow(max(clamped, vec3<f32>(0.0)), vec3<f32>(gamma));
    return vec4<f32>(powered, c.a);
}
