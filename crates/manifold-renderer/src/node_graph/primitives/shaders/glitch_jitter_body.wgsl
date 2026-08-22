// node.glitch_jitter — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// step = floor(time * rate); pos += (hash(idx * 3 + axis XOR step_key) - 0.5) * amount * w.
// Uses hash_u32 from noise_common.wgsl (prepended via wgsl_includes).
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
    rate: f32,
    seed: f32,
    time: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let step_val = floor(time * rate);
    let step_key = u32(step_val * 12345.0) + u32(seed);

    let dx = hash_u32((idx * 3u + 0u) ^ step_key) - 0.5;
    let dy = hash_u32((idx * 3u + 1u) ^ step_key) - 0.5;
    let dz = hash_u32((idx * 3u + 2u) ^ step_key) - 0.5;

    let jitter = vec3<f32>(dx, dy, dz) * amount * w;
    let displaced = e_in.position + jitter;
    return Element(displaced, e_in.normal, e_in.uv, e_in.tangent);
}
