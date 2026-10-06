// node.cut_out_box — fusable BUFFER body. Multiplies vertex alpha by 0 inside
// a world box, ramping back to 1 over `feather` metres outside it, so a
// material in alpha Mask mode hides that part of the mesh
// (docs/OCEAN_SURFACE_DESIGN.md D7).
//
// ABI (buffer standalone codegen): `mesh` is coincident (e_mesh); params in
// PARAMS order.

fn body(
    idx: u32,
    count: u32,
    e_mesh: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    feather: f32,
) -> Element {
    var v = e_mesh;
    let q = abs(v.position - vec3<f32>(center_x, center_y, center_z)) - 0.5 * vec3<f32>(size_x, size_y, size_z);
    // Outside distance along the worst axis; negative inside the box.
    let d = max(q.x, max(q.y, q.z));
    let keep = select(f32(d >= 0.0), clamp(d / feather, 0.0, 1.0), feather > 0.0);
    v.color = vec4<f32>(v.color.rgb, v.color.a * keep);
    return v;
}
