// Open fraction of face a at record f: the solid's, which is 0 across a wall.
fn open_at(f: vec3<i32>, a: i32) -> f32 {
    return solid_faces[ts_face(f)].face_weight[a];
}

// (c − w)·v_s on a face: the solid's flux through the part of it the solid
// covers.
fn solid_flux(f: vec3<i32>, a: i32, c: f32) -> f32 {
    let at = ts_face(f);
    return (c - solid_faces[at].face_weight[a]) * solid_velocity[at].face_velocity[a];
}

// 512 threads a slot of C, `faces_in` to `cell_out`: in a water cell, the net
// outflow through its six faces over h, each face's velocity
// times its open fraction (PressureSolver::_calculateNegativeDivergenceVector),
// plus the solids' C·v_s term, (c − w)·v_s outward through each face
// with c the cell's open volume, less the surface tension's share; 0 in air.
@compute @workgroup_size(256)
fn divergence(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let p = ts_cell_coord(idx);
    if !(water[idx] > 0.5) {
        cell_out[idx] = 0.0;
        return;
    }
    let here = faces_in[idx].face_velocity;
    let open_volume = solid_faces[idx].face_weight.w;
    var outflow = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        var q = p;
        q[a] = p[a] + 1;
        let upper = open_at(q, a) * faces_in[ts_face(q)].face_velocity[a];
        outflow = outflow + upper - open_at(p, a) * here[a]
            + solid_flux(q, a, open_volume) - solid_flux(p, a, open_volume);
    }
    // Surface tension: an open face to air takes the ghost pressure θ·p +
    // (1 − θ)·p_surface (projection.wgsl), whose known part moves here as
    // the outflow −w·(1 − θ)·p_surface / h it would drive.
    let surface = surface_pressure(p);
    if surface != 0.0 {
        for (var k = 0; k < 6; k = k + 1) {
            let a = k / 2;
            let d = select(-1, 1, (k & 1) == 1);
            var q = p;
            q[a] = p[a] + d;
            let air = ts_cell_at(q);
            if water[air] > 0.5 {
                continue;
            }
            let w = open_at(select(p, q, d > 0), a);
            outflow = outflow - w * (1.0 - ghost_share(idx, air)) * surface / u.cell_size;
        }
    }
    cell_out[idx] = outflow / u.cell_size;
}
