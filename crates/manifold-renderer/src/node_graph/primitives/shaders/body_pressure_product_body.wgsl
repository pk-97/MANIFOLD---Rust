// Ported from FLIP Fluids pressuresolver.cpp and rigidboundaryvelocity.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.body_pressure_product — fusable BUFFER body, GATHER. One thread per
// cell. The bodies' share of the pressure operator, J M⁻¹ Jᵀ in the
// engine's coupled matrix: in a water cell, base plus (1/h)·Σ over its six
// inner faces a body owns of sign·(c − w)·(dv + dω × r)[a], sign +1 on the
// cell's high face and −1 on its low, c the cell's open volume, w the
// face's open fraction, r the face centre less the body's centre of mass
// posed tick_seconds on, and dv, dω the body's velocity change from `sums`
// (node.face_impulse_to_bodies of node.pressure_face_impulse of the same
// pressure). With M⁻¹ in the sums this is ρh·G M⁻¹ Gᵀ p: the solve's
// −L p plus it is the coupled operator. Air cells keep base.
//
// ABI: `base` is read coincident, the output's one anchor; `water`,
// `solid_faces`, `solid_velocity`, `sums` and `bodies` gathered. A body past `sums` or
// `bodies` adds nothing.

fn body(
    idx: u32,
    count: u32,
    e_base: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    body_count: f32,
    rows: f32,
    tick_seconds: f32,
) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > arrayLength(&buf_water) || faces > min(arrayLength(&buf_solid_faces), arrayLength(&buf_solid_velocity)) {
        return e_base;
    }
    if !(buf_water[idx] > 0.5) {
        return e_base;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let first = max(i32(rows) - i32(body_count), 0);
    let c = buf_solid_faces[u32(p.x + m.x * (p.y + m.y * p.z))].face_weight.w;
    var total = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var side = 0; side < 2; side = side + 1) {
            var f = p;
            f[a] = p[a] + side;
            if f[a] == 0 || f[a] == n[a] {
                continue;
            }
            let at = u32(f.x + m.x * (f.y + m.y * f.z));
            let owner = solid_owner(buf_solid_velocity[at].face_velocity.w, a);
            let row = u32(first + owner);
            if owner < 0 || 16u * u32(owner + 1) > arrayLength(&buf_sums) || row >= arrayLength(&buf_bodies) {
                continue;
            }
            let bd = buf_bodies[row];
            let centre = fma(bd.linear_velocity.xyz, vec3<f32>(tick_seconds), bd.position_inv_mass.xyz);
            let s = 16u * u32(owner);
            let dv = vec3<f32>(buf_sums[s + 8u], buf_sums[s + 9u], buf_sums[s + 10u]);
            let dw = vec3<f32>(buf_sums[s + 12u], buf_sums[s + 13u], buf_sums[s + 14u]);
            let r = solid_face_centre(lattice_min, f, a, cell_size) - centre;
            let sign = select(-1.0, 1.0, side == 1);
            total = fma(sign * (c - buf_solid_faces[at].face_weight[a]), solid_basis_dot(a, r, dv, dw), total);
        }
    }
    return e_base + total / cell_size;
}
