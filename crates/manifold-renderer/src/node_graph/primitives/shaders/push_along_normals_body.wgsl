// node.push_along_normals — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights` + OPTIONAL TEXTURE `field`.
// pos += normal * amount * w * f. Matches push_along_normals.wgsl.
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx] (both coincident
// pre-reads by the wrapper). `weights_len` is a DERIVED uniform — run() packs
// the wired weights buffer's element count, or 0 when unwired (weights binds a
// filler buffer, so the pre-read stays in-bounds and its garbage is discarded).
// A vertex past weights_len degrades to w = 1.0, never a silent 0. The optional
// `field` Texture2D is `tex_field` + shared `samp`, gated by the injected
// `use_field` flag (run() packs is_some(), binds a 1×1 dummy when unwired).
// Normals pass through unchanged (D4 approximate).
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    tex_field: texture_2d<f32>,
    samp: sampler,
    amount: f32,
    field_bias: f32,
    weights_len: u32,
    use_field: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    var f = 1.0;
    if use_field != 0u {
        f = textureSampleLevel(tex_field, samp, e_in.uv, 0.0).r - field_bias;
    }

    let displaced = e_in.position + e_in.normal * (amount * w * f);
    return Element(displaced, e_in.normal, e_in.uv);
}
