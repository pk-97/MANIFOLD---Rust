// node.rotate_3d — fusable BUFFER body (freeze section 12, buffer domain), COINCIDENT.
// XYZ Euler rotation of each MeshVertex's position + normal (X -> Y -> Z order)
// about the frame's origin: p = position + source_offset is rotated, then
// shifted back, so a group turns as one piece. uv passes through.
//
// ABI (buffer standalone codegen): `in` (MeshVertex) is coincident, so the
// wrapper pre-reads `e_in = buf_in[idx]` and passes it; the body returns the
// rotated element written to buf_out[idx]. active_count == capacity in run()
// (full pass, no inactive collapse), so the only guard is the wrapper's
// idx >= count.
fn rotate_xyz_local(p: vec3<f32>, ax: f32, ay: f32, az: f32) -> vec3<f32> {
    let cx = cos(ax);
    let sx = sin(ax);
    let cy = cos(ay);
    let sy = sin(ay);
    let cz = cos(az);
    let sz = sin(az);

    var x = p.x;
    var y = p.y;
    var z = p.z;

    let ny1 = y * cx - z * sx;
    let nz1 = y * sx + z * cx;
    y = ny1;
    z = nz1;

    let nx2 = x * cy + z * sy;
    let nz2 = -x * sy + z * cy;
    x = nx2;
    z = nz2;

    let nx3 = x * cz - y * sz;
    let ny3 = x * sz + y * cz;
    x = nx3;
    y = ny3;

    return vec3<f32>(x, y, z);
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    angle_x: f32,
    angle_y: f32,
    angle_z: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
) -> Element {
    let offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    var v = e_in;
    v.position = rotate_xyz_local(e_in.position + offset, angle_x, angle_y, angle_z) - offset;
    v.normal = rotate_xyz_local(e_in.normal, angle_x, angle_y, angle_z);
    return v;
}
