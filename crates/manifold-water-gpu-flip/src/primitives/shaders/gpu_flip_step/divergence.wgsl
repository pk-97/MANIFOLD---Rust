// Open fraction of face a at record f: 0 on a box wall, which is closed in
// the solve too, else the solid's.
fn open_at(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 0.0;
    }
    return solid_faces[flatten(f, m)].face_weight[a];
}

// (c − w)·v_s on an inner face; 0 on a box wall, which no solid moves.
fn solid_flux(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>, c: f32) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 0.0;
    }
    let at = flatten(f, m);
    return (c - solid_faces[at].face_weight[a]) * solid_velocity[at].face_velocity[a];
}

// One thread per cell, `faces_in` to `cell_out`: in a water cell, the net
// outflow through its six faces over h, each face's velocity times its open
// fraction (PressureSolver::_calculateNegativeDivergenceVector), plus the
// solids' C·v_s term, (c − w)·v_s outward through each inner face with c the
// cell's open volume; 0 in air.
@compute @workgroup_size(256)
fn divergence(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
        return;
    }
    if !(water[idx] > 0.5) {
        cell_out[idx] = 0.0;
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    let at = flatten(p, m);
    let here = faces_in[at].face_velocity;
    let open_volume = solid_faces[at].face_weight.w;
    var outflow = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        var q = p;
        q[a] = p[a] + 1;
        let upper = open_at(q, a, n, m) * faces_in[flatten(q, m)].face_velocity[a];
        outflow = outflow + upper - open_at(p, a, n, m) * here[a]
            + solid_flux(q, a, n, m, open_volume) - solid_flux(p, a, n, m, open_volume);
    }
    cell_out[idx] = outflow / u.cell_size;
}

