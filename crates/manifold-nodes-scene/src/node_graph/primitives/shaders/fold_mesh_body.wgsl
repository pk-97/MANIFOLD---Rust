// node.fold_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// p = position + source_offset; p = mix(p, reflect(p), amount), where reflect
// flips the coordinate along `axis` (mirror across the plane through the
// frame's origin whose normal is the axis), then shifted back, so a group
// folds about one shared plane. The normal is reflected by the same amount so
// the mirrored half lights correctly. `w` is the optional per-vertex `weights`
// input (degrades to 1.0 past the buffer).
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx]. `weights_len` is a
// derived uniform.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    axis: u32,
    amount: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);

    let pos = e_in.position + offset;
    var refl_pos = pos;
    var refl_nrm = e_in.normal;
    if axis == 0u {
        refl_pos.x = -refl_pos.x;
        refl_nrm.x = -refl_nrm.x;
    } else if axis == 1u {
        refl_pos.y = -refl_pos.y;
        refl_nrm.y = -refl_nrm.y;
    } else {
        refl_pos.z = -refl_pos.z;
        refl_nrm.z = -refl_nrm.z;
    }

    let new_pos = mix(pos, refl_pos, amount * w) - offset;
    let new_nrm = normalize(mix(e_in.normal, refl_nrm, amount * w));
    return Element(new_pos, new_nrm, e_in.uv, e_in.uv1, e_in.tangent, e_in.color);
}
