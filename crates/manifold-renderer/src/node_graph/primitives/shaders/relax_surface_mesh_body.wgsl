// Uses the neighbour-mean mesh smoothing from FLIP Fluids trianglemesh.cpp `smooth` (MIT); see THIRD_PARTY_NOTICES.md.
// node.relax_surface_mesh — fusable BUFFER body, GATHER. One umbrella
// relaxation pass over node.volume_surface_mesh's triangle list:
// v += strength × (mean of v's neighbours − v). One thread per vertex slot.
//
// The list has no index buffer, but every vertex sits on one lattice edge, and
// the mesh places triangle t of cell c at slots 3 × (scan[c − 1] + t). So a
// vertex finds its neighbours by walking the four cells around its lattice
// edge and the triangles there that use the edge. Each neighbour of a closed
// fan is met twice, once per triangle on either side of the shared edge, so
// the plain mean over those meetings is the mean over distinct neighbours.
// Every copy of a vertex walks the same cells in the same order and reads the
// same copies, so the copies stay bit-identical: the mesh stays closed.
//
// Strength 0 copies the input. Past the live triangles, or when the mesh
// overflowed (`count` too small for the total), the vertex is zero, as the
// mesh writes it. Normals, uvs and colours pass through.
//
// ABI: `vertices` (MeshVertex), `levelset` (f32) and `scan` (u32, inclusive
// running total of per-cell triangle counts) are gathered; the output
// MeshVertex is Element. `levelset` and `scan` must be the ones the mesh was
// built from this frame.

// The cell corner at offset `o` (each component 0 or 1).
fn rsm_corner(o: vec3<u32>) -> u32 {
    for (var c = 0u; c < 8u; c = c + 1u) {
        if all(MC_CORNERS[c] == o) {
            return c;
        }
    }
    return 8u;
}

// The cell edge joining corners `p` and `q`, or 12 when none does.
fn rsm_cell_edge(p: u32, q: u32) -> u32 {
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
}

fn rsm_neighbour_sum(
    home: vec3<u32>,
    edge: u32,
    cells: vec3<u32>,
    nodes: vec3<u32>,
    indexed: u32,
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
        let cell_edge = rsm_cell_edge(rsm_corner(a - cell), rsm_corner(b - cell));
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
            }
        }
    }
    return RsmNeighbourSum(sum, met);
}

fn rsm_relax_vertex(idx: u32, strength: f32, neighbours: RsmNeighbourSum) -> Element {
    let v = buf_vertices[idx];
    let unmoved = Element(v.position, v.normal, v.uv, v.uv1, v.tangent, v.color);
    if strength == 0.0 || neighbours.met == 0u {
        return unmoved;
    }
    let mean = neighbours.sum / f32(neighbours.met);
    return Element(v.position + strength * (mean - v.position), v.normal, v.uv, v.uv1, v.tangent, v.color);
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
            buf_relaxed[idx] = zero;
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
                edge_cache[edge] = rsm_neighbour_sum(home, edge, cells, nodes, indexed);
                edge_ready[edge] = true;
            }
            var slot = (base + t) * 3u + corner;
            if indexed != 0u { slot = se_vertex(home, edge, nodes); }
            buf_relaxed[slot] = rsm_relax_vertex(slot, strength, edge_cache[edge]);
        }
    }
}
