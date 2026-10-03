// node.count_surface_edges — fusable BUFFER body, GATHER. One thread per
// lattice node: count its positive-axis sign-crossing edges (x, y, z), with
// zero padding past the lattice's node count.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> u32 {
    if min(min(nodes_x, nodes_y), nodes_z) < 2.0 {
        return 0u;
    }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let node_total = nodes.x * nodes.y * nodes.z;
    if idx >= count || idx >= node_total {
        return 0u;
    }
    let p = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    return countOneBits(se_mask(p, nodes));
}
