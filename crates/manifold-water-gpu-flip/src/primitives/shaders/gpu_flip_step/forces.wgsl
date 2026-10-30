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

// Fixed-point units of one whole cell of sand in `sand_share`, and of one
// whole cell of sand moving at 1 m/s in its momentum words (a 16th of the
// share's, so tens of grains at tens of m/s stay inside an i32).
const SAND_FIXED: f32 = 65536.0;
const SAND_MOMENTUM_FIXED: f32 = 4096.0;

// One thread a sand particle, after the step's table is built: its rest
// volume (the sphere of its radius) over a cell's, and that volume times its
// velocity, shared trilinearly among the eight corners round it. A grain whose own tile the water does not hold
// is far from any water (the table holds rings of tiles round it) and adds
// nothing, so a pile away from the water costs one lookup a grain.
@compute @workgroup_size(256)
fn sand_corners(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || gid.x >= u.sand_count { return; }
    let point = sand_points[gid.x];
    let r = point.position_radius.w * u.inv_cell;
    if r <= 0.0 { return; }
    let g = (point.position_radius.xyz - u.origin) * u.inv_cell;
    let base = vec3<i32>(floor(g));
    let home = ts_cell_at(base);
    if home == ts_canon() { return; }
    ts_home(home);
    let f = g - floor(g);
    let share = SAND_FIXED * 4.18879 * r * r * r;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, k >> 2u);
        let idx = ts_cell_at(base + vec3<i32>(o));
        if idx == ts_canon() { continue; }
        let w = select(1.0 - f, f, o == vec3<u32>(1u));
        let weight = w.x * w.y * w.z;
        let corner = SAND_WORDS * idx;
        atomicAdd(&sand_share[corner], u32(share * weight + 0.5));
        // Signed momentum adds as two's complement in the u32 words.
        let momentum = round(point.velocity * (SAND_MOMENTUM_FIXED / SAND_FIXED) * share * weight);
        for (var axis = 0u; axis < 3u; axis = axis + 1u) {
            atomicAdd(&sand_share[corner + 1u + axis], bitcast<u32>(i32(momentum[axis])));
        }
    }
}

// 512 threads a held slot, after `face_gravity`: sand is a porous moving
// solid, so the water inside it is drawn toward the sand's mean velocity at
// the cell's low corner by its share there, which stands for the cell's
// three faces (a grain's share spreads over the cell round it). The words
// are taken, so the next step's `sand_corners` adds to zero.
@compute @workgroup_size(256)
fn sand_drag(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let corner = SAND_WORDS * list_cell(gid.x);
    let total = f32(atomicExchange(&sand_share[corner], 0u)) / SAND_FIXED;
    var momentum = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        momentum[axis] = f32(bitcast<i32>(atomicExchange(&sand_share[corner + 1u + axis], 0u)));
    }
    if total <= 0.0 { return; }
    let sand_velocity = momentum / (SAND_MOMENTUM_FIXED * total);
    let at = list_cell(gid.x);
    var here = faces_out[at];
    let keep = exp(-u.sand_drag * min(total, 1.0) * adaptive_step_dt());
    here.face_velocity = vec4<f32>(sand_velocity + (here.face_velocity.xyz - sand_velocity) * keep, here.face_velocity.w);
    faces_out[at] = here;
}
