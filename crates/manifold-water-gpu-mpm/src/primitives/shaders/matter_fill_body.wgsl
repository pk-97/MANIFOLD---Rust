// node.matter_fill — BUFFER source body. One thread per seeded point. Points are enumerated in cell order:
// first the pool (every authored cell below `pool_cells`, slot 0's), then
// each slot's box from the fill table, clipped above the pool; each cell
// holds 2³ or 3³ points, one per sub-cell, jittered inside it by a hash of
// (seed, slot, cell, sub-cell). id = birth ordinal. V0 = dx³ /
// points_per_cell, J = 1, C = 0, at rest, undeformed (F = I, no plastic
// history, no stress), its slot in affine_z.w. Cell coordinates are in
// the authored box, which starts at (fill_min_x, fill_min_y, fill_min_z) and
// holds fill_cells_x/y/z cells. A seed inside an enabled collider at its pose
// when the epoch starts (rows 0..body_count of `bodies`, sampled from its
// shape's lattice in the atlas) is left an unused slot, id 0, so ids stay
// sorted. A row whose region is in 0..region_rows is a shaped fill's: a box
// seed outside that row of `regions` (liquid_fill_shape.wgsl) is an unused
// slot too. `bodies` and `regions` (LiquidBody → Element), `shapes`
// (LiquidShape → Element2), `atlas` and the collider trees (Vec4Vertex →
// Element3) are gathered, and `fills` (u32, node.matter_domain
// `fill_table`: x0, x1, y0, y1, z0, z1, region, first point a row); the
// output is MatterPoint (Element4).

const MF_SLOTS: u32 = 8u;
const MF_ROW: u32 = 8u;
fn matter_fill_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn matter_fill_unit(x: u32) -> f32 {
    return f32(matter_fill_hash(x) >> 8u) / 16777216.0;
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn cb_node(i: u32) -> vec4<f32> {
    let n = buf_collider_nodes[i];
    return vec4<f32>(n.x, n.y, n.z, n.w);
}

fn cb_triangle(i: u32) -> vec4<f32> {
    let t = buf_collider_triangles[i];
    return vec4<f32>(t.x, t.y, t.z, t.w);
}

// Whether `x` lies inside an enabled body at its tick-start pose.
fn mf_inside_collider(x: vec3<f32>, body_count: i32, cell_size: f32) -> bool {
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        if sh.tree == 1u {
            let local = mt_local(x, bd.position_inv_mass.xyz, bd.rotation);
            if mt_phi(sh.tree_node, sh.tree_triangle, local, cell_size) < 0.0 {
                return true;
            }
            continue;
        }
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if liquid_lattice_holds(g, dims) && liquid_lattice_distance(sh.atlas_offset, dims, g) < 0.0 {
            return true;
        }
    }
    return false;
}

fn body(
    idx: u32,
    count: u32,
    fill_min_x: f32,
    fill_min_y: f32,
    fill_min_z: f32,
    cell_size: f32,
    fill_cells_x: i32,
    fill_cells_y: i32,
    fill_cells_z: i32,
    pool_cells: i32,
    points_per_cell: i32,
    seed: i32,
    body_count: i32,
    epoch: i32,
    region_rows: i32,
) -> Element4 {
    let ppc = u32(points_per_cell);
    var per_axis = 2u;
    if ppc == 27u {
        per_axis = 3u;
    }
    let cells = vec3<u32>(u32(fill_cells_x), u32(fill_cells_y), u32(fill_cells_z));
    let pool = u32(pool_cells);
    let cell_index = idx / ppc;
    let sub = idx % ppc;
    let pool_total = cells.x * pool * cells.z;
    var cell: vec3<u32>;
    var slot = 0u;
    var region = -1;
    if cell_index < pool_total {
        cell = vec3<u32>(cell_index % cells.x, (cell_index / cells.x) % pool, cell_index / (cells.x * pool));
    } else {
        // The row whose run holds idx: rows start in slot order, an empty
        // row's run is empty.
        for (var k = 0u; k < MF_SLOTS; k = k + 1u) {
            let r = k * MF_ROW;
            let n = (buf_fills[r + 1u] - buf_fills[r]) * (buf_fills[r + 3u] - buf_fills[r + 2u])
                * (buf_fills[r + 5u] - buf_fills[r + 4u]) * ppc;
            if idx >= buf_fills[r + 7u] && idx - buf_fills[r + 7u] < n {
                slot = k;
            }
        }
        let r = slot * MF_ROW;
        let c = (idx - buf_fills[r + 7u]) / ppc;
        let w = buf_fills[r + 1u] - buf_fills[r];
        let h = buf_fills[r + 3u] - buf_fills[r + 2u];
        cell = vec3<u32>(buf_fills[r] + c % w, buf_fills[r + 2u] + (c / w) % h, buf_fills[r + 4u] + c / (w * h));
        region = bitcast<i32>(buf_fills[r + 6u]);
    }
    let s = vec3<u32>(sub % per_axis, (sub / per_axis) % per_axis, sub / (per_axis * per_axis));
    let key = ((cell.z * cells.y + cell.y) * cells.x + cell.x) * ppc + sub;
    // Two fills over one cell seed apart.
    let h0 = matter_fill_hash(key ^ (u32(seed) * 2654435761u) ^ (slot * 0x9e3779b9u));
    let jitter = vec3<f32>(
        matter_fill_unit(h0),
        matter_fill_unit(h0 ^ 0x9e3779b9u),
        matter_fill_unit(h0 ^ 0x85ebca6bu),
    );
    let local = vec3<f32>(cell) + (vec3<f32>(s) + jitter) / f32(per_axis);
    var p: Element4;
    p.position = vec3<f32>(fill_min_x, fill_min_y, fill_min_z) + local * cell_size;
    let outside = region >= 0 && region < region_rows && !liquid_fill_in_shape(p.position, region, cell_size);
    p.id = select(idx + 1u, 0u, outside || mf_inside_collider(p.position, body_count, cell_size));
    p.velocity = vec3<f32>(0.0);
    p.volume_ratio = 1.0;
    p.affine_x = vec4<f32>(0.0);
    p.affine_y = vec4<f32>(0.0, 0.0, 0.0, cell_size * cell_size * cell_size / f32(ppc));
    p.affine_z = vec4<f32>(0.0, 0.0, 0.0, f32(slot));
    p.f_x = vec4<f32>(1.0, 0.0, 0.0, 0.0);
    p.f_y = vec4<f32>(0.0, 1.0, 0.0, 0.0);
    p.f_z = vec4<f32>(0.0, 0.0, 1.0, 0.0);
    p.stress_diag = vec4<f32>(0.0);
    p.stress_off = vec4<f32>(0.0);
    return p;
}
