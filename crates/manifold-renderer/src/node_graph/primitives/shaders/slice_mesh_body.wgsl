// node.slice_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// Verts past the cut plane along `axis` clamp onto the plane.
//   axis=0: if pos.x > cut, pos.x = cut
//   axis=1: if pos.y > cut, pos.y = cut
//   axis=2: if pos.z > cut, pos.z = cut
// `w` is the optional per-vertex `weights` input (degrades to 1.0 past the
// buffer). Normals, uv, tangent pass through unchanged.
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx]. `weights_len` is a
// derived uniform.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    axis: u32,
    cut: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    var p = e_in.position;
    if axis == 0u {
        p.x = min(p.x, mix(p.x, cut, w));
    } else if axis == 1u {
        p.y = min(p.y, mix(p.y, cut, w));
    } else {
        p.z = min(p.z, mix(p.z, cut, w));
    }

    return Element(p, e_in.normal, e_in.uv, e_in.tangent);
}
