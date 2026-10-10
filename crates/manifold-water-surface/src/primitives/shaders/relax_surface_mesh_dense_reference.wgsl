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

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, strength: f32) -> Element {
    let zero = Element(vec3<f32>(0.0), vec3<f32>(0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    if min(min(nodes_x, nodes_y), nodes_z) < 2.0 {
        return zero;
    }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = nodes - vec3<u32>(1u);
    let cell_total = cells.x * cells.y * cells.z;
    let triangles = buf_scan[cell_total - 1u];
    if triangles > count / 3u || idx >= triangles * 3u {
        return zero;
    }
    let v = buf_vertices[idx];
    let unmoved = Element(v.position, v.normal, v.uv, v.uv1, v.tangent, v.color);
    if strength == 0.0 {
        return unmoved;
    }

    // This vertex's cell and lattice edge, as the mesh found them.
    let triangle = idx / 3u;
    var lo = 0u;
    var hi = cell_total - 1u;
    loop {
        if lo >= hi {
            break;
        }
        let mid = (lo + hi) / 2u;
        if buf_scan[mid] > triangle {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    var first = 0u;
    if lo > 0u {
        first = buf_scan[lo - 1u];
    }
    let home = vec3<u32>(lo % cells.x, (lo / cells.x) % cells.y, lo / (cells.x * cells.y));
    let edge = mc_edge(mc_case(home, nodes), (triangle - first) * 3u + idx % 3u);
    var a = home + MC_CORNERS[MC_EDGE_A[edge]];
    var b = home + MC_CORNERS[MC_EDGE_B[edge]];
    if mc_node(b, nodes) < mc_node(a, nodes) {
        let swap = a;
        a = b;
        b = swap;
    }
    // The lattice edge runs from `a` one node up along `step`; the four cells
    // around it sit at `a` minus 0 or 1 along each of the other two axes.
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
                sum = sum + buf_vertices[slot + (corner + 1u) % 3u].position;
                sum = sum + buf_vertices[slot + (corner + 2u) % 3u].position;
                met = met + 2u;
            }
        }
    }
    if met == 0u {
        return unmoved;
    }
    let mean = sum / f32(met);
    return Element(v.position + strength * (mean - v.position), v.normal, v.uv, v.uv1, v.tangent, v.color);
}
