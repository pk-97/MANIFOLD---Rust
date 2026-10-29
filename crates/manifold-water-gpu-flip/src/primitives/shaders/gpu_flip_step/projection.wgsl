// φ for the free surface: the particles' distance with `ghost` 1, zero
// (air at zero pressure on its centre) with 0.
fn surface_phi(cell: u32) -> f32 {
    return select(0.0, phi[cell], u.ghost == 1u);
}

// The ghost share θ of air cell `air` beside water cell `wet`: its ghost
// pressure is θ · p_water + (1 − θ) · p_surface, the line through the water's
// pressure and the surface's, θ = clamp(φ_air / (φ_water + 1e-6), −25, 25)
// with φ_water taken at most −0.005h and φ_air at least 0 (the matrix uses
// 1e-9 instead).
fn ghost_share(wet: u32, air: u32) -> f32 {
    let surface = -0.005 * u.cell_size;
    return clamp(max(surface_phi(air), 0.0) / (min(surface_phi(wet), surface) + 1e-6), -25.0, 25.0);
}

// φ at cell coordinate c.
fn phi_at(c: vec3<i32>) -> f32 {
    return phi[ts_cell_at(c)];
}

// The water's pressure at the free surface beside the water cell at `c`, in
// the solve's units (pressure · step_dt / density): σ/ρ · κ · step_dt, κ the
// mean curvature ∇·(∇φ/|∇φ|) at the cell from central differences, held
// within ±1/h so the cell of a lone particle does not kick it. Positive where
// the water bulges out, so bumps and drops are pressed round and flat. 0
// without ghost fluid or tension.
fn surface_pressure(c: vec3<i32>) -> f32 {
    if u.ghost != 1u || !(u.tension > 0.0) {
        return 0.0;
    }
    let h = u.cell_size;
    let f = phi_at(c);
    let px = phi_at(c + vec3<i32>(1, 0, 0));
    let mx = phi_at(c - vec3<i32>(1, 0, 0));
    let py = phi_at(c + vec3<i32>(0, 1, 0));
    let my = phi_at(c - vec3<i32>(0, 1, 0));
    let pz = phi_at(c + vec3<i32>(0, 0, 1));
    let mz = phi_at(c - vec3<i32>(0, 0, 1));
    let fx = (px - mx) * 0.5;
    let fy = (py - my) * 0.5;
    let fz = (pz - mz) * 0.5;
    let g2 = fx * fx + fy * fy + fz * fz;
    if !(g2 > 1e-12 * h * h) {
        return 0.0;
    }
    let fxx = px - 2.0 * f + mx;
    let fyy = py - 2.0 * f + my;
    let fzz = pz - 2.0 * f + mz;
    let fxy = 0.25 * (phi_at(c + vec3<i32>(1, 1, 0)) - phi_at(c + vec3<i32>(1, -1, 0))
        - phi_at(c + vec3<i32>(-1, 1, 0)) + phi_at(c + vec3<i32>(-1, -1, 0)));
    let fxz = 0.25 * (phi_at(c + vec3<i32>(1, 0, 1)) - phi_at(c + vec3<i32>(1, 0, -1))
        - phi_at(c + vec3<i32>(-1, 0, 1)) + phi_at(c + vec3<i32>(-1, 0, -1)));
    let fyz = 0.25 * (phi_at(c + vec3<i32>(0, 1, 1)) - phi_at(c + vec3<i32>(0, 1, -1))
        - phi_at(c + vec3<i32>(0, -1, 1)) + phi_at(c + vec3<i32>(0, -1, -1)));
    // Differences are per cell, so κ comes out per cell; over h it is per metre.
    let k = (fxx * (fy * fy + fz * fz) + fyy * (fx * fx + fz * fz) + fzz * (fx * fx + fy * fy)
        - 2.0 * (fx * fy * fxy + fx * fz * fxz + fy * fz * fyz)) / (g2 * sqrt(g2));
    let kappa = clamp(k, -1.0, 1.0) / h;
    return u.tension * kappa * adaptive_step_dt();
}

// 512 threads a held slot, one per record, in place on
// `faces_rw`. A closed face (a wall's included) is cleared (0 velocity, weight 0) before
// extension; the later solid constraint writes its velocity independently of
// this validity flag (PressureSolver::_applyPressureToVelocityField). An open
// inner face beside water loses (p_upper − p_lower) / h, the air side's
// pressure its ghost value (`ghost_share`, with the surface's pressure from
// `surface_pressure`), and is valid. A face between two air cells is also cleared for
// the extension to fill.
@compute @workgroup_size(256)
fn subtract_pressure(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    let here = faces_rw[at];
    let open = solid_faces[at];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
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
                let share = ghost_share(lower, upper);
                p_upper = share * p_lower + (1.0 - share) * surface_pressure(below);
            } else if !wet_lower {
                let share = ghost_share(upper, lower);
                p_lower = share * p_upper + (1.0 - share) * surface_pressure(p);
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
