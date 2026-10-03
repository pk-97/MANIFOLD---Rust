// Test-only pre-brick dense reference, main 4aab34f86.
// node.clamp_liquid_to_solids — fusable BUFFER body; `levelset` is read at
// the thread's own node, `solid` is gathered. One thread per level-set node:
// border nodes read band (1/3 bin, outside), nodes inside a solid read at
// least 0 (never liquid), and every other node passes through unchanged. The
// node position and the trilinear solid sample are node.particle_volume's, so
// on an unsmoothed level set this changes nothing. Nodes past the lattice,
// and every node while there is no lattice, pass through.

fn clamp_liquid_solid_at(p: vec3<f32>, lattice_min: vec3<f32>, spacing: vec3<f32>, nodes: vec3<u32>) -> f32 {
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
    e_levelset: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    solid_nodes_x: f32,
    solid_nodes_y: f32,
    solid_nodes_z: f32,
    cell_size: f32,
) -> f32 {
    let lattice = vec3<f32>(nodes_x, nodes_y, nodes_z);
    let solid_lattice = vec3<f32>(solid_nodes_x, solid_nodes_y, solid_nodes_z);
    if any(lattice < vec3<f32>(2.0)) || any(solid_lattice < vec3<f32>(2.0)) {
        return e_levelset;
    }
    let nodes = vec3<u32>(lattice);
    let solid_nodes = vec3<u32>(solid_lattice);
    if idx >= nodes.x * nodes.y * nodes.z || solid_nodes.x * solid_nodes.y * solid_nodes.z > arrayLength(&buf_solid) {
        return e_levelset;
    }
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    if any(ijk == vec3<u32>(0u)) || any(ijk == nodes - vec3<u32>(1u)) {
        return cell_size / 3.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = lattice_min + vec3<f32>(ijk) * size / vec3<f32>(nodes - vec3<u32>(1u));
    let spacing = size / vec3<f32>(solid_nodes - vec3<u32>(1u));
    if clamp_liquid_solid_at(p, lattice_min, spacing, solid_nodes) < 0.0 {
        return max(e_levelset, 0.0);
    }
    return e_levelset;
}
