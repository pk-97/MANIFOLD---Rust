// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.particle_volume — fusable BUFFER body, GATHER. One thread per level-set
// node: the distance to the nearest blob ellipsoid in the node's 27 bins,
// a·(|G·(x − c)| − 1) with a the blob's longest axis (exact for a sphere),
// negative inside, capped at band = 1/3 bin outside (GPU_FLUID_SURFACE_DESIGN.md
// D18, P6e; never an atomic splat). node.shape_particle_blobs keeps every blob
// within 2/3 bin of its particle, so a blob the search misses is at least band
// away and the cap is exact. Band = half the reach is the FLIP Fluids mesher's
// ratio (its field is exact out to 1.5 radii). A node inside a solid is capped at 0 — never
// inside the liquid, as upstream's scalar field caps solid vertices — and the
// lattice border is outside, so the surface closes (D15).
//
// ABI: `blobs` (FluidBlob → Element), `cell_ranges` (CellRange → Element2) and
// `solid` (f32) are gathered; the output is one f32 per node. The bin grid is
// the sort's (`bins_x/y/z`), never ceil(size / cell_size) again: fast-math
// division can land one bin past the ranges the sort wrote.

fn pv_solid(p: vec3<f32>, lattice_min: vec3<f32>, spacing: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let g = clamp((p - lattice_min) / spacing, vec3<f32>(0.0), vec3<f32>(nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_solid[at.x + nodes.x * (at.y + nodes.y * at.z)];
    }
    return value;
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
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    brick_pass: u32,
) -> f32 {
    let band = cell_size / 3.0;
    if brick_pass == 2u { return band; }
    let solid_nodes = max(vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)), vec3<u32>(2u));
    let scale = u32(clamp(resolution_scale, 1, 8));
    let nodes = (solid_nodes - vec3<u32>(1u)) * scale + vec3<u32>(1u);
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if idx >= nodes.x * nodes.y * nodes.z || any(bins < vec3<i32>(1)) {
        return band;
    }
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    if any(ijk == vec3<u32>(0u)) || any(ijk == nodes - vec3<u32>(1u)) {
        return band;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = lattice_min + vec3<f32>(ijk) * size / vec3<f32>(nodes - vec3<u32>(1u));

    var phi = band;
    let home = clamp(vec3<i32>(floor((p - lattice_min) / cell_size)), vec3<i32>(0), bins - vec3<i32>(1));
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let b = home + vec3<i32>(dx, dy, dz);
                if any(b < vec3<i32>(0)) || any(b >= bins) {
                    continue;
                }
                let range = buf_cell_ranges[u32(b.x + bins.x * (b.y + bins.y * b.z))];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let blob = buf_blobs[k];
                    let reach = blob.center_radius.w;
                    let d = p - blob.center_radius.xyz;
                    // Past reach + band the blob cannot go below the cap.
                    let limit = reach + band;
                    if !(reach > 0.0) || dot(d, d) >= limit * limit {
                        continue;
                    }
                    let diag = blob.shape_diag;
                    let off = blob.shape_off;
                    let v = vec3<f32>(
                        diag.x * d.x + off.x * d.y + off.y * d.z,
                        off.x * d.x + diag.y * d.y + off.z * d.z,
                        off.y * d.x + off.z * d.y + diag.z * d.z,
                    );
                    phi = min(phi, reach * (length(v) - 1.0));
                }
            }
        }
    }
    let spacing = size / vec3<f32>(solid_nodes - vec3<u32>(1u));
    if pv_solid(p, lattice_min, spacing, solid_nodes) < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}

fn liquid_brick_index(invocation: u32) -> u32 {
    let dims = (max(vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z)), vec3<u32>(2u)) - vec3<u32>(1u)) * u32(clamp(params.resolution_scale, 1, 8)) + vec3<u32>(1u);
    return liquid_brick_select(invocation, dims, params.brick_pass);
}
