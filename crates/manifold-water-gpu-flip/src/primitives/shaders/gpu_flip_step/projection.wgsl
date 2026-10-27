// φ for the free surface: the particles' distance with `ghost` 1, zero
// (air at zero pressure on its centre) with 0.
fn surface_phi(cell: u32) -> f32 {
    return select(0.0, phi[cell], u.ghost == 1u);
}

// 512 threads a held slot, one per record, in place on
// `faces_rw`. A closed face (a wall's included) is cleared (0 velocity, weight 0) before
// extension; the later solid constraint writes its velocity independently of
// this validity flag (PressureSolver::_applyPressureToVelocityField). An open
// inner face beside water loses (p_upper − p_lower) / h, the air side's
// pressure the ghost value clamp(φ_air / (φ_water + 1e-6), −25, 25) · p_water,
// φ_water taken at most −0.005h and φ_air at least 0 (the matrix uses 1e-9
// instead), and is valid. A face between two air cells is also cleared for
// the extension to fill.
@compute @workgroup_size(256)
fn subtract_pressure(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    let here = faces_rw[at];
    let open = solid_faces[at];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let surface = -0.005 * u.cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        var below = p;
        below[a] = p[a] - 1;
        let upper = at;
        let lower = ts_cell_at(below);
        let wet_upper = water[upper] > 0.5;
        let wet_lower = water[lower] > 0.5;
        if open.face_weight[a] > 0.0 && (wet_upper || wet_lower) {
            var p_upper = pressure[upper];
            var p_lower = pressure[lower];
            if !wet_upper {
                p_upper = clamp(max(surface_phi(upper), 0.0) / (min(surface_phi(lower), surface) + 1e-6), -25.0, 25.0) * p_lower;
            } else if !wet_lower {
                p_lower = clamp(max(surface_phi(lower), 0.0) / (min(surface_phi(upper), surface) + 1e-6), -25.0, 25.0) * p_upper;
            }
            out.face_velocity[a] = here.face_velocity[a] - (p_upper - p_lower) / u.cell_size;
            out.face_weight[a] = 1.0;
        }
    }
    faces_rw[at] = out;
}

// 512 threads a held slot, one per record, in place on
// `faces_rw` (FluidSimulation::_constrainVelocityFieldThread): a
// closed face (open fraction 0) takes the solid's velocity v_s, a cut face
// takes f·v_s + (1 − f)·u with f the solid's friction, an open face keeps u.
// A wall's face is closed like any solid's. Weights pass through.
@compute @workgroup_size(256)
fn constrain_solid_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    var out = faces_rw[at];
    let open = solid_faces[at];
    let solid_here = solid_velocity[at];
    for (var a = 0; a < 3; a = a + 1) {
        let w = open.face_weight[a];
        let v_s = solid_here.face_velocity[a];
        if !(w > 0.0) {
            out.face_velocity[a] = v_s;
        } else if w < 1.0 {
            let f = solid_here.face_weight[a];
            out.face_velocity[a] = f * v_s + (1.0 - f) * out.face_velocity[a];
        }
    }
    faces_rw[at] = out;
}
