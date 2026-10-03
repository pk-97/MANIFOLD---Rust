// node.push_out_of_solid — fusable BUFFER body, COINCIDENT particles,
// GATHER signed-distance lattice. The lattice is a box in scene metres;
// negative values are inside the solid.

fn po_node(at: vec3<i32>, nodes: vec3<u32>) -> f32 {
    if any(at < vec3<i32>(0)) || any(at >= vec3<i32>(nodes)) {
        return 0.0;
    }
    let u = vec3<u32>(at);
    return buf_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
}

fn po_sample(q_in: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let top = vec3<f32>(nodes - vec3<u32>(1u));
    let q = clamp(q_in, vec3<f32>(0.0), top);
    let base = min(vec3<i32>(floor(q)), vec3<i32>(nodes - vec3<u32>(2u)));
    let f = q - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let offset = vec3<i32>(i32(corner & 1u), i32((corner >> 1u) & 1u), i32((corner >> 2u) & 1u));
        let weight = select(1.0 - f.x, f.x, offset.x == 1)
            * select(1.0 - f.y, f.y, offset.y == 1)
            * select(1.0 - f.z, f.z, offset.z == 1);
        value = value + weight * po_node(base + offset, nodes);
    }
    return value;
}

fn po_gradient(q: vec3<f32>, nodes: vec3<u32>, spacing: vec3<f32>) -> vec3<f32> {
    // Half a lattice cell on either side gives a central difference while
    // retaining the same trilinear value at arbitrary particle positions.
    let half = vec3<f32>(0.5);
    let dx = po_sample(q + vec3<f32>(half.x, 0.0, 0.0), nodes)
        - po_sample(q - vec3<f32>(half.x, 0.0, 0.0), nodes);
    let dy = po_sample(q + vec3<f32>(0.0, half.y, 0.0), nodes)
        - po_sample(q - vec3<f32>(0.0, half.y, 0.0), nodes);
    let dz = po_sample(q + vec3<f32>(0.0, 0.0, half.z), nodes)
        - po_sample(q - vec3<f32>(0.0, 0.0, half.z), nodes);
    return vec3<f32>(dx / max(spacing.x, 1.0e-6), dy / max(spacing.y, 1.0e-6), dz / max(spacing.z, 1.0e-6));
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
) -> Element {
    var out = e_particles;
    if !(e_particles.position_radius.w > 0.0) {
        return out;
    }
    // Match the standalone runtime's scalar policy exactly: live wires may
    // carry fractional values, but lattice dimensions are rounded and have a
    // minimum of two nodes before sizing and bounds checks.
    let raw_nodes = vec3<f32>(nodes_x, nodes_y, nodes_z);
    // Avoid adding 0.5 to an already integral large f32, which can round up.
    let lattice = floor(raw_nodes) + select(vec3<f32>(0.0), vec3<f32>(1.0), fract(raw_nodes) >= vec3<f32>(0.5));
    if !all(lattice >= vec3<f32>(2.0)) || !all(lattice <= vec3<f32>(16777216.0)) {
        return out;
    }
    let nodes = vec3<u32>(lattice);
    // Divide the available storage so a large authored lattice cannot wrap
    // a u32 product and accidentally pass the bounds check.
    if nodes.z > arrayLength(&buf_solid) / nodes.x / nodes.y {
        return out;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let center = vec3<f32>(center_x, center_y, center_z);
    if !all(size > vec3<f32>(0.0)) || !all(size <= vec3<f32>(3.402823e+38))
        || !all(abs(center) <= vec3<f32>(3.402823e+38)) {
        return out;
    }
    let spacing = size / vec3<f32>(nodes - vec3<u32>(1u));
    let origin = center - 0.5 * size;
    var position = e_particles.position_radius.xyz;
    let top = vec3<f32>(nodes - vec3<u32>(1u));
    // Four fixed normalized-gradient steps cover curvature and trilinear
    // interpolation error while keeping this a barrier-free per-particle
    // body with deterministic work.
    for (var iteration = 0u; iteration < 4u; iteration = iteration + 1u) {
        let q = (position - origin) / spacing;
        if any(q < vec3<f32>(0.0)) || any(q > top) {
            break;
        }
        let phi = po_sample(q, nodes);
        if !(phi < 0.0) {
            break;
        }
        let gradient = po_gradient(q, nodes, spacing);
        let gradient_length = length(gradient);
        // A flat negative field has no defined outward direction. Keep it
        // stable rather than producing NaNs or an arbitrary axis push.
        if !(gradient_length > 1.0e-6)
            || !(gradient_length < 3.402823e+38)
            || !(gradient_length == gradient_length)
        {
            break;
        }
        position = position - phi * gradient / gradient_length;
    }
    out.position_radius = vec4<f32>(position, e_particles.position_radius.w);
    return out;
}
