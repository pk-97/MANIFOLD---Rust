// node.liquid_solid_distance — fusable BUFFER body, GATHER (GPU_MPM_SOLVER_DESIGN.md
// D11, section 3.2). One thread per lattice node, x fastest: the seam's solid
// lattice, positive in free space and negative inside a solid. It is the
// smaller of the distance to the nearest closed wall, wall_inset nodes in
// from the lattice edge (3 on a padded lattice, as node.matter_frame's wall
// lattice; 0 on node.gpu_flip_step's cell lattice, whose walls are its
// edge), and every enabled body's signed distance, sampled from its
// shape's lattice in the atlas through its pose at the end of this frame's
// last tick (the row rows − body_count moved for tick_seconds, as
// node.matter_move_bodies moves it) and scaled by the shape's smallest scale.
//
// ABI: `bodies` (LiquidBody), `shapes` (LiquidShape) and `atlas` (distances
// two halves per word) are gathered; the output is one f32 per node. Poses
// and sampling are liquid_pose.wgsl's and liquid_collider.wgsl's.

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
    wall_inset: i32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
) -> f32 {
    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let x = lattice_min + vec3<f32>(coord) * cell_size;
    let inset = u32(max(wall_inset, 0));
    let low = lattice_min + vec3<f32>(f32(inset) * cell_size);
    let high = low + vec3<f32>(n - vec3<u32>(1u + 2u * inset)) * cell_size;
    var distance = length(vec3<f32>(n) * cell_size);
    let faces = u32(closed_faces);
    for (var d = 0u; d < 3u; d = d + 1u) {
        if (faces & (1u << (2u * d))) != 0u {
            distance = min(distance, x[d] - low[d]);
        }
        if (faces & (1u << (2u * d + 1u))) != 0u {
            distance = min(distance, high[d] - x[d]);
        }
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
        let position = bd.position_inv_mass.xyz + bd.linear_velocity.xyz * tick_seconds;
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, tick_seconds);
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
        if !liquid_lattice_holds(g, dims) {
            continue;
        }
        distance = min(distance, liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w);
    }
    return distance;
}
