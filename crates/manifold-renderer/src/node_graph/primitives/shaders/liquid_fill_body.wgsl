// node.liquid_fill — BUFFER source body. One thread per particle slot
// (FluidParticle → Element3). Particles sit on the half-cell site lattice,
// site j at (1/4 + j/2) cells along each axis (2·nodes sites per axis), the
// FLIP Fluids engine's seeding lattice. Sites are enumerated in lattice
// order: first the pool (every site below `pool_sites`), then the box
// [box_x0, x1) × [max(box_y0, pool), y1) × [box_z0, z1). Each particle is
// jittered by up to jitter / 4 cells each way by a hash of (seed, slot) and
// starts at rest; the radius is that of a sphere of an eighth of a cell;
// id = slot + 1. Slots past the fill, up to particle_capacity, start dead
// (all zero) for sources to emit into. The lattice
// params are the padded lattice (liquid::lattice, 3 nodes of padding a side):
// the authored box starts 3 cells in and has nodes − 7 cells per axis. A
// site inside an enabled body at its pose when the epoch starts (rows
// 0..body_count of `bodies`, sampled from its shape's lattice in the atlas)
// keeps its slot and id with radius 0, dead: the engine seeds a site only
// where the solid distance is positive (_addNewFluidCellsAABB). `bodies`
// (LiquidBody → Element), `shapes` (LiquidShape → Element2) and `atlas` are
// gathered.

fn liquid_fill_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn liquid_fill_unit(x: u32) -> f32 {
    return f32(liquid_fill_hash(x) >> 8u) / 16777216.0;
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// Whether `x` lies in or on an enabled body at its tick-start pose.
fn liquid_fill_in_solid(x: vec3<f32>, body_count: i32) -> bool {
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if liquid_lattice_holds(g, dims) && liquid_lattice_distance(sh.atlas_offset, dims, g) <= 0.0 {
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
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    pool_sites: i32,
    box_x0: i32,
    box_x1: i32,
    box_y0: i32,
    box_y1: i32,
    box_z0: i32,
    box_z1: i32,
    jitter: f32,
    seed: i32,
    body_count: i32,
    epoch: i32,
    particle_capacity: i32,
) -> Element3 {
    var out = Element3(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    let n = 2u * vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z) - vec3<f32>(7.0), vec3<f32>(0.0)));
    let pool = min(u32(max(pool_sites, 0)), n.y);
    let x0 = min(u32(max(box_x0, 0)), n.x);
    let x1 = min(u32(max(box_x1, 0)), n.x);
    let y0 = max(min(u32(max(box_y0, 0)), n.y), pool);
    let y1 = min(u32(max(box_y1, 0)), n.y);
    let z0 = min(u32(max(box_z0, 0)), n.z);
    let z1 = min(u32(max(box_z1, 0)), n.z);
    let ex = select(0u, x1 - x0, x1 > x0);
    let ey = select(0u, y1 - y0, y1 > y0);
    let ez = select(0u, z1 - z0, z1 > z0);
    let pool_count = n.x * pool * n.z;
    if idx >= pool_count + ex * ey * ez {
        return out;
    }
    var s: vec3<u32>;
    if idx < pool_count {
        s = vec3<u32>(idx % n.x, (idx / n.x) % pool, idx / (n.x * pool));
    } else {
        let e = idx - pool_count;
        s = vec3<u32>(x0 + e % ex, y0 + (e / ex) % ey, z0 + e / (ex * ey));
    }
    let key = idx * 3u + u32(seed) * 2654435761u;
    let unit = vec3<f32>(liquid_fill_unit(key), liquid_fill_unit(key + 1u), liquid_fill_unit(key + 2u));
    let local = vec3<f32>(0.25) + 0.5 * vec3<f32>(s) + 0.5 * jitter * (unit - vec3<f32>(0.5));
    let lo = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z) + vec3<f32>(3.0 * cell_size);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell.
    let x = lo + local * cell_size;
    let radius = select(0.31017 * cell_size, 0.0, liquid_fill_in_solid(x, body_count));
    out.position_radius = vec4<f32>(x, radius);
    out.id = idx + 1u;
    return out;
}
