// node.ripple_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// pos += normal * amplitude * sin(dot(pos, dir) * frequency - time * speed),
// where dir is the unit vector along `axis`.
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx]. `weights_len` is a
// derived uniform.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    amplitude: f32,
    frequency: f32,
    speed: f32,
    axis: u32,
    time: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    var dir = vec3<f32>(0.0, 1.0, 0.0);
    if axis == 0u {
        dir = vec3<f32>(1.0, 0.0, 0.0);
    } else if axis == 2u {
        dir = vec3<f32>(0.0, 0.0, 1.0);
    }

    let phase = dot(e_in.position, dir) * frequency - time * speed;
    let displaced = e_in.position + e_in.normal * (amplitude * sin(phase) * w);
    return Element(displaced, e_in.normal, e_in.uv, e_in.tangent);
}
