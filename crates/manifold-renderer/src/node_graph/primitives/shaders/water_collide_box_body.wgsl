// node.water_collide_box — fusable BUFFER body, coincident. Particle boundary
// projection against the translating cube AABB plus the static basin, closing
// the grid-resolution leakage after G2P advection (design step 6). Inside the
// box, the particle is pushed out along the minimum-penetration axis to the
// face (penetration 0 <= 0.1*h acceptance), and the into-surface component of
// the RELATIVE velocity (v - collider_velocity) is removed — free-slip
// tangential, design section 6. Static basin geometry matches the grid; this
// particle stage only corrects penetration. No-slip wall velocity is enforced
// on the grid before G2P.
//
// ABI (buffer standalone codegen): `in` is coincident (pre-read into
// e_particles); the collider translation and velocity arrive as derived
// vec3 uniforms packed by run() from the Transform / ScalarVec3 wires. The
// cube's rotation and scale are display-only (fixed half-extents AABB);
// step_dt is the stage-table contract param — the projection is
// instantaneous, so the body does not integrate it.
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    step_dt: f32,
    cube_half_x: f32,
    cube_half_y: f32,
    cube_half_z: f32,
    basin_min_x: f32,
    basin_min_y: f32,
    basin_min_z: f32,
    basin_max_x: f32,
    basin_max_y: f32,
    basin_max_z: f32,
    collider: vec3<f32>,
    collider_velocity: vec3<f32>,
) -> Element {
    var out = e_particles;
    let m = e_particles.position_mass.w;
    if (m == 0.0) {
        return out;
    }
    var pos = e_particles.position_mass.xyz;
    var v = e_particles.velocity_density.xyz;
    if (!water_finite3(pos) || !water_finite3(v)) {
        return out;
    }

    // Translating cube: fixed half-extents AABB centred on the collider.
    let half = vec3<f32>(cube_half_x, cube_half_y, cube_half_z);
    let d = pos - collider;
    let ad = abs(d);
    if (all(ad < half)) {
        // Distance to each face; push out along the smallest.
        let dx_lo = d.x + half.x;
        let dx_hi = half.x - d.x;
        let dy_lo = d.y + half.y;
        let dy_hi = half.y - d.y;
        let dz_lo = d.z + half.z;
        let dz_hi = half.z - d.z;
        var best = dx_lo;
        var axis = 0u;
        var sgn = -1.0;
        if (dx_hi < best) { best = dx_hi; axis = 0u; sgn = 1.0; }
        if (dy_lo < best) { best = dy_lo; axis = 1u; sgn = -1.0; }
        if (dy_hi < best) { best = dy_hi; axis = 1u; sgn = 1.0; }
        if (dz_lo < best) { best = dz_lo; axis = 2u; sgn = -1.0; }
        if (dz_hi < best) { best = dz_hi; axis = 2u; sgn = 1.0; }

        var n = vec3<f32>(0.0);
        if (axis == 0u) {
            pos.x = collider.x + sgn * half.x;
            n = vec3<f32>(sgn, 0.0, 0.0);
        } else if (axis == 1u) {
            pos.y = collider.y + sgn * half.y;
            n = vec3<f32>(0.0, sgn, 0.0);
        } else {
            pos.z = collider.z + sgn * half.z;
            n = vec3<f32>(0.0, 0.0, sgn);
        }
        // Relative normal velocity at contact: remove only the into-surface
        // normal component of v - collider_velocity; tangential is free-slip.
        let vn = dot(v - collider_velocity, n);
        if (vn < 0.0) {
            v = v - n * vn;
        }
    }

    // Static basin: geometric no-penetration cleanup after advection.
    // The grid stage separately enforces no-slip wall velocity.
    if (pos.x <= basin_min_x) { pos.x = basin_min_x; v.x = max(v.x, 0.0); }
    if (pos.x >= basin_max_x) { pos.x = basin_max_x; v.x = min(v.x, 0.0); }
    if (pos.y <= basin_min_y) { pos.y = basin_min_y; v.y = max(v.y, 0.0); }
    if (pos.y >= basin_max_y) { pos.y = basin_max_y; v.y = min(v.y, 0.0); }
    if (pos.z <= basin_min_z) { pos.z = basin_min_z; v.z = max(v.z, 0.0); }
    if (pos.z >= basin_max_z) { pos.z = basin_max_z; v.z = min(v.z, 0.0); }

    out.position_mass = vec4<f32>(pos, m);
    out.velocity_density = vec4<f32>(v, e_particles.velocity_density.w);
    return out;
}
