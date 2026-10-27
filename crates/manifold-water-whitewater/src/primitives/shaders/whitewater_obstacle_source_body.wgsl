// Nearest-object metadata, using liquid_solid_distance's posed SDF sampling.
// The domain is the Wall faces of the Bounds box (bounds_lo..bounds_hi in
// metres as placed, closed_faces the Wall bits −X +X −Y +Y −Z +Z) with
// liquid_solid_distance's wall distance: the least of x − lo and hi − x over
// the Wall faces, 1e30 with none, minus the length of the per-axis overshoots
// past a Wall.
// LiquidBody = Element; LiquidShape = Element2; WhitewaterSource = Element3.

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn body(
    idx: u32,
    count: u32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    closed_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    influence: f32, dust_strength: f32,
) -> Element3 {
    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let x = lattice_min + vec3<f32>(coord) * cell_size;
    let lo = vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z);
    let hi = vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z);
    var distance = 1.0e30;
    var overshoot = vec3<f32>(0.0);
    let faces = u32(closed_faces);
    for (var d = 0u; d < 3u; d = d + 1u) {
        if (faces & (1u << (2u * d))) != 0u {
            let wall = x[d] - lo[d];
            distance = min(distance, wall);
            overshoot[d] = min(overshoot[d], wall);
        }
        if (faces & (1u << (2u * d + 1u))) != 0u {
            let wall = hi[d] - x[d];
            distance = min(distance, wall);
            overshoot[d] = min(overshoot[d], wall);
        }
    }
    if any(overshoot < vec3<f32>(0.0)) {
        distance = -length(overshoot);
    }
    var kind = select(0u, 1u, faces != 0u);
    let first = max(rows - body_count, 0);
    for (var b = 0; b < body_count; b = b + 1) {
        let row = first + b;
        if row >= rows {
            break;
        }
        let bd = buf_bodies[u32(row)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        // The pose after tick_seconds, as node.matter_move_bodies moves it.
        let position = bd.position_inv_mass.xyz + bd.linear_velocity.xyz * tick_seconds;
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, tick_seconds);
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
        let candidate = liquid_shape_distance(sh.atlas_offset, dims, g, sh.origin_spacing.w, sh.scale_min.xyz);
        if candidate < distance { distance = candidate; kind = 2u; }
    }
    return Element3(influence, select(dust_strength, 1.0, kind == 1u), kind, 0u);
}
