// Ported from FLIP Fluids fluidsimulation.cpp and meshlevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.solid_face_velocity — fusable BUFFER body, GATHER. One thread per
// padded cell of the face grid (node.particles_to_faces' layout). On an inner
// face a solid cuts (open fraction under 1 in `solid_faces`): velocity is
// the normal part of the closest body's rigid velocity at the face centre,
// the face velocity FLIP Fluids' MeshLevelSet keeps for its solids; weight is
// the friction of the closest body at each of the face's four corners,
// averaged (FluidSimulation::_getFaceFrictionU/V/W), 0 at a corner no body's
// lattice holds. Every other face is zero. Bodies are posed after
// tick_seconds as node.liquid_solid_distance poses them, so velocity and
// open fraction see the same solid. A dynamic body (1/m > 0) moves at its
// tick-start velocity, plus its predicted external acceleration over
// tick_seconds, plus its row of `changes` (node.face_impulse_to_bodies' sums:
// the liquid's push so far this tick). Velocity w names each face's body
// (solid_body_faces.wgsl).
//
// ABI: `bodies` (LiquidBody), `shapes` (LiquidShape), `atlas` and `changes`
// are gathered; `solid_faces` is read coincident. A row past `bodies` or a
// shape past `shapes` is never read (the extent walk proves the rows fit); a
// body past `changes` takes no change.

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// The row of the body nearest x (smallest signed distance), −1 when no
// enabled body's lattice holds x.
fn solid_face_velocity_closest(x: vec3<f32>, body_count: i32, rows: i32, tick_seconds: f32) -> i32 {
    var best = -1;
    var nearest = 0.0;
    let first = max(rows - body_count, 0);
    for (var b = 0; b < body_count; b = b + 1) {
        let row = first + b;
        if row >= rows || u32(row) >= arrayLength(&buf_bodies) {
            break;
        }
        let bd = buf_bodies[u32(row)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 || u32(shape_index) >= arrayLength(&buf_shapes) {
            continue;
        }
        let position = fma(bd.linear_velocity.xyz, vec3<f32>(tick_seconds), bd.position_inv_mass.xyz);
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, tick_seconds);
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
        if !liquid_lattice_holds(g, dims) {
            continue;
        }
        let d = liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w;
        if best < 0 || d < nearest {
            best = row;
            nearest = d;
        }
    }
    return best;
}

fn body(
    idx: u32,
    count: u32,
    e_solid_faces: Element,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    body_count: f32,
    rows: f32,
    tick_seconds: f32,
) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let bodies = i32(body_count);
    let row_count = i32(rows);
    let first = max(row_count - bodies, 0);
    var code = 0u;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) || p[a] == 0 || p[a] == n[a] || !(e_solid_faces.face_weight[a] < 1.0) {
            continue;
        }
        var centre = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(cell_size), lattice_min);
        centre[a] = fma(f32(p[a]), cell_size, lattice_min[a]);
        let row = solid_face_velocity_closest(centre, bodies, row_count, tick_seconds);
        if row >= 0 {
            let bd = buf_bodies[u32(row)];
            let position = fma(bd.linear_velocity.xyz, vec3<f32>(tick_seconds), bd.position_inv_mass.xyz);
            var linear = bd.linear_velocity.xyz;
            var angular = bd.angular_velocity.xyz;
            let b = u32(row - first);
            if bd.position_inv_mass.w > 0.0 {
                linear = fma(bd.accel_shape.xyz, vec3<f32>(tick_seconds), linear);
                angular = fma(vec3<f32>(bd.inv_inertia_x.w, bd.inv_inertia_y.w, bd.inv_inertia_z.w), vec3<f32>(tick_seconds), angular);
                if 16u * (b + 1u) <= arrayLength(&buf_changes) {
                    linear = linear + vec3<f32>(buf_changes[16u * b + 8u], buf_changes[16u * b + 9u], buf_changes[16u * b + 10u]);
                    angular = angular + vec3<f32>(buf_changes[16u * b + 12u], buf_changes[16u * b + 13u], buf_changes[16u * b + 14u]);
                }
            }
            out.face_velocity[a] = liquid_body_velocity(linear, angular, position, centre)[a];
            code = code + (b + 1u) * (1u << (8u * u32(a)));
        }
        // The engine's corners: U (j, k), V (k, i), W (j, i) offsets.
        var b = 1;
        var c = 2;
        if a == 1 {
            b = 2;
            c = 0;
        } else if a == 2 {
            c = 0;
        }
        var friction = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            var q = p;
            q[b] = q[b] + (k & 1);
            q[c] = q[c] + ((k >> 1u) & 1);
            let at = solid_face_velocity_closest(lattice_min + vec3<f32>(q) * cell_size, bodies, row_count, tick_seconds);
            if at >= 0 {
                friction = friction + buf_bodies[u32(at)].linear_velocity.w;
            }
        }
        out.face_weight[a] = 0.25 * friction;
    }
    out.face_velocity.w = f32(code);
    return out;
}
