// node.rotate_4d — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT.
// 4D rotation (XY, ZW, XW planes) of each Vec4Vertex. Matches
// generator_math::rotate_4d / rotate_4d.wgsl bit-for-bit (trig on-GPU both ways).
//
// ABI (buffer standalone codegen): `in` (Vec4Vertex) is coincident, so the
// wrapper pre-reads `e_in = buf_in[idx]` and passes it; the body returns the
// rotated element written to buf_out[idx]. The codegen synthesizes
//   struct Element { x: f32, y: f32, z: f32, w: f32 }
// from Vec4Vertex's Channels signature (stride 16 == the vec4 hand layout).
// active_count == capacity in run() (full pass), so the only guard is the
// wrapper's idx >= count. PARAMS order: angle_xy, angle_zw, angle_xw.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    angle_xy: f32,
    angle_zw: f32,
    angle_xw: f32,
) -> Element {
    var x = e_in.x;
    var y = e_in.y;
    var z = e_in.z;
    var w = e_in.w;

    // XY plane
    let cxy = cos(angle_xy);
    let sxy = sin(angle_xy);
    let nx_xy = x * cxy - y * sxy;
    let ny_xy = x * sxy + y * cxy;
    x = nx_xy;
    y = ny_xy;

    // ZW plane
    let czw = cos(angle_zw);
    let szw = sin(angle_zw);
    let nz_zw = z * czw - w * szw;
    let nw_zw = z * szw + w * czw;
    z = nz_zw;
    w = nw_zw;

    // XW plane
    let cxw = cos(angle_xw);
    let sxw = sin(angle_xw);
    let nx_xw = x * cxw - w * sxw;
    let nw_xw = x * sxw + w * cxw;
    x = nx_xw;
    w = nw_xw;

    return Element(x, y, z, w);
}
