// node.melt_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// pos.y -= amount * (simplex3(pos.xz * frequency + seed) * 0.5 + 0.5).
// Uses simplex3d from noise_common.wgsl (prepended via wgsl_includes).
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx]. `weights_len` is a
// derived uniform.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    amount: f32,
    frequency: f32,
    seed: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    let noise_pos = vec3<f32>(
        e_in.position.x * frequency + seed,
        e_in.position.z * frequency + seed,
        0.0,
    );
    let envelope = simplex3d(noise_pos) * 0.5 + 0.5;
    var displaced = e_in.position;
    displaced.y = displaced.y - amount * envelope * w;
    return Element(displaced, e_in.normal, e_in.uv, e_in.tangent);
}
