// The force lattice at x along one axis: (push, drag rate, drag rate × wind).
fn gravity_force(x: vec3<f32>, origin: vec3<f32>, base: u32, axis: u32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, u.field_spacing, u.field_nodes, k);
        let w = base + c.index * FORCE_NODE_WORDS;
        sum = fma(vec3<f32>(forces[w + axis], forces[w + 3u], forces[w + 4u + axis]), vec3<f32>(c.weight), sum);
    }
    return sum;
}

fn gravity_impulse(x: vec3<f32>, origin: vec3<f32>, axis: u32) -> f32 {
    var sum = 0.0;
    let event = clock_plan[0].event;
    let live = (event & 0x80000000u) != 0u && u.live_impulse_stride > 0u;
    let base = select(0u, (event & 0x7fffffffu) * u.live_impulse_stride, live);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, u.field_spacing, u.field_nodes, k);
        let value = impulses[base + c.index * 4u + axis];
        sum = fma(value, c.weight, sum);
    }
    return sum;
}

// The world-space signed distance at x to region r of this substep's tick,
// posed tick_seconds into the tick as closest_body poses a body; far
// outside (1e30) when the region is off or its lattice does not hold x.
fn region_distance(r: i32, x: vec3<f32>) -> f32 {
    let row = (u.tick_index - u.first_tick) * u.region_count + r;
    if row < 0 || row >= u.region_rows {
        return 1e30;
    }
    let bd = regions[u32(row)];
    let shape_index = i32(bd.accel_shape.w);
    if shape_index < 0 || u32(shape_index) >= u.shapes_len {
        return 1e30;
    }
    let pose_time = adaptive_tick_seconds();
    let position = fma(bd.linear_velocity.xyz, vec3<f32>(pose_time), bd.position_inv_mass.xyz);
    let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, pose_time);
    let sh = shapes[u32(shape_index)];
    let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
    let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
    if !liquid_lattice_holds(g, dims) {
        return 1e30;
    }
    return liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w;
}

// The first region of kind `code` (2 inflow, 3 outflow) holding x: distance
// at or below 0 when `closed`, below 0 otherwise; −1 when none does.
fn region_holding(x: vec3<f32>, code: f32, closed: bool) -> i32 {
    let first = (u.tick_index - u.first_tick) * u.region_count;
    for (var r = 0; r < u.region_count; r = r + 1) {
        let row = first + r;
        if row < 0 || row >= u.region_rows || regions[u32(row)].angular_velocity.w != code {
            continue;
        }
        let d = region_distance(r, x);
        if d < 0.0 || (closed && d <= 0.0) {
            return r;
        }
    }
    return -1;
}

// The Water region r's births carry (bodies.rs region_row).
fn region_water(r: i32) -> u32 {
    return u32(regions[u32((u.tick_index - u.first_tick) * u.region_count + r)].inv_inertia_y.x);
}

// An inflow's velocity at x: its authored velocity plus its share of the
// region's own rigid motion there (the engine's append-object-velocity).
fn region_velocity(r: i32, x: vec3<f32>) -> vec3<f32> {
    let bd = regions[u32((u.tick_index - u.first_tick) * u.region_count + r)];
    let pose_time = adaptive_tick_seconds();
    let position = fma(bd.linear_velocity.xyz, vec3<f32>(pose_time), bd.position_inv_mass.xyz);
    let rigid = bd.linear_velocity.xyz + cross(bd.angular_velocity.xyz, x - position);
    return bd.inv_inertia_x.xyz + bd.inv_inertia_x.w * rigid;
}

// 512 threads a held slot, one per record, `faces_in` to
// `faces_out`: each face gains
// step_dt · (g + forces(x)) along its normal a, x its centre, relaxing toward
// the wind at the air's drag rate where the field carries air (exactly
// integrated, force_lattice_drag_step), plus the
// impulses on step 0 of impulse_tick, read from the domain's coarse field
// lattices (origin `field_min`). Weights pass through.
@compute @workgroup_size(256)
fn face_gravity(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    let here = faces_in[at];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let origin = u.field_min;
    var force_base = 0u;
    if u.force_lattices > 0 {
        force_base = force_lattice_base(u.tick_index, u.first_tick, u.force_lattices, u.field_nodes);
    }
    let live_event = (clock_plan[0].event & 0x80000000u) != 0u;
    let impulse = select(u.tick_index == u.impulse_tick && u.step_in_tick == 0,
        live_event, clock_live());
    for (var a = 0; a < 3; a = a + 1) {
        out.face_weight[a] = here.face_weight[a];
        var centre = vec3<f32>(p) + vec3<f32>(0.5);
        centre[a] = f32(p[a]);
        let x = fma(centre, vec3<f32>(u.cell_size), u.origin);
        var accel = u.gravity[a];
        var air = vec2<f32>(0.0);
        if u.force_lattices > 0 {
            let f = gravity_force(x, origin, force_base, u32(a));
            accel = accel + f.x;
            air = f.yz;
        }
        // A valid face inside an inflow takes no body force
        // (_getInflowConstrainedVelocityComponents 6112-6175, before the body
        // forces; valid is weight > 0, the engine's _validVelocities after
        // extension). Nothing pins faces after the solve: the engine's
        // _constrainVelocityFields is solids only.
        if u.region_count > 0 && here.face_weight[a] > 0.0 && region_holding(x, 2.0, false) >= 0 {
            accel = 0.0;
            air = vec2<f32>(0.0);
        }
        var v = fma(accel, adaptive_step_dt(), here.face_velocity[a]);
        if air.x > 0.0 {
            v = force_lattice_drag_step(here.face_velocity[a], accel, air.x, air.y, adaptive_step_dt());
        }
        if impulse {
            v = v + gravity_impulse(x, origin, u32(a));
        }
        out.face_velocity[a] = v;
    }
    faces_out[at] = out;
}

// The first Sand at a held cell's low corner (origin + p·h), read from its
// grid as its tick ended.
fn sand_corner(at: u32) -> SandHere {
    return sand_at(u.origin + vec3<f32>(ts_cell_coord(at)) / u.inv_cell, u.sand_inv_cell);
}

// 512 threads a held slot, after `face_gravity`: sand is a porous moving
// solid, so the water inside it is drawn toward the sand's mean velocity at
// the cell's low corner by how full of sand it is there, which stands for
// the cell's three faces.
@compute @workgroup_size(256)
fn sand_drag(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let sand = sand_corner(at);
    if !(sand.fraction > 0.0) { return; }
    var here = faces_out[at];
    let keep = exp(-u.sand_drag * min(sand.fraction, 1.0) * adaptive_step_dt());
    here.face_velocity = vec4<f32>(sand.velocity + (here.face_velocity.xyz - sand.velocity) * keep, here.face_velocity.w);
    faces_out[at] = here;
}

// 512 threads a held slot, after the solids' velocity (`faces_rw` the open
// fractions, `faces_out` the solid velocity): sand is a porous moving solid
// in the solve. On each of the cell's three faces no body or wall cuts, the
// grains' solid share at the cell's low corner (standing for its three
// faces, as in `sand_drag`) closes that much of the face, and the closed
// part moves at the sand's mean velocity, with no friction and no body's
// reaction. The divergence then holds the mixture's flux, so the water makes
// way for sand coming in and passes a pile only through its pores.
@compute @workgroup_size(256)
fn sand_porous(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let sand = sand_corner(at);
    // Packed past a full cell, the grains still fill only their packing.
    let solid = sand.solid / max(sand.fraction, 1.0);
    if !(sand.fraction > 0.0) || !(solid > 0.0) { return; }
    let sand_velocity = sand.velocity;
    var open = faces_rw[at];
    var moving = faces_out[at];
    var code = u32(moving.face_velocity.w);
    for (var a = 0; a < 3; a = a + 1) {
        if open.face_weight[a] < 1.0 { continue; }
        open.face_weight[a] = 1.0 - solid;
        moving.face_velocity[a] = sand_velocity[a];
        moving.face_weight[a] = 0.0;
        code = code & ~(0xffu << (8u * u32(a)));
    }
    moving.face_velocity.w = f32(code);
    faces_rw[at] = open;
    faces_out[at] = moving;
}
