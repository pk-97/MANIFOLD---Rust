// node.twist_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`. Per-vertex twist about
// `axis` itself: rotates BOTH position and normal by the SAME local
// rotation (D4 exact).
//
// The twist runs in a shifted frame: p = position + source_offset, so the
// twist axis passes through the frame's origin and a group twists as one.
// theta(v) = angle * (p_axis - center) * w (w = weights[idx] or 1.0 past
// weights_len, D2 degrade-to-1.0). Standard right-handed per-axis rotation,
// cyclic: axis=X rotates (y,z), axis=Y rotates (z,x), axis=Z rotates (x,y).
// `center` only shifts where along `axis` theta is zero. `angle` is
// UNBOUNDED at the param level (BUG-039 class) — sin/cos here absorb any
// multiple of 2*pi with no discontinuity.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    axis: u32,
    angle: f32,
    center: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);

    var pos = e_in.position + offset;
    var coord: f32;
    if axis == 0u {
        coord = pos.x;
    } else if axis == 1u {
        coord = pos.y;
    } else {
        coord = pos.z;
    }
    let theta = angle * (coord - center) * w;
    let c = cos(theta);
    let sn = sin(theta);

    var nrm = e_in.normal;
    if axis == 0u {
        let py = pos.y;
        let pz = pos.z;
        pos.y = py * c - pz * sn;
        pos.z = py * sn + pz * c;
        let ny = nrm.y;
        let nz = nrm.z;
        nrm.y = ny * c - nz * sn;
        nrm.z = ny * sn + nz * c;
    } else if axis == 1u {
        let pz = pos.z;
        let px = pos.x;
        pos.z = pz * c - px * sn;
        pos.x = pz * sn + px * c;
        let nz = nrm.z;
        let nx = nrm.x;
        nrm.z = nz * c - nx * sn;
        nrm.x = nz * sn + nx * c;
    } else {
        let px = pos.x;
        let py = pos.y;
        pos.x = px * c - py * sn;
        pos.y = px * sn + py * c;
        let nx = nrm.x;
        let ny = nrm.y;
        nrm.x = nx * c - ny * sn;
        nrm.y = nx * sn + ny * c;
    }

    return Element(pos - offset, nrm, e_in.uv, e_in.uv1, e_in.tangent, e_in.color);
}
