// node.liquid_fill — BUFFER source body. One thread per particle slot
// (FluidParticle → Element). Cells are enumerated in lattice order: first
// the pool (every cell below `pool_cells`), then the box
// [column_x0, x1) × [max(column_y0, pool), y1) × [column_z0, z1). Each cell
// holds 8 particles, one per half-cell, jittered by up to a quarter cell by
// a hash of (seed, slot). Particles start at rest; the radius is that of a
// sphere of an eighth of the cell's volume; id = slot + 1. Slots past the
// fill are unused (all zero).

fn liquid_fill_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn liquid_fill_unit(x: u32) -> f32 {
    return f32(liquid_fill_hash(x) >> 8u) / 16777216.0;
}

fn body(
    idx: u32,
    count: u32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    pool_cells: i32,
    column_x0: i32,
    column_x1: i32,
    column_y0: i32,
    column_y1: i32,
    column_z0: i32,
    column_z1: i32,
    seed: i32,
    max_capacity: i32,
) -> Element {
    var out = Element(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    let n = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let pool = min(u32(max(pool_cells, 0)), n.y);
    let x0 = min(u32(max(column_x0, 0)), n.x);
    let x1 = min(u32(max(column_x1, 0)), n.x);
    let y0 = max(min(u32(max(column_y0, 0)), n.y), pool);
    let y1 = min(u32(max(column_y1, 0)), n.y);
    let z0 = min(u32(max(column_z0, 0)), n.z);
    let z1 = min(u32(max(column_z1, 0)), n.z);
    let ex = select(0u, x1 - x0, x1 > x0);
    let ey = select(0u, y1 - y0, y1 > y0);
    let ez = select(0u, z1 - z0, z1 > z0);
    let pool_count = n.x * pool * n.z;
    let cell_index = idx / 8u;
    if cell_index >= pool_count + ex * ey * ez {
        return out;
    }
    var c: vec3<u32>;
    if cell_index < pool_count {
        c = vec3<u32>(cell_index % n.x, (cell_index / n.x) % pool, cell_index / (n.x * pool));
    } else {
        let e = cell_index - pool_count;
        c = vec3<u32>(x0 + e % ex, y0 + (e / ex) % ey, z0 + e / (ex * ey));
    }
    let sub = idx % 8u;
    let half = vec3<f32>(f32(sub & 1u), f32((sub >> 1u) & 1u), f32((sub >> 2u) & 1u));
    let key = idx * 3u + u32(seed) * 2654435761u;
    let jitter = vec3<f32>(liquid_fill_unit(key), liquid_fill_unit(key + 1u), liquid_fill_unit(key + 2u)) - vec3<f32>(0.5);
    let local = vec3<f32>(0.25) + 0.5 * half + 0.25 * jitter;
    let lo = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell.
    let radius = 0.31017 * cell_size;
    out.position_radius = vec4<f32>(lo + (vec3<f32>(c) + local) * cell_size, radius);
    out.id = idx + 1u;
    return out;
}
