// Ferstl et al. 2016, Narrow Band FLIP: RK4 forward transport before
// particle-to-grid transfer, paired with RK4 distance/face backtraces.
// Solid resolution and domain boundaries reuse the existing FLIP step.
@compute @workgroup_size(256)
fn narrow_move(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.particles { return; }
    var out = sorted[i];
    if u.step_in_tick == 0 { capped[2u*i] = 0u; capped[2u*i+1u] = 0u; }
    if out.position_radius.w <= 0.0 { particles_out[i] = out; return; }
    let n = lattice();
    let q = (out.position_radius.xyz - u.box_min) / u.cell_size;
    let dt = adaptive_step_dt() / u.cell_size;
    let a = sample(q, n, 0u);
    let b = sample(q + 0.5 * dt * a, n, 0u);
    let c = sample(q + 0.5 * dt * b, n, 0u);
    let d = sample(q + dt * c, n, 0u);
    var reached = q + dt * (a + 2.0*b + 2.0*c + d) / 6.0;
    let edge = vec3<f32>(boundary_edge());
    if finite(reached) {
        if !inside_grid(reached, n) {
            reached = clamp_boundary(reached, n, edge.x);
        }
        reached = resolve_solid(q, reached, n, edge);
        if solid_at(reached, n) < 0.0 { reached = push_out(reached, n, edge); }
        capped[2u*i+1u] += push_refused;
    }
    if open_band(reached, n) { out.position_radius.w = 0.0; }
    out.position_radius = vec4<f32>(u.box_min + reached*u.cell_size, out.position_radius.w);
    particles_out[i] = out;
}

@group(0) @binding(45) var<storage, read> narrow_mask: array<u32>;

fn narrow_cell(index: u32) -> bool { return narrow_mask[index] != 0u; }

@group(0) @binding(43) var<storage, read> narrow_status: array<u32>;

@group(0) @binding(44) var<storage, read_write> narrow_failure: array<u32>;

@compute @workgroup_size(1)
fn narrow_latch() {
    narrow_failure[0] = max(narrow_failure[0], narrow_status[0]);
}

@compute @workgroup_size(1)
fn narrow_tally() {
    if u.narrow_band != 0u { capped[6] = bitcast<u32>(1.0); }
    if u.step_in_tick == 0 { capped[NARROW_BAND_SHORTAGE_TAIL] = 0u; }
    capped[NARROW_BAND_SHORTAGE_TAIL] += narrow_status[0];
    // Without regions no emit_tally runs: no births, none refused.
    if u.region_count == 0 {
        capped[INFLOW_BIRTHS_TAIL] = 0u;
        capped[INFLOW_BIRTHS_TAIL + 1u] = 0u;
    }
}

@compute @workgroup_size(256)
fn narrow_disabled(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < cell_total() { cell_out[gid.x] = f32(u.n.x + u.n.y + u.n.z) * u.cell_size; }
}
