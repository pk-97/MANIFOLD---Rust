// FLIP diffuseparticlesimulation.cpp:1699: scale eligible surface emitter velocity.

fn wc_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
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
    spray_speed: f32,
    seed: f32,
    epoch: f32,
) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return e_particles;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return e_particles;
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
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c, cells);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
    }
    var out = e_particles;
    if abs(d) < 1.5 * h && wc_borders_air(vec3<i32>(floor(q)), cells) && d > -0.75 * h {
        out.velocity *= 1.0 + (spray_speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 9u);
    }
    return out;
}
