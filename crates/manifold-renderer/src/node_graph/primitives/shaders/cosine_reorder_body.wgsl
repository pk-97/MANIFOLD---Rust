// node.cosine_reorder — fusable BUFFER body, GATHER. The index shuffle that
// turns a cosine transform into a plain FFT (Makhoul 1980), on every
// transformed axis of a lattice (node (i, j, k) at i + nx·(j + ny·k)): x, y
// and z with axes 3, x and y with axes 2 (z slices stay put). Forward:
// v[p] = x[2p] for p < n/2, v[n - 1 - p] = x[2p + 1]. Inverse undoes it.

fn cosine_reorder_source(p: u32, n: u32) -> u32 {
    return select(2u * (n - 1u - p) + 1u, 2u * p, p < n / 2u);
}

fn cosine_reorder_target(m: u32, n: u32) -> u32 {
    return select(n - 1u - (m - 1u) / 2u, m / 2u, (m & 1u) == 0u);
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, direction: i32, axes: i32) -> f32 {
    let n = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let c = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
    var s = vec3<u32>(
        cosine_reorder_source(c.x, n.x),
        cosine_reorder_source(c.y, n.y),
        cosine_reorder_source(c.z, n.z),
    );
    if direction != 0 {
        s = vec3<u32>(
            cosine_reorder_target(c.x, n.x),
            cosine_reorder_target(c.y, n.y),
            cosine_reorder_target(c.z, n.z),
        );
    }
    if axes == 2 {
        s.z = c.z;
    }
    return buf_values[s.x + n.x * (s.y + n.y * s.z)];
}
