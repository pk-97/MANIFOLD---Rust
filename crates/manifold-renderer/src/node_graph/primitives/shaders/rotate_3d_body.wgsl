// node.rotate_3d — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT.
// XYZ Euler rotation of each MeshVertex's position + normal (X -> Y -> Z order),
// uv passes through. Matches generator_math::rotate_3d / rotate_3d.wgsl
// bit-for-bit (trig computed on-GPU both ways, so identical).
//
// ABI (buffer standalone codegen): `in` (MeshVertex) is coincident, so the
// wrapper pre-reads `e_in = buf_in[idx]` and passes it; the body returns the
// rotated element written to buf_out[idx]. The codegen synthesizes
//   struct Element { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }
// from MeshVertex's Channels signature. active_count == capacity in run() (full
// pass, no inactive collapse), so the only guard is the wrapper's idx >= count.
// The hand kernel's `rotate_xyz` referenced the params global; here it's a
// self-contained helper taking the angles as args.
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

    // Rotate around X
    let ny1 = y * cx - z * sx;
    let nz1 = y * sx + z * cx;
    y = ny1;
    z = nz1;

    // Rotate around Y
    let nx2 = x * cy + z * sy;
    let nz2 = -x * sy + z * cy;
    x = nx2;
    z = nz2;

    // Rotate around Z
    let nx3 = x * cz - y * sz;
    let ny3 = x * sz + y * cz;
    x = nx3;
    y = ny3;

    return vec3<f32>(x, y, z);
}

fn body(idx: u32, count: u32, e_in: Element, angle_x: f32, angle_y: f32, angle_z: f32) -> Element {
    var v = e_in;
    v.position = rotate_xyz_local(e_in.position, angle_x, angle_y, angle_z);
    v.normal = rotate_xyz_local(e_in.normal, angle_x, angle_y, angle_z);
    // uv is a parametric surface value — does not rotate with position.
    return v;
}
