// node.smooth_lattice — fusable BUFFER body, GATHER. One thread per lattice
// node: `passes` rounds of the [1, 2, 1] / 4 filter along one axis, as one
// (2·passes + 1)-tap binomial gather with edge-clamped indices. Chained over
// x, y and z it equals the full 3D binomial blur, because the weights are a
// product of per-axis rows and the clamp is per axis. Nodes past the lattice,
// and every node while there is no lattice, pass through. `levelset` (f32) is
// gathered through `buf_levelset`.

// Binomial rows C(2p, k) / 4^p for p = 1, 2, 3, packed at offsets 0, 3, 8.
fn smooth_weight(passes: i32, d: i32) -> f32 {
    var rows = array<f32, 15>(
        0.25, 0.5, 0.25,
        0.0625, 0.25, 0.375, 0.25, 0.0625,
        0.015625, 0.09375, 0.234375, 0.3125, 0.234375, 0.09375, 0.015625,
    );
    let offset = select(select(8, 3, passes == 2), 0, passes == 1);
    return rows[offset + passes + d];
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, passes: f32, axis: i32) -> f32 {
    let value = buf_levelset[idx];
    let p = i32(clamp(round(passes), 0.0, 3.0));
    if p == 0 || min(min(nodes_x, nodes_y), nodes_z) < 2.0 {
        return value;
    }
    let nodes = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    if idx >= u32(nodes.x) * u32(nodes.y) * u32(nodes.z) {
        return value;
    }
    let node = vec3<i32>(
        i32(idx % u32(nodes.x)),
        i32((idx / u32(nodes.x)) % u32(nodes.y)),
        i32(idx / (u32(nodes.x) * u32(nodes.y))),
    );
    let a = clamp(axis, 0, 2);
    var sum = 0.0;
    for (var d = -p; d <= p; d = d + 1) {
        var q = node;
        q[a] = clamp(node[a] + d, 0, nodes[a] - 1);
        sum = sum + smooth_weight(p, d) * buf_levelset[u32(q.x + nodes.x * (q.y + nodes.y * q.z))];
    }
    return sum;
}
