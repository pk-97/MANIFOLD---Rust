// node.noise_displace — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// pos += normal * amount * simplex3(pos * frequency + time * speed).
// Uses simplex3d from noise_common.wgsl (prepended via wgsl_includes).
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx] (both coincident
// pre-reads by the wrapper). `weights_len` is a DERIVED uniform — run() packs
// the wired weights buffer's element count, or 0 when unwired. A vertex past
// weights_len degrades to w = 1.0. Normals, uv, tangent pass through unchanged.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    amount: f32,
    frequency: f32,
    speed: f32,
    time: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let p = e_in.position * frequency + time * speed;
    let n = simplex3d(p);
    let displaced = e_in.position + e_in.normal * (amount * n * w);
    return Element(displaced, e_in.normal, e_in.uv, e_in.tangent);
}
