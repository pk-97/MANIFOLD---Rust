fn dust_solid(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_solid[ww_cell_index(vec3<u32>(c), cells)];
}

fn wc_turbulence(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_turbulence[ww_cell_index(vec3<u32>(c), cells)];
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
    min_turbulence: f32,
    max_turbulence: f32,
    dust_enabled: f32, boundary_dust: f32,
) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0.0;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) || total > arrayLength(&buf_turbulence) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_source) {
        return 0.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    if dust_enabled < 0.5 || any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) { return 0.0; }
    let source = buf_source[ww_cell_index(vec3<u32>(q), nodes)];
    if source.kind == 0u || source.dust_strength <= 0.0 { return 0.0; }
    if source.kind == 1u && (boundary_dust < 0.5 || q.z > 3.0) { return 0.0; }
    let solid_base = vec3<i32>(floor(q));
    var clearance = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        clearance += ww_corner_weight(fract(q), corner) * dust_solid(solid_base + vec3<i32>(ww_corner(corner)), nodes);
    }
    if clearance < 0.0 || clearance > 2.5 * h { return 0.0; }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        let w = ww_corner_weight(f, corner);
        k = k + w * wc_turbulence(c, cells);
    }
    let minimum = 0.75 * min_turbulence;
    return source.dust_strength * (clamp(k, minimum, max_turbulence) - minimum) / (max_turbulence - minimum);
}
