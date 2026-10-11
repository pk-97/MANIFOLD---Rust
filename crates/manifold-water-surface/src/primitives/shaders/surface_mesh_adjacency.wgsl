// Incident triangles of a welded marching-cubes lattice edge.
// The cell corner at offset `o` (each component 0 or 1).
fn sm_adj_corner(o: vec3<u32>) -> u32 {
    for (var c = 0u; c < 8u; c = c + 1u) {
        if all(MC_CORNERS[c] == o) {
            return c;
        }
    }
    return 8u;
}

// The cell edge joining corners `p` and `q`, or 12 when none does.
fn sm_adj_cell_edge(p: u32, q: u32) -> u32 {
    for (var e = 0u; e < 12u; e = e + 1u) {
        if (MC_EDGE_A[e] == p && MC_EDGE_B[e] == q) || (MC_EDGE_A[e] == q && MC_EDGE_B[e] == p) {
            return e;
        }
    }
    return 12u;
}

struct RsmNeighbourSum {
    sum: vec3<f32>,
    met: u32,
    normal: vec3<f32>,
}

fn rsm_neighbour_sum(
    home: vec3<u32>,
    edge: u32,
    cells: vec3<u32>,
    nodes: vec3<u32>,
    indexed: u32,
    normals: bool,
) -> RsmNeighbourSum {
    var a = home + MC_CORNERS[MC_EDGE_A[edge]];
    var b = home + MC_CORNERS[MC_EDGE_B[edge]];
    if mc_node(b, nodes) < mc_node(a, nodes) {
        let swap = a;
        a = b;
        b = swap;
    }
    // The lattice edge runs from `a` one node up along `step`; the four cells
    // around it are visited in the exact order used by the dense oracle.
    let step = b - a;
    let side_u = select(vec3<u32>(1u, 0u, 0u), vec3<u32>(0u, 1u, 0u), step.x == 1u);
    let side_v = select(vec3<u32>(0u, 0u, 1u), vec3<u32>(0u, 1u, 0u), step.z == 1u);
    var sum = vec3<f32>(0.0);
    var met = 0u;
    var normal = vec3<f32>(0.0);
    for (var around = 0u; around < 4u; around = around + 1u) {
        let du = side_u * (around & 1u);
        let dv = side_v * (around >> 1u);
        if any(a < du + dv) {
            continue;
        }
        let cell = a - du - dv;
        if any(cell >= cells) {
            continue;
        }
        let cell_edge = sm_adj_cell_edge(sm_adj_corner(a - cell), sm_adj_corner(b - cell));
        let case_index = mc_case(cell, nodes);
        let cell_index = cell.x + cells.x * (cell.y + cells.y * cell.z);
        var base = 0u;
        if cell_index > 0u {
            base = buf_scan[cell_index - 1u];
        }
        let cell_triangles = MC_TRIANGLE_COUNT[case_index];
        for (var t = 0u; t < cell_triangles; t = t + 1u) {
            for (var corner = 0u; corner < 3u; corner = corner + 1u) {
                if mc_edge(case_index, t * 3u + corner) != cell_edge {
                    continue;
                }
                let slot = (base + t) * 3u;
                var next = slot + (corner + 1u) % 3u;
                var previous = slot + (corner + 2u) % 3u;
                if indexed != 0u {
                    next = se_vertex(cell, mc_edge(case_index, t * 3u + (corner + 1u) % 3u), nodes);
                    previous = se_vertex(cell, mc_edge(case_index, t * 3u + (corner + 2u) % 3u), nodes);
                }
                sum = sum + buf_vertices[next].position;
                sum = sum + buf_vertices[previous].position;
                met = met + 2u;
                if normals {
                    var centre = slot + corner;
                    if indexed != 0u { centre = se_vertex(cell, cell_edge, nodes); }
                    let p = buf_vertices[centre].position;
                    normal = normal + cross(buf_vertices[next].position - p, buf_vertices[previous].position - p);
                }
            }
        }
    }
    return RsmNeighbourSum(sum, met, normal);
}

