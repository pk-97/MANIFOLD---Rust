var<private> matter_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return matter_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(matter_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.matter_grid_update — BUFFER body. One thread per node of a held world
// tile (`mgu_node_of`); node
// p sits at world p·cell_size, found through the world-tile table (`buf_tiles`):
//   v_before = momentum / mass       (node.matter_body_reaction replays from it)
//   v = v_before + dt · (g + forces(x))              (forces from tick_index's lattice)
//   v += impulses(x)                                 (substep 0 of impulse_tick)
// where forces and impulses are trilinear reads of the domain's coarse field
// lattices (force_lattice.wgsl; origin field_min, the first node of the
// force lattices). Where the field carries air, v instead relaxes toward the
// wind at its drag rate over dt, exactly integrated (force_lattice_drag_step).
// Then the Bounds box's Wall faces and each collider in body order
// (matter_wall_stop in matter_bounds.wgsl and liquid_collider_project in liquid_collider.wgsl, which
// node.matter_body_reaction repeats to attribute each body's share).
// Each component is then clamped to ±0.9·dx/dt; a clamped node sets
// velocity_before.w.
// `accum` is gathered (4 words per node: momentum xyz in m_unit·U at 2^27, U
// the domain's power-of-two momentum unit that P2G also reads, mass in
// m_unit = 1000·dx³/8 kg at 2^16; D5). Element = MatterGridNode; `bodies`
// (LiquidBody), `shapes` (LiquidShape) and `atlas` (distances two halves
// per word) are gathered; the sampling is liquid_collider.wgsl's.

// The pooled node thread `t` resolves: the dispatch covers only the table's
// held tiles (`held`, from node.matter_to_grid), 512 threads a tile, so
// empty space costs nothing.
fn mgu_node_of(t: u32) -> u32 {
    return buf_held[t >> 9u] * 512u + (t & 511u);
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// The tick's force at x: push and drag rate, then drag rate × wind.
struct GridUpdateForce {
    push_drag: vec4<f32>,
    drag_wind: vec3<f32>,
}

fn grid_update_forces(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> GridUpdateForce {
    var f: GridUpdateForce;
    f.push_drag = vec4<f32>(0.0);
    f.drag_wind = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = base + c.index * FORCE_NODE_WORDS;
        f.push_drag = f.push_drag + vec4<f32>(buf_forces[w], buf_forces[w + 1u], buf_forces[w + 2u], buf_forces[w + 3u]) * c.weight;
        f.drag_wind = f.drag_wind + vec3<f32>(buf_forces[w + 4u], buf_forces[w + 5u], buf_forces[w + 6u]) * c.weight;
    }
    return f;
}

fn grid_update_impulses(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = c.index * 4u;
        sum = sum + vec3<f32>(buf_impulses[w], buf_impulses[w + 1u], buf_impulses[w + 2u]) * c.weight;
    }
    return sum;
}

fn body(
    idx: u32,
    count: u32,
    e_grid: Element,
    e_static_solid: Element2,
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    momentum_unit: f32,
    field_min_x: f32,
    field_min_y: f32,
    field_min_z: f32,
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
    pool_tiles: i32,
    wall_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    wall_velocity_lo_x: f32,
    wall_velocity_lo_y: f32,
    wall_velocity_lo_z: f32,
    wall_velocity_hi_x: f32,
    wall_velocity_hi_y: f32,
    wall_velocity_hi_z: f32,
    wall_friction: f32,
) -> Element {
    matter_pool_tiles = u32(pool_tiles);
    var out: Element;
    out.velocity_mass = vec4<f32>(0.0);
    out.velocity_before = vec4<f32>(0.0);
    if idx >= ts_canon() {
        return out;
    }
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
    ts_home(idx);
    let node = ts_cell_coord(idx);
    let origin = vec3<f32>(field_min_x, field_min_y, field_min_z);
    let x = vec3<f32>(node) * cell_size;
    let field_dims = vec3<u32>(u32(field_nodes_x), u32(field_nodes_y), u32(field_nodes_z));
    var accel = vec3<f32>(gravity_x, gravity, gravity_z);
    var drag = 0.0;
    var drag_wind = vec3<f32>(0.0);
    if force_lattices > 0 {
        let base = force_lattice_base(tick_index, first_tick, force_lattices, field_dims);
        let f = grid_update_forces(x, origin, field_spacing, field_dims, base);
        accel = accel + f.push_drag.xyz;
        drag = f.push_drag.w;
        drag_wind = f.drag_wind;
    }
    var v = v_before + step_dt * accel;
    if drag > 0.0 {
        for (var axis = 0; axis < 3; axis = axis + 1) {
            v[axis] = force_lattice_drag_step(v_before[axis], accel[axis], drag, drag_wind[axis], step_dt);
        }
    }
    if tick_index == impulse_tick && substep_in_tick == 0 {
        v = v + grid_update_impulses(x, origin, field_spacing, field_dims);
    }
    v = matter_wall_stop(
        v, x,
        vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z),
        vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z),
        u32(wall_faces),
        vec3<f32>(wall_velocity_lo_x, wall_velocity_lo_y, wall_velocity_lo_z),
        vec3<f32>(wall_velocity_hi_x, wall_velocity_hi_y, wall_velocity_hi_z),
        wall_friction,
    );
    let solid = vec4<f32>(e_static_solid.x, e_static_solid.y, e_static_solid.z, e_static_solid.w);
    let solid_row = matter_static_row(solid);
    if solid_row >= 0 && solid_row < body_count {
        let sb = buf_bodies[u32(solid_row)];
        v = matter_static_project(
            v, solid,
            liquid_body_velocity(sb.linear_velocity.xyz, sb.angular_velocity.xyz, sb.position_inv_mass.xyz, x),
            sb.linear_velocity.w,
        );
    }

    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        // A static mesh is read from the cache above.
        if sh.tree == 1u {
            continue;
        }
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
