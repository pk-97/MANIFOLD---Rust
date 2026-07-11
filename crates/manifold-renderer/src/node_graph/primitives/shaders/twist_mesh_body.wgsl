// node.twist_mesh — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`. Per-vertex twist about
// `axis` itself: rotates BOTH position and normal by the SAME local
// rotation (D4 exact). Matches twist_mesh.wgsl.
//
// theta(v) = angle * (coord(v) - center) * w (w = weights[idx] or 1.0 past
// weights_len, D2 degrade-to-1.0). Standard right-handed per-axis rotation,
// cyclic: axis=X rotates (y,z), axis=Y rotates (z,x), axis=Z rotates (x,y).
// No pivot subtraction on the rotated pair — the axis passes through
// local-space origin; `center` only shifts where along `axis` theta is
// zero. `angle` is UNBOUNDED at the param level (BUG-039 class) — sin/cos
// here absorb any multiple of 2*pi with no discontinuity.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    axis: u32,
    angle: f32,
    center: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    var coord: f32;
    if axis == 0u {
        coord = e_in.position.x;
    } else if axis == 1u {
        coord = e_in.position.y;
    } else {
        coord = e_in.position.z;
    }
    let theta = angle * (coord - center) * w;
    let c = cos(theta);
    let sn = sin(theta);

    var pos = e_in.position;
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

    return Element(pos, nrm, e_in.uv);
}
