// node.matter_grid_update — fusable BUFFER body (GPU_MPM_SOLVER_DESIGN.md
// section 4.1 step 4). One thread per lattice node, x fastest:
//   v_before = momentum / mass                       (Liveliness reads it)
//   v = v_before + dt · g
// Closed faces (bits −X, +X, −Y, +Y, −Z, +Z) stop velocity into the wall on the
// face node and the three padding nodes beyond it (the authored face sits on
// node 3), frictionless (taichi_elements grid_bounding_box). Colliders (D11):
// a node inside a body (φ < 0, sampled through the body's pose and scale from
// its shape's lattice in the atlas) moving into it, v_rel · n < 0 with
// v_rel = v − v_body(x), loses the normal part of v_rel and keeps
// t̂·max(0, |t| + v_n·friction) of the tangential part; v = v_body + v_rel'.
// Each component is then clamped to ±0.9·dx/dt; a clamped node sets
// velocity_before.w.
// `accum` is gathered (4 words per node: momentum xyz in m_unit·U at 2^27, U
// the domain's power-of-two momentum unit that P2G also reads, mass in
// m_unit = 1000·dx³/8 kg at 2^16; D5). Element = MatterGridNode; `bodies`
// (MatterBody), `shapes` (MatterShape) and `atlas` (distances two halves
// per word) are gathered; the sampling is matter_collider.wgsl's.

fn matter_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

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
    momentum_unit: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    body_count: i32,
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
    // U · 2^16 / 2^27 is exact: the inverse of P2G's scale ratio.
    let v_before = vec3<f32>(
        f32(buf_accum[word]),
        f32(buf_accum[word + 1u]),
        f32(buf_accum[word + 2u]),
    ) / m_norm * (momentum_unit * (65536.0 / 134217728.0));
    var v = v_before + step_dt * vec3<f32>(gravity_x, gravity, gravity_z);

    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let faces = u32(closed_faces);
    let low_closed = vec3<bool>((faces & 1u) != 0u, (faces & 4u) != 0u, (faces & 16u) != 0u);
    let high_closed = vec3<bool>((faces & 2u) != 0u, (faces & 8u) != 0u, (faces & 32u) != 0u);
    let stop_low = low_closed & (coord < vec3<u32>(4u)) & (v < vec3<f32>(0.0));
    let stop_high = high_closed & (coord >= n - vec3<u32>(4u)) & (v > vec3<f32>(0.0));
    v = select(v, vec3<f32>(0.0), stop_low | stop_high);

    let x = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z) + vec3<f32>(coord) * cell_size;
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let centre = bd.position_inv_mass.xyz;
        let g = matter_lattice_coord(x, centre, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if !matter_lattice_holds(g, dims) || matter_lattice_distance(sh.atlas_offset, dims, g) >= 0.0 {
            continue;
        }
        let grad = matter_lattice_gradient(sh.atlas_offset, dims, g, sh.origin_spacing.w, bd.rotation, sh.scale_min.xyz);
        let length_sq = dot(grad, grad);
        if !(length_sq > 0.0) {
            continue;
        }
        let normal = grad * inverseSqrt(length_sq);
        let v_body = matter_body_velocity(bd.linear_velocity.xyz, bd.angular_velocity.xyz, centre, x);
        let v_rel = v - v_body;
        let v_n = dot(v_rel, normal);
        if v_n < 0.0 {
            let t = v_rel - v_n * normal;
            let t_len = length(t);
            let keep = max(0.0, t_len + v_n * bd.linear_velocity.w);
            v = v_body + select(vec3<f32>(0.0), t * (keep / t_len), t_len > 0.0);
        }
    }

    let limit = 0.9 * vel_unit;
    let clamped = any(abs(v) > vec3<f32>(limit));
    v = clamp(v, vec3<f32>(-limit), vec3<f32>(limit));
    let mass_unit = 125.0 * cell_size * cell_size * cell_size;
    out.velocity_mass = vec4<f32>(v, m_norm / 65536.0 * mass_unit);
    out.velocity_before = vec4<f32>(v_before, select(0.0, 1.0, clamped));
    return out;
}
