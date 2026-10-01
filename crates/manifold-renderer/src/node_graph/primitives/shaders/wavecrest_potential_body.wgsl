// node.wavecrest_potential — fusable BUFFER body, COINCIDENT particles,
// GATHER distance, curvature and cells. FLIP's wavecrest potential for a
// surface particle (diffuseparticlesimulation.cpp:1571, :1718), with the
// distance and curvature read trilinearly at cell centres (a cell outside
// the grid reads 0):
//   0 unless the particle sits within 1.5 cells of the surface and its cell
//   borders air (26 neighbours; outside the grid counts as solid);
//   0 when it is still (every velocity component under 1e-6);
//   k = curvature × cell size, 0 below min_curvature, held at max_curvature;
//   0 when the distance's gradient vanishes or the velocity's direction
//   meets the surface normal below `sharpness` (a dot product);
//   else (k − min_curvature)/(max_curvature − min_curvature).
// 0 for a slot with radius 0, or a grid past any gathered array.

fn wc_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn wc_curvature(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_curvature[ww_cell_index(vec3<u32>(c), cells)].value;
}

fn wc_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
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
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_curvature > min_curvature) {
        return 0.0;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0.0;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_curvature) || total > arrayLength(&buf_cells) {
        return 0.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c, cells);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
        k = k + w * wc_curvature(c, cells);
    }
    if !(abs(d) < 1.5 * h) || !wc_borders_air(vec3<i32>(floor(q)), cells) {
        return 0.0;
    }
    let v = e_particles.velocity;
    if all(abs(v) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    k = k * h;
    if k < min_curvature {
        return 0.0;
    }
    k = min(k, max_curvature);
    // FLIP's trilinear gradient (interpolation.cpp:197), unscaled: corner
    // index = x + 2y + 4z.
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    let grad = vec3<f32>(gx, gy, gz);
    if all(abs(grad) < vec3<f32>(1e-6)) {
        return 0.0;
    }
    if dot(normalize(v), normalize(grad)) < sharpness {
        return 0.0;
    }
    return (k - min_curvature) / (max_curvature - min_curvature);
}
