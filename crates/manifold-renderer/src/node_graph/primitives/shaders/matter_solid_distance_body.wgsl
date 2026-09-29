// node.matter_solid_distance — fusable BUFFER body, GATHER (GPU_MPM_SOLVER_DESIGN.md
// D11, section 3.2). One thread per lattice node, x fastest: the seam's solid
// lattice, positive in free space and negative inside a solid. It is the
// smaller of the distance to the nearest closed wall (as node.matter_frame's
// wall lattice) and every enabled body's signed distance, sampled from its
// shape's lattice in the atlas through its pose at the end of this frame's
// last tick (the row rows − body_count moved for tick_seconds, as
// node.matter_move_bodies moves it) and scaled by the shape's smallest scale.
//
// ABI: `bodies` (MatterBody), `shapes` (MatterShape) and `atlas` (distances
// two halves per word) are gathered; the output is one f32 per node.

fn sd_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

fn sd_atlas(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn sd_lattice(offset: u32, dims: vec3<u32>, g: vec3<f32>) -> f32 {
    let c = clamp(g, vec3<f32>(0.0), vec3<f32>(dims - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(c)), dims - vec3<u32>(2u));
    let f = c - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * sd_atlas(offset + at.x + dims.x * (at.y + dims.y * at.z));
    }
    return value;
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
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    max_capacity: i32,
) -> f32 {
    let n = vec3<u32>(u32(nodes_x), u32(nodes_y), u32(nodes_z));
    let coord = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let x = lattice_min + vec3<f32>(coord) * cell_size;
    // Walls: the authored faces sit three nodes in from the lattice edge.
    let low = lattice_min + vec3<f32>(3.0 * cell_size);
    let high = low + vec3<f32>(n - vec3<u32>(7u)) * cell_size;
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
        var q = bd.rotation;
        let w = bd.angular_velocity.xyz;
        let speed = length(w);
        let angle = speed * tick_seconds;
        if angle > 0.0 {
            let dq = vec4<f32>(w * (sin(0.5 * angle) / speed), cos(0.5 * angle));
            q = vec4<f32>(
                dq.w * q.x + dq.x * q.w + dq.y * q.z - dq.z * q.y,
                dq.w * q.y - dq.x * q.z + dq.y * q.w + dq.z * q.x,
                dq.w * q.z + dq.x * q.y - dq.y * q.x + dq.z * q.w,
                dq.w * q.w - dq.x * q.x - dq.y * q.y - dq.z * q.z,
            );
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let local = sd_rotate(vec4<f32>(-q.xyz, q.w), x - position) / sh.scale_min.xyz;
        let g = (local - sh.origin_spacing.xyz) / sh.origin_spacing.w;
        if any(g < vec3<f32>(0.0)) || any(g > vec3<f32>(dims - vec3<u32>(1u))) {
            continue;
        }
        distance = min(distance, sd_lattice(sh.atlas_offset, dims, g) * sh.scale_min.w);
    }
    return distance;
}
