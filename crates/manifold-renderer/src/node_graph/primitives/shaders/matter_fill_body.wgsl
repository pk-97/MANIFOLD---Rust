// node.matter_fill — BUFFER source body (GPU_MPM_SOLVER_DESIGN.md section 4.2,
// D9). One thread per seeded point. Points are enumerated in lattice order:
// first the pool (every authored cell below `pool_cells`), then the column box
// clipped above the pool; each cell holds 2³ or 3³ points, one per sub-cell,
// jittered inside it by a hash of (seed, cell, sub-cell). id = birth ordinal.
// V0 = dx³ / points_per_cell, J = 1, C = 0, at rest. Cell coordinates are in
// the authored box, which starts PADDING = 3 nodes inside the lattice. A
// seed inside an enabled collider at its pose when the epoch starts (rows
// 0..body_count of `bodies`, sampled from its shape's lattice in the atlas)
// is left an unused slot, id 0, so ids stay sorted. `bodies` (MatterBody →
// Element), `shapes` (MatterShape → Element2) and `atlas` are gathered; the
// output is MatterPoint (Element3).
fn matter_fill_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn matter_fill_unit(x: u32) -> f32 {
    return f32(matter_fill_hash(x) >> 8u) / 16777216.0;
}

fn matter_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// Whether `x` lies inside an enabled body at its tick-start pose.
fn mf_inside_collider(x: vec3<f32>, body_count: i32) -> bool {
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = matter_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if matter_lattice_holds(g, dims) && matter_lattice_distance(sh.atlas_offset, dims, g) < 0.0 {
            return true;
        }
    }
    return false;
}

fn body(
    idx: u32,
    count: u32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    pool_cells: i32,
    column_x0: i32,
    column_x1: i32,
    column_y0: i32,
    column_y1: i32,
    column_z0: i32,
    column_z1: i32,
    points_per_cell: i32,
    seed: i32,
    body_count: i32,
    epoch: i32,
) -> Element3 {
    let ppc = u32(points_per_cell);
    var per_axis = 2u;
    if ppc == 27u {
        per_axis = 3u;
    }
    let cells = vec3<u32>(u32(nodes_x) - 7u, u32(nodes_y) - 7u, u32(nodes_z) - 7u);
    let pool = u32(pool_cells);
    let cell_index = idx / ppc;
    let sub = idx % ppc;
    let pool_total = cells.x * pool * cells.z;
    var cell: vec3<u32>;
    if cell_index < pool_total {
        cell = vec3<u32>(cell_index % cells.x, (cell_index / cells.x) % pool, cell_index / (cells.x * pool));
    } else {
        let c = cell_index - pool_total;
        let y0 = max(u32(column_y0), pool);
        let w = u32(column_x1 - column_x0);
        let h = u32(column_y1) - y0;
        cell = vec3<u32>(u32(column_x0) + c % w, y0 + (c / w) % h, u32(column_z0) + c / (w * h));
    }
    let s = vec3<u32>(sub % per_axis, (sub / per_axis) % per_axis, sub / (per_axis * per_axis));
    let key = ((cell.z * cells.y + cell.y) * cells.x + cell.x) * ppc + sub;
    let h0 = matter_fill_hash(key ^ (u32(seed) * 2654435761u));
    let jitter = vec3<f32>(
        matter_fill_unit(h0),
        matter_fill_unit(h0 ^ 0x9e3779b9u),
        matter_fill_unit(h0 ^ 0x85ebca6bu),
    );
    let local = (vec3<f32>(cell) + vec3<f32>(3.0) + (vec3<f32>(s) + jitter) / f32(per_axis));
    var p: Element3;
    p.position = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z) + local * cell_size;
    p.id = select(idx + 1u, 0u, mf_inside_collider(p.position, body_count));
    p.velocity = vec3<f32>(0.0);
    p.volume_ratio = 1.0;
    p.affine_x = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    p.affine_y = vec4<f32>(0.0, 0.0, 0.0, cell_size * cell_size * cell_size / f32(ppc));
    p.affine_z = vec4<f32>(0.0);
    return p;
}
