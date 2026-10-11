// Normals of the final mesh, gathered through its shared lattice edge topology.
fn rsm_normal_vertex(idx: u32, neighbours: RsmNeighbourSum) -> Element {
    let v = buf_vertices[idx];
    let magnitude = length(neighbours.normal);
    var normal = vec3<f32>(0.0);
    if magnitude > 1.1920928955078125e-7 && magnitude <= 3.402823466e38 {
        normal = neighbours.normal / magnitude;
    }
    return Element(v.position, normal, v.uv, v.uv1, v.tangent, v.color);
}

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    strength: f32,
    max_capacity: u32,
    brick_pass: u32,
    indexed: u32,
) {
    let zero = Element(
        vec3<f32>(0.0),
        vec3<f32>(0.0),
        vec2<f32>(0.0),
        vec2<f32>(0.0),
        vec4<f32>(0.0),
        vec4<f32>(0.0),
    );
    if brick_pass == 2u {
        var live = 0u;
        if min(min(nodes_x, nodes_y), nodes_z) >= 2.0 {
            let clear_nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
            let clear_cells = clear_nodes - vec3<u32>(1u);
            let clear_total = clear_cells.x * clear_cells.y * clear_cells.z;
            let triangles = buf_scan[clear_total - 1u];
            if triangles <= max_capacity / 3u {
                live = triangles * 3u;
                if indexed != 0u {
                    live = buf_edge_scan[clear_nodes.x * clear_nodes.y * clear_nodes.z - 1u];
                }
            }
        }
        if idx >= live {
            buf_out[idx] = zero;
        }
        return;
    }
    if min(min(nodes_x, nodes_y), nodes_z) < 2.0 {
        return;
    }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = nodes - vec3<u32>(1u);
    let cell_total = cells.x * cells.y * cells.z;
    if idx >= count || idx >= cell_total {
        return;
    }
    let triangles = buf_scan[cell_total - 1u];
    if triangles > max_capacity / 3u {
        return;
    }
    let home = vec3<u32>(idx % cells.x, (idx / cells.x) % cells.y, idx / (cells.x * cells.y));
    let case_index = mc_case(home, nodes);
    let cell_triangles = MC_TRIANGLE_COUNT[case_index];
    var base = 0u;
    if idx > 0u {
        base = buf_scan[idx - 1u];
    }
    var edge_cache: array<RsmNeighbourSum, 12>;
    var edge_ready: array<bool, 12>;
    for (var e = 0u; e < 12u; e = e + 1u) {
        edge_ready[e] = false;
    }
    for (var t = 0u; t < cell_triangles; t = t + 1u) {
        for (var corner = 0u; corner < 3u; corner = corner + 1u) {
            let edge_entry = t * 3u + corner;
            let edge = mc_edge(case_index, edge_entry);
            if indexed != 0u && (!se_owner(home, edge, nodes) || edge_ready[edge]) {
                continue;
            }
            if !edge_ready[edge] {
                edge_cache[edge] = rsm_neighbour_sum(home, edge, cells, nodes, indexed, true);
                edge_ready[edge] = true;
            }
            var slot = (base + t) * 3u + corner;
            if indexed != 0u { slot = se_vertex(home, edge, nodes); }
            buf_out[slot] = rsm_normal_vertex(slot, edge_cache[edge]);
        }
    }
}
