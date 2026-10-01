// node.restrict_lattice — fusable BUFFER body, GATHER. One thread per coarse
// cell: in a coarse water cell, the full-weighting average of the fine
// lattice (twice as many cells per axis) around it, the transpose of
// node.prolong_lattice's trilinear weights over 8; 0 in air. `fine` is
// gathered through buf_fine; a fine lattice longer than it gives 0.

// The share fine cell f takes from coarse cell c along one axis in
// prolongation: 3/4 from its parent, 1/4 from the parent's neighbour on its
// side, clamped at the walls.
fn restrict_lattice_weight(f: i32, c: i32, coarse: i32) -> f32 {
    let parent = f / 2;
    let other = clamp(select(parent - 1, parent + 1, f % 2 == 1), 0, coarse - 1);
    return select(0.0, 0.75, parent == c) + select(0.0, 0.25, other == c);
}

fn body(idx: u32, count: u32, e_water: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = 2 * n;
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || 8u * cells > arrayLength(&buf_fine) || !(e_water > 0.5) {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    var sum = 0.0;
    for (var dz = -1; dz <= 2; dz = dz + 1) {
        let fz = 2 * p.z + dz;
        if fz < 0 || fz >= m.z {
            continue;
        }
        let wz = restrict_lattice_weight(fz, p.z, n.z);
        for (var dy = -1; dy <= 2; dy = dy + 1) {
            let fy = 2 * p.y + dy;
            if fy < 0 || fy >= m.y {
                continue;
            }
            let wy = wz * restrict_lattice_weight(fy, p.y, n.y);
            for (var dx = -1; dx <= 2; dx = dx + 1) {
                let fx = 2 * p.x + dx;
                if fx < 0 || fx >= m.x {
                    continue;
                }
                let w = wy * restrict_lattice_weight(fx, p.x, n.x);
                sum = sum + w * buf_fine[u32(fx + m.x * (fy + m.y * fz))];
            }
        }
    }
    return sum / 8.0;
}
