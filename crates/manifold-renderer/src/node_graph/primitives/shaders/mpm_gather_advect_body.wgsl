// node.mpm_gather_advect — fusable BUFFER body, BufferGather. G2P + advect:
// v_p = sum(w * v_i), C_p = 4/h^2 * sum(w * outer(v_i, d)), x_next = x +
// step_dt * v_p, previous accepted position stored. The reconstructed
// density written by mpm_scatter_stress flows through untouched
// (velocity_density.w) — density must reach the candidate this way, never
// through a parallel copy of pre-stress records.
//
// ABI (buffer standalone codegen): `particles` is coincident (pre-read into
// e_particles); `grid` is BufferGather — the body reads the 27 stencil cells
// through `buf_grid` (array<vec4<f32>>, xyz velocity / w mass). Returns the
// advected WaterParticle element.
//
// No fault output: a stencil that leaves the grid cannot be reported here.
// The particle is passed through unchanged (it does not advance) and
// water_validate flags it on the candidate. With the static basin well
// inside the guard shell this path is unreachable — grid no-penetration
// keeps every live particle stencil-contained.
fn body(idx: u32, count: u32, e_particles: Element, step_dt: f32) -> Element {
    var out = e_particles;
    let m = e_particles.position_mass.w;
    if (m == 0.0) {
        return out;
    }
    let pos = e_particles.position_mass.xyz;
    // Fast-math-safe finite guard: absurd positions must not reach the i32
    // base conversion below.
    if (!water_finite3(pos)) {
        return out;
    }
    let q = (pos - WATER_ORIGIN) * WATER_INV_H;
    if (!water_q_plausible(q)) {
        return out;
    }
    let base = water_stencil_base(q);
    if (!water_stencil_contained(base)) {
        return out;
    }
    let f = q - vec3<f32>(base);
    let wx = water_weights(f.x);
    let wy = water_weights(f.y);
    let wz = water_weights(f.z);

    var v = vec3<f32>(0.0);
    // Column accumulators of sum(w * outer(v_i, d)): cc[b] += w * v_i * d[b].
    var cc0 = vec3<f32>(0.0);
    var cc1 = vec3<f32>(0.0);
    var cc2 = vec3<f32>(0.0);
    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var j = 0u; j < 3u; j = j + 1u) {
            for (var i = 0u; i < 3u; i = i + 1u) {
                let cell = water_grid_index(u32(base.x + i32(i)), u32(base.y + i32(j)), u32(base.z + i32(k)));
                let w = wx[i] * wy[j] * wz[k];
                let vi = buf_grid[cell].xyz;
                // d = (node - q) * h = node position - particle position
                // (S1 stencil_offset_d, same formula the scatter uses).
                let node = vec3<f32>(base) + vec3<f32>(f32(i), f32(j), f32(k));
                let d = node * WATER_H + WATER_ORIGIN - pos;
                v = v + w * vi;
                cc0 = cc0 + w * vi * d.x;
                cc1 = cc1 + w * vi * d.y;
                cc2 = cc2 + w * vi * d.z;
            }
        }
    }

    let scale = 4.0 / (WATER_H * WATER_H);
    let c_row0 = vec3<f32>(cc0.x, cc1.x, cc2.x) * scale;
    let c_row1 = vec3<f32>(cc0.y, cc1.y, cc2.y) * scale;
    let c_row2 = vec3<f32>(cc0.z, cc1.z, cc2.z) * scale;

    out.velocity_density = vec4<f32>(v, e_particles.velocity_density.w);
    out.affine_x = vec4<f32>(c_row0, 0.0);
    out.affine_y = vec4<f32>(c_row1, 0.0);
    out.affine_z = vec4<f32>(c_row2, 0.0);
    out.previous_position = vec4<f32>(pos, 0.0);
    out.position_mass = vec4<f32>(pos + step_dt * v, m);
    return out;
}
