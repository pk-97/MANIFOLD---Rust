// node.voxelize_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`.
// p = position + source_offset; mix(p, round(p / cell_size) * cell_size,
// amount * w), shifted back: the grid sits on the frame's origin, so the
// parts of a group snap to one shared grid.
//
// ABI: e_in = buf_in[idx], e_weights = buf_weights[idx] (both coincident
// pre-reads by the wrapper). `weights_len` is a DERIVED uniform — run() packs
// the wired weights buffer's element count, or 0 when unwired (weights binds a
// filler buffer, so the pre-read stays in-bounds and its garbage is discarded).
// A vertex past weights_len degrades to w = 1.0. Normals, uv, tangent pass
// through unchanged.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    amount: f32,
    cell_size: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    let cs = max(cell_size, 1e-6);
    let pos = e_in.position + offset;
    let voxel_pos = round(pos / cs) * cs;
    let displaced = mix(pos, voxel_pos, amount * w) - offset;
    return Element(displaced, e_in.normal, e_in.uv, e_in.uv1, e_in.tangent, e_in.color);
}
