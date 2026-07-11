// node.bend_mesh — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`. Classic per-vertex bend:
// rotates BOTH position and normal by the SAME local rotation (D4 exact).
// Matches bend_mesh.wgsl.
//
// Rotation convention (bend axis A, companion C = next(A) in cyclic
// X->Y->Z->X, rotation happens about the THIRD axis B = the one that is
// neither A nor C):
//   axis=X (A=X,C=Y,B=Z): rotate (x,y) about Z, pivoting x by `center`.
//   axis=Y (A=Y,C=Z,B=X): rotate (y,z) about X, pivoting y by `center`.
//   axis=Z (A=Z,C=X,B=Y): rotate (z,x) about Y, pivoting z by `center`.
// s = coord(A) - center; theta = angle * s * w (w = weights[idx] or 1.0
// past weights_len, D2 degrade-to-1.0). Position: A' = center + s*cos -
// C*sin, C' = s*sin + C*cos. Normal: same rotation, NO center pivot (a
// direction, not a point) -- n_A' = n_A*cos - n_C*sin, n_C' = n_A*sin +
// n_C*cos.
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
    let s = coord - center;
    let theta = angle * s * w;
    let c = cos(theta);
    let sn = sin(theta);

    var pos = e_in.position;
    var nrm = e_in.normal;
    if axis == 0u {
        let py = pos.y;
        pos.x = center + s * c - py * sn;
        pos.y = s * sn + py * c;
        let nx = nrm.x;
        let ny = nrm.y;
        nrm.x = nx * c - ny * sn;
        nrm.y = nx * sn + ny * c;
    } else if axis == 1u {
        let pz = pos.z;
        pos.y = center + s * c - pz * sn;
        pos.z = s * sn + pz * c;
        let ny = nrm.y;
        let nz = nrm.z;
        nrm.y = ny * c - nz * sn;
        nrm.z = ny * sn + nz * c;
    } else {
        let px = pos.x;
        pos.z = center + s * c - px * sn;
        pos.x = s * sn + px * c;
        let nz = nrm.z;
        let nx = nrm.x;
        nrm.z = nz * c - nx * sn;
        nrm.x = nz * sn + nx * c;
    }

    return Element(pos, nrm, e_in.uv);
}
