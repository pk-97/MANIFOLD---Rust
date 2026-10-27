// node.liquid_solid_distance — fusable BUFFER body, GATHER. One thread per lattice node, x fastest: the seam's solid
// lattice, positive in free space and negative inside a solid. It is the
// smaller of the distance to the Wall faces of the Bounds box and every
// enabled body's signed distance, sampled from its shape's lattice in the
// atlas through its pose at the end of this frame's last tick (the row
// rows − body_count moved for tick_seconds, as node.matter_move_bodies moves
// it), in metres by liquid_shape_distance. The Bounds box is bounds_lo..
// bounds_hi in metres as placed; closed_faces (bits −X +X −Y +Y −Z +Z) marks
// its Wall faces. Per Wall face the distance is x − lo (low face) or hi − x
// (high face), positive on the water side; the wall distance is the least of
// them, 1e30 with no Wall face, and past a Wall it is minus the length of the
// per-axis overshoots. Open faces are not solids. This is the formula of
// manifold_water_liquid::lattice::bounds_distance.
//
// ABI: `bodies` (LiquidBody), `shapes` (LiquidShape) and `atlas` (distances
// two halves per word) are gathered; the output is one f32 per node. Poses
// and sampling are liquid_pose.wgsl's and liquid_collider.wgsl's.

struct ClockPlan {
    step_dt: f32,
    elapsed: f32,
    remaining: f32,
    maximum_speed: f32,
    cap_hit: u32,
    nonfinite: u32,
    step_index: u32,
    event: u32,
    numerical_end: f32,
    marker_limit: f32,
    _pad0: u32,
    live_mode: u32,
};

@group(0) @binding(5) var<storage, read> clock_plan: array<ClockPlan>;

fn adaptive_tick_seconds(tick_seconds: f32) -> f32 {
    return select(tick_seconds, clock_plan[0].elapsed, clock_plan[0].live_mode != 0u);
}

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
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
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
) -> f32 {
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
        let pose_time = adaptive_tick_seconds(tick_seconds);
        let position = bd.position_inv_mass.xyz + bd.linear_velocity.xyz * pose_time;
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, pose_time);
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
        distance = min(distance, liquid_shape_distance(sh.atlas_offset, dims, g, sh.origin_spacing.w, sh.scale_min.xyz));
    }
    return distance;
}
