// node.matter_grid_update — fusable BUFFER body (GPU_MPM_SOLVER_DESIGN.md
// section 4.1 step 4). One thread per lattice node, x fastest:
//   v_before = momentum / mass                       (Liveliness reads it)
//   v = v_before + dt · g
// Closed faces (bits −X, +X, −Y, +Y, −Z, +Z) stop velocity into the wall on the
// face node and the three padding nodes beyond it (the authored face sits on
// node 3), frictionless (taichi_elements grid_bounding_box). Each
// component is clamped to ±0.9·dx/dt; a clamped node sets velocity_before.w.
// `accum` is gathered (4 words per node: momentum xyz, mass; fixed point in
// m_unit = 1000·dx³/8 kg and m_unit·dx/dt at Q = 2^20). Element = MatterGridNode.
fn body(
    idx: u32,
    count: u32,
    e_grid: Element,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    closed_faces: i32,
) -> Element {
    var out: Element;
    out.velocity_mass = vec4<f32>(0.0);
    out.velocity_before = vec4<f32>(0.0);
    let word = idx * 4u;
    let m_raw = buf_accum[word + 3u];
    if m_raw <= 0 {
        return out;
    }
    let m_norm = f32(m_raw);
    let vel_unit = cell_size / step_dt;
    let v_before = vec3<f32>(
        f32(buf_accum[word]),
        f32(buf_accum[word + 1u]),
        f32(buf_accum[word + 2u]),
    ) / m_norm * vel_unit;
    var v = v_before + step_dt * vec3<f32>(gravity_x, gravity, gravity_z);

    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let faces = u32(closed_faces);
    let low_closed = vec3<bool>((faces & 1u) != 0u, (faces & 4u) != 0u, (faces & 16u) != 0u);
    let high_closed = vec3<bool>((faces & 2u) != 0u, (faces & 8u) != 0u, (faces & 32u) != 0u);
    let stop_low = low_closed & (coord < vec3<u32>(4u)) & (v < vec3<f32>(0.0));
    let stop_high = high_closed & (coord >= n - vec3<u32>(4u)) & (v > vec3<f32>(0.0));
    v = select(v, vec3<f32>(0.0), stop_low | stop_high);

    let limit = 0.9 * vel_unit;
    let clamped = any(abs(v) > vec3<f32>(limit));
    v = clamp(v, vec3<f32>(-limit), vec3<f32>(limit));
    let mass_unit = 125.0 * cell_size * cell_size * cell_size;
    out.velocity_mass = vec4<f32>(v, m_norm / 1048576.0 * mass_unit);
    out.velocity_before = vec4<f32>(v_before, select(0.0, 1.0, clamped));
    return out;
}
