// Shared marching-cubes edge identity and ownership helpers.
//
// A lattice edge is identified by its lower endpoint and its positive axis.
// The cell at the lattice boundary on either perpendicular axis is the one
// that owns it, so every geometric edge has exactly one writer.

// The three positive-axis crossing bits at lattice node p: x, y, z. A bit is
// absent at the positive boundary because there is no node beyond it.
fn se_mask(p: vec3<u32>, nodes: vec3<u32>) -> u32 {
    let here = buf_levelset[mc_node(p, nodes)] < 0.0;
    var mask = 0u;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        if p[axis] + 1u >= nodes[axis] {
            continue;
        }
        var next = p;
        next[axis] = next[axis] + 1u;
        let there = buf_levelset[mc_node(next, nodes)] < 0.0;
        if here != there {
            mask = mask | (1u << axis);
        }
    }
    return mask;
}

// Lower endpoint of a cell edge, in lattice-node coordinates.
fn se_lower(cell: vec3<u32>, edge: u32) -> vec3<u32> {
    let a = cell + MC_CORNERS[MC_EDGE_A[edge]];
    let b = cell + MC_CORNERS[MC_EDGE_B[edge]];
    return min(a, b);
}

// Positive axis of a marching-cubes edge: x=0, y=1, z=2.
fn se_axis(cell: vec3<u32>, edge: u32) -> u32 {
    if edge >= 8u {
        return 1u;
    }
    return select(2u, 0u, (edge & 1u) == 0u);
}

// Clamp the lower endpoint to the last valid cell on every axis. This picks
// the sole owner for an edge that lies on one or more positive boundaries.
fn se_owner(cell: vec3<u32>, edge: u32, nodes: vec3<u32>) -> bool {
    if any(nodes < vec3<u32>(2u)) {
        return false;
    }
    let last_cell = nodes - vec3<u32>(2u);
    return all(cell == min(se_lower(cell, edge), last_cell));
}
