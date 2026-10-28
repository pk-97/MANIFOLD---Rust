var<private> matter_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return matter_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(matter_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.matter_body_reaction — BUFFER body, atomic scatter
// One thread per pooled world-tile node (node p at world p·cell_size, found
// through `buf_tiles`; the canonical node does nothing),
// after node.matter_grid_update. It repeats grid_update's projection from the
// node's velocity after forces (v_before + dt·(g + forces(x)), plus impulses(x)
// on the impulse tick's first substep, the Bounds box's Wall faces, then each body in order, the
// same helpers in matter_bounds.wgsl, force_lattice.wgsl and liquid_collider.wgsl) and adds what each dynamic
// body (inv_mass > 0) removed from the node, m·(v_in − v_out), to that body's
// 16 words of `reaction_out`:
//   [0..3)  Σ inv_mass·m·Δv                       (velocity change, m/s)
//   [3..6)  Σ (s/n)·inv_mass·m·Δv                 (s = substep_in_tick, n = substeps)
//   [6..9)  Σ inv_mass·((x − x_com) × m·Δv) / dx  (angular impulse, m/s)
//   [9..12) Σ (s/n)·the same
// Each word is value·2^24/U with U the momentum unit, rounded stochastically
// on a key of node, tick and substep so the sum has no bias.
// `grid` (MatterGridNode) is coincident; bodies, shapes and atlas are gathered
// exactly as grid_update gathers them; `reaction` aliases `reaction_out` and is
// never read here.

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn cb_node(i: u32) -> vec4<f32> {
    let n = buf_collider_nodes[i];
    return vec4<f32>(n.x, n.y, n.z, n.w);
}

fn cb_triangle(i: u32) -> vec4<f32> {
    let t = buf_collider_triangles[i];
    return vec4<f32>(t.x, t.y, t.z, t.w);
}

fn reaction_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// floor(x + u) with the carry in integers: an f32 sum x + u would round first
// and bias every word upward by about |x|·2^-24.
fn reaction_encode(x: f32, key: u32, slot: u32) -> i32 {
    let whole = floor(x);
    let fraction = u32((x - whole) * 16777216.0);
    let carry = (fraction + (reaction_hash(key ^ slot) >> 8u)) >> 24u;
    return i32(whole) + i32(carry);
}

fn reaction_add(base: u32, value: vec3<f32>, key: u32) {
    for (var c = 0u; c < 3u; c = c + 1u) {
        let word = base + c;
        let encoded = reaction_encode(value[c], key, word);
        if encoded != 0 {
            atomicAdd(&buf_reaction_out[word], encoded);
        }
    }
}

// The same reads as grid_update_forces and grid_update_impulses.
// The tick's force at x: push and drag rate, then drag rate × wind.
struct BodyReactionForce {
    push_drag: vec4<f32>,
    drag_wind: vec3<f32>,
}

fn body_reaction_forces(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> BodyReactionForce {
    var f: BodyReactionForce;
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

fn body_reaction_impulses(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>) -> vec3<f32> {
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
    substeps_per_tick: i32,
    // Gated on the CPU: run() dispatches nothing when it is 0.
    dynamic_count: i32,
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
) {
    matter_pool_tiles = u32(pool_tiles);
    if idx >= ts_canon() {
        return;
    }
    let m = e_grid.velocity_mass.w;
    if !(m > 0.0) {
        return;
    }
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
        let f = body_reaction_forces(x, origin, field_spacing, field_dims, base);
        accel = accel + f.push_drag.xyz;
        drag = f.push_drag.w;
        drag_wind = f.drag_wind;
    }
    var v = e_grid.velocity_before.xyz + step_dt * accel;
    if drag > 0.0 {
        for (var axis = 0; axis < 3; axis = axis + 1) {
            v[axis] = force_lattice_drag_step(e_grid.velocity_before.xyz[axis], accel[axis], drag, drag_wind[axis], step_dt);
        }
    }
    if tick_index == impulse_tick && substep_in_tick == 0 {
        v = v + body_reaction_impulses(x, origin, field_spacing, field_dims);
    }
    v = matter_wall_stop(
        v, x,
        vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z),
        vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z),
        u32(wall_faces),
        vec3<f32>(wall_velocity_lo_x, wall_velocity_lo_y, wall_velocity_lo_z),
        vec3<f32>(wall_velocity_hi_x, wall_velocity_hi_y, wall_velocity_hi_z),
    );

    let counts = 16777216.0 / momentum_unit;
    let weight = f32(substep_in_tick) / f32(max(substeps_per_tick, 1));
    let key = reaction_hash(idx ^ reaction_hash(u32(tick_index) * 4096u + u32(substep_in_tick)));
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        var projected = v;
        if sh.tree == 1u {
            projected = mt_collider_project(
                v, x, bd.position_inv_mass.xyz, bd.rotation, bd.linear_velocity.xyz,
                bd.angular_velocity.xyz, bd.linear_velocity.w,
                sh.tree_node, sh.tree_triangle, cell_size,
            );
        } else {
            projected = liquid_collider_project(
                v, x, bd.position_inv_mass.xyz, bd.rotation, bd.linear_velocity.xyz,
                bd.angular_velocity.xyz, bd.linear_velocity.w, sh.origin_spacing,
                vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z), sh.atlas_offset, sh.scale_min.xyz,
            );
        }
        let inv_mass = bd.position_inv_mass.w;
        if inv_mass > 0.0 && any(projected != v) {
            let impulse = m * (v - projected);
            let dv = inv_mass * impulse * counts;
            let dl = inv_mass * cross(x - bd.position_inv_mass.xyz, impulse) / cell_size * counts;
            let base = u32(b) * 16u;
            reaction_add(base, dv, key);
            reaction_add(base + 3u, weight * dv, key);
            reaction_add(base + 6u, dl, key);
            reaction_add(base + 9u, weight * dl, key);
        }
        v = projected;
    }
}
