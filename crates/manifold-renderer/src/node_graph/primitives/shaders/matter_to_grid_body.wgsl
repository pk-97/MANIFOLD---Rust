// node.matter_to_grid — ATOMIC SCATTER body (GPU_MPM_SOLVER_DESIGN.md section
// 4.1 step 3, D5). One thread per material point adds mass and momentum to the
// 27 lattice nodes of its quadratic B-spline stencil:
//   mass     += w · m_p
//   momentum += w · (m_p·v_p + (m_p·C_p − dt·V0·(4/dx²)·τ_p·I)·d_i)
// with the water Kirchhoff pressure τ = λ·J·(J − 1) (× Cohesion for J ≥ 1).
// Sums are signed fixed point: mass in m_unit = 1000·dx³/8 kg, momentum in
// m_unit·dx/dt, scaled by Q = 2^20 and rounded per contribution. Integer adds
// are order-independent, so the grid is deterministic. Accumulator words per
// node: momentum x, y, z, mass. `accum` (the aliased input) is not read.
// Element = MatterPoint.
fn body(
    idx: u32,
    count: u32,
    e_points: Element,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    step_dt: f32,
    lambda: f32,
    cohesion: f32,
    density: f32,
    active_count: i32,
) {
    if e_points.id == 0u {
        return;
    }
    let origin = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let inv_dx = 1.0 / cell_size;
    let q = (e_points.position - origin) * inv_dx;
    let base = floor(q - vec3<f32>(0.5));
    let nodes = vec3<i32>(nodes_x, nodes_y, nodes_z);
    let base_i = vec3<i32>(base);
    if any(base_i < vec3<i32>(0)) || any(base_i + vec3<i32>(2) > nodes - vec3<i32>(1)) {
        return;
    }
    let f = q - base;
    let w0 = 0.5 * (vec3<f32>(1.5) - f) * (vec3<f32>(1.5) - f);
    let w1 = vec3<f32>(0.75) - (f - vec3<f32>(1.0)) * (f - vec3<f32>(1.0));
    let w2 = 0.5 * (f - vec3<f32>(0.5)) * (f - vec3<f32>(0.5));

    let j = e_points.volume_ratio;
    let v0 = e_points.affine_y.w;
    let mass = v0 * density;
    var tau = lambda * j * (j - 1.0);
    if j >= 1.0 {
        tau = tau * cohesion;
    }
    let stress = step_dt * v0 * 4.0 * inv_dx * inv_dx * tau;
    // Affine rows: m·C − stress·I.
    let a0 = mass * e_points.affine_x.xyz - vec3<f32>(stress, 0.0, 0.0);
    let a1 = mass * e_points.affine_y.xyz - vec3<f32>(0.0, stress, 0.0);
    let a2 = mass * e_points.affine_z.xyz - vec3<f32>(0.0, 0.0, stress);
    let mv = mass * e_points.velocity;

    let mass_unit = 125.0 * cell_size * cell_size * cell_size;
    let to_mass = 1048576.0 / mass_unit;
    let to_momentum = to_mass * step_dt * inv_dx;

    for (var a = 0; a < 3; a = a + 1) {
        var wx = w0.x;
        if a == 1 { wx = w1.x; } else if a == 2 { wx = w2.x; }
        for (var b = 0; b < 3; b = b + 1) {
            var wy = w0.y;
            if b == 1 { wy = w1.y; } else if b == 2 { wy = w2.y; }
            for (var c = 0; c < 3; c = c + 1) {
                var wz = w0.z;
                if c == 1 { wz = w1.z; } else if c == 2 { wz = w2.z; }
                let weight = wx * wy * wz;
                let d = (vec3<f32>(f32(a), f32(b), f32(c)) - f) * cell_size;
                let momentum = weight * (mv + vec3<f32>(dot(a0, d), dot(a1, d), dot(a2, d)));
                let node = base_i + vec3<i32>(a, b, c);
                let slot = u32((node.z * nodes.y + node.y) * nodes.x + node.x) * 4u;
                atomicAdd(&buf_accum_out[slot], i32(round(momentum.x * to_momentum)));
                atomicAdd(&buf_accum_out[slot + 1u], i32(round(momentum.y * to_momentum)));
                atomicAdd(&buf_accum_out[slot + 2u], i32(round(momentum.z * to_momentum)));
                atomicAdd(&buf_accum_out[slot + 3u], i32(round(weight * mass * to_mass)));
            }
        }
    }
}
