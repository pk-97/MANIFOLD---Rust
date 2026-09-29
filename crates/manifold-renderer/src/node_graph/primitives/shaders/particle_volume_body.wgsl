// node.particle_volume — fusable BUFFER body, GATHER. One thread per level-set
// node: threshold − Σ (1 − |G·(x − c)|²)³ over the blobs in the node's 27 bins
// (negative inside; GPU_FLUID_SURFACE_DESIGN.md D18, never an atomic splat).
// The lattice is the solid lattice refined by resolution_scale over the same
// box. A node inside a solid is capped at 0 — never inside the liquid, as
// upstream's scalar field caps solid vertices at the threshold — and the
// lattice border is empty, so the surface closes (D15).
//
// ABI: `blobs` (FluidBlob → Element), `cell_ranges` (CellRange → Element2) and
// `solid` (f32) are gathered; the output is one f32 per node.

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
    threshold: f32,
) -> f32 {
    let solid_nodes = max(vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)), vec3<u32>(2u));
    let scale = u32(clamp(resolution_scale, 1, 8));
    let nodes = (solid_nodes - vec3<u32>(1u)) * scale + vec3<u32>(1u);
    if idx >= nodes.x * nodes.y * nodes.z {
        return threshold;
    }
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    if any(ijk == vec3<u32>(0u)) || any(ijk == nodes - vec3<u32>(1u)) {
        return threshold;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = lattice_min + vec3<f32>(ijk) * size / vec3<f32>(nodes - vec3<u32>(1u));

    var sum = 0.0;
    let bins = max(vec3<i32>(1), vec3<i32>(ceil(size / cell_size)));
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
                    let d = p - blob.center_radius.xyz;
                    let reach = blob.center_radius.w;
                    if !(reach > 0.0) || dot(d, d) >= reach * reach {
                        continue;
                    }
                    let diag = blob.shape_diag;
                    let off = blob.shape_off;
                    let v = vec3<f32>(
                        diag.x * d.x + off.x * d.y + off.y * d.z,
                        off.x * d.x + diag.y * d.y + off.z * d.z,
                        off.y * d.x + off.z * d.y + diag.z * d.z,
                    );
                    let q2 = dot(v, v);
                    if q2 < 1.0 {
                        let falloff = 1.0 - q2;
                        sum = sum + falloff * falloff * falloff;
                    }
                }
            }
        }
    }
    var phi = threshold - sum;
    let spacing = size / vec3<f32>(solid_nodes - vec3<u32>(1u));
    if pv_solid(p, lattice_min, spacing, solid_nodes) < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}
