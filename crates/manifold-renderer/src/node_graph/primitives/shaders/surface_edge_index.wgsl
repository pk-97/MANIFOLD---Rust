// Compact U/V/W edge indices from the inclusive per-node crossing count.
fn se_vertex(cell: vec3<u32>, edge: u32, nodes: vec3<u32>) -> u32 {
    let p = se_lower(cell, edge);
    let node = mc_node(p, nodes);
    var base = 0u;
    if node > 0u { base = buf_edge_scan[node - 1u]; }
    return base + countOneBits(se_mask(p, nodes) & ((1u << se_axis(cell, edge)) - 1u));
}
