// node.matter_grid_update — fusable BUFFER body (GPU_MPM_SOLVER_DESIGN.md
// section 4.1 step 4). One thread per lattice node, x fastest:
//   v_before = momentum / mass                       (Liveliness reads it)
//   v = v_before + dt · (g + forces(x))              (forces from tick_index's lattice)
//   v += impulses(x)                                 (substep 0 of impulse_tick)
// where forces and impulses are trilinear reads of the domain's coarse field
// lattices (liquid_field.wgsl; origin the lattice minimum, 4 floats a node).
// Then the closed walls and each collider in body order (matter_wall_stop in
// matter_walls.wgsl and liquid_collider_project in liquid_collider.wgsl, which
// node.matter_body_reaction repeats to attribute each body's share).
// Each component is then clamped to ±0.9·dx/dt; a clamped node sets
// velocity_before.w.
// `accum` is gathered (4 words per node: momentum xyz in m_unit·U at 2^27, U
// the domain's power-of-two momentum unit that P2G also reads, mass in
// m_unit = 1000·dx³/8 kg at 2^16; D5). Element = MatterGridNode; `bodies`
// (LiquidBody), `shapes` (LiquidShape) and `atlas` (distances two halves
// per word) are gathered; the sampling is liquid_collider.wgsl's.

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn grid_update_forces(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, spacing, dims, k);
        let w = c.index * 4u;
        sum = sum + vec3<f32>(buf_forces[base + w], buf_forces[base + w + 1u], buf_forces[base + w + 2u]) * c.weight;
    }
    return sum;
}

fn grid_update_impulses(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, spacing, dims, k);
        let w = c.index * 4u;
        sum = sum + vec3<f32>(buf_impulses[w], buf_impulses[w + 1u], buf_impulses[w + 2u]) * c.weight;
    }
    return sum;
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
    tick_index: i32,
    substep_in_tick: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
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
    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let origin = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let x = origin + vec3<f32>(coord) * cell_size;
    let field_dims = vec3<u32>(u32(field_nodes_x), u32(field_nodes_y), u32(field_nodes_z));
    var accel = vec3<f32>(gravity_x, gravity, gravity_z);
    if force_lattices > 0 {
        let base = liquid_field_force_base(tick_index, first_tick, force_lattices, field_dims);
        accel = accel + grid_update_forces(x, origin, field_spacing, field_dims, base);
    }
    var v = v_before + step_dt * accel;
    if tick_index == impulse_tick && substep_in_tick == 0 {
        v = v + grid_update_impulses(x, origin, field_spacing, field_dims);
    }
    v = matter_wall_stop(v, coord, n, u32(closed_faces));

    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        v = liquid_collider_project(
            v, x, bd.position_inv_mass.xyz, bd.rotation, bd.linear_velocity.xyz,
            bd.angular_velocity.xyz, bd.linear_velocity.w, sh.origin_spacing,
            vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z), sh.atlas_offset, sh.scale_min.xyz,
        );
    }

    let limit = 0.9 * vel_unit;
    let clamped = any(abs(v) > vec3<f32>(limit));
    v = clamp(v, vec3<f32>(-limit), vec3<f32>(limit));
    let mass_unit = 125.0 * cell_size * cell_size * cell_size;
    out.velocity_mass = vec4<f32>(v, m_norm / 65536.0 * mass_unit);
    out.velocity_before = vec4<f32>(v_before, select(0.0, 1.0, clamped));
    return out;
}
