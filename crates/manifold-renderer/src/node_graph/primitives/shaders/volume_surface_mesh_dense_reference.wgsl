// node.volume_surface_mesh — fusable BUFFER body, GATHER. One thread per
// output vertex (GPU_FLUID_SURFACE_DESIGN.md D16): binary-search the running
// total for the cell that owns triangle idx / 3, look the edge up in the
// marching-cubes table, interpolate the crossing and the gradient normal.
// Past the live total the vertex is zero (the zeroed-tail contract); a total
// past capacity makes the whole mesh empty (the node reports it next frame).
//
// ABI: `levelset` (f32) and `scan` (u32, inclusive running total of per-cell
// triangle counts) are gathered; the output MeshVertex is Element. Attributes
// match the CPU fluid mesh: uv from the authored domain's x/z, white colour.

fn vsm_phi(p: vec3<u32>, nodes: vec3<u32>) -> f32 {
    return buf_levelset[mc_node(p, nodes)];
}

// Central-difference gradient (one-sided at the border), in level-set units
// per metre. Points outward: the level set grows outside.
fn vsm_gradient(p: vec3<u32>, nodes: vec3<u32>, spacing: vec3<f32>) -> vec3<f32> {
    let last = nodes - vec3<u32>(1u);
    let lo = vec3<u32>(select(p - vec3<u32>(1u), p, p == vec3<u32>(0u)));
    let hi = min(p + vec3<u32>(1u), last);
    return vec3<f32>(
        (vsm_phi(vec3<u32>(hi.x, p.y, p.z), nodes) - vsm_phi(vec3<u32>(lo.x, p.y, p.z), nodes)) / (f32(hi.x - lo.x) * spacing.x),
        (vsm_phi(vec3<u32>(p.x, hi.y, p.z), nodes) - vsm_phi(vec3<u32>(p.x, lo.y, p.z), nodes)) / (f32(hi.y - lo.y) * spacing.y),
        (vsm_phi(vec3<u32>(p.x, p.y, hi.z), nodes) - vsm_phi(vec3<u32>(p.x, p.y, lo.z), nodes)) / (f32(hi.z - lo.z) * spacing.z),
    );
}

fn body(
    idx: u32,
    count: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    resolution_scale: i32,
    max_capacity: i32,
) -> Element {
    let zero = Element(vec3<f32>(0.0), vec3<f32>(0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    // Fewer than two nodes on an axis: no lattice yet, an empty mesh.
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
    let cell_index = lo;
    var first = 0u;
    if cell_index > 0u {
        first = buf_scan[cell_index - 1u];
    }
    let cell = vec3<u32>(cell_index % cells.x, (cell_index / cells.x) % cells.y, cell_index / (cells.x * cells.y));
    let edge = mc_edge(mc_case(cell, nodes), (triangle - first) * 3u + idx % 3u);
    // Interpolate every lattice edge from its lower-indexed node, so the two
    // cells that share it produce bit-identical vertices (no hairline gaps).
    var a = cell + MC_CORNERS[MC_EDGE_A[edge]];
    var b = cell + MC_CORNERS[MC_EDGE_B[edge]];
    if mc_node(b, nodes) < mc_node(a, nodes) {
        let swap = a;
        a = b;
        b = swap;
    }
    let phi_a = vsm_phi(a, nodes);
    let phi_b = vsm_phi(b, nodes);
    let mu = clamp(phi_a / (phi_a - phi_b), 0.0, 1.0);

    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let spacing = size / vec3<f32>(cells);
    let position = lattice_min + mix(vec3<f32>(a), vec3<f32>(b), mu) * spacing;
    var normal = mix(vsm_gradient(a, nodes, spacing), vsm_gradient(b, nodes, spacing), mu);
    let length_squared = dot(normal, normal);
    if length_squared > 0.0 {
        normal = normal * inverseSqrt(length_squared);
    } else {
        normal = vec3<f32>(0.0, 1.0, 0.0);
    }
    // The authored domain sits 1.5 simulation cells inside the padded lattice.
    let cell_size = spacing * f32(max(resolution_scale, 1));
    let domain_min = lattice_min + 1.5 * cell_size;
    let domain_size = size - 3.0 * cell_size;
    let uv = (position.xz - domain_min.xz) / domain_size.xz;
    return Element(position, normal, uv, vec2<f32>(0.0), vec4<f32>(0.0), vec4<f32>(1.0));
}
