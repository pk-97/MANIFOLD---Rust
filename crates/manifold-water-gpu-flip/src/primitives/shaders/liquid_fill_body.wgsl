// Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.liquid_fill — BUFFER source body. One thread per particle slot
// (FluidParticle → Element4). Particles sit on the half-cell site lattice,
// site j at (1/4 + j/2) cells along each axis (2·fill_cells sites per axis), the
// FLIP Fluids engine's seeding lattice. The domain puts the box on whole world
// cells (cell p centred on p·h), so site j is world half cell 2p + 1 + j, at
// a fixed world position whatever the box's corner. Sites are enumerated in
// lattice order: first the pool (every site below `pool_sites`), then the box
// [box_x0, x1) × [max(box_y0, pool), y1) × [box_z0, z1). Each particle is
// jittered by up to jitter / 4 cells each way by a hash of (seed, slot) and
// starts at rest; the radius is that of a sphere of an eighth of a cell;
// id = slot + 1. Slots past the fill, up to particle_capacity, start dead
// (all zero) for sources to emit into. The box starts at fill_min (metres)
// and has fill_cells cells per axis. A site the engine's solid holds when the epoch starts keeps its slot and id
// with radius 0, dead: the engine seeds a site only where the solid distance
// is positive (_addNewFluidCellsAABB). That distance is the Bounds box's Wall
// faces (bounds_lo..bounds_hi in metres as placed, closed_faces the Wall bits
// −X +X −Y +Y −Z +Z, as node.liquid_solid_distance computes them) and every
// enabled body at its pose (rows 0..body_count of `bodies`, sampled from its
// shape's lattice in the atlas), each node of a grid 1.5 cells outside the
// box the smaller of the two, read trilinearly as
// _solidSDF.trilinearInterpolate. With fill_region at or above 0 the box is a
// shaped fill's: a site outside region row fill_region of `regions` (its
// lattice in `shapes` and `atlas`, read as the step reads a region) is dead
// too. `bodies` and `regions` (LiquidBody → Element), `shapes` (LiquidShape →
// Element2), `atlas` and the collider trees (Vec4Vertex → Element3) are
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

fn cb_node(i: u32) -> vec4<f32> { let n = buf_collider_nodes[i]; return vec4<f32>(n.x, n.y, n.z, n.w); }
fn cb_triangle(i: u32) -> vec4<f32> { let t = buf_collider_triangles[i]; return vec4<f32>(t.x, t.y, t.z, t.w); }
var<private> lf_cell: f32;

// The Wall faces' distance at node `y`: the least of x − lo (low face) and
// hi − x (high face) over the Wall faces, 1e30 with none, and past a Wall
// minus the length of the per-axis overshoots (bounds::bounds_distance).
fn liquid_fill_wall_node(y: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, faces: u32) -> f32 {
    var nearest = 1.0e30;
    var overshoot = vec3<f32>(0.0);
    for (var a = 0u; a < 3u; a = a + 1u) {
        if (faces & (1u << (2u * a))) != 0u {
            let wall = y[a] - lo[a];
            nearest = min(nearest, wall);
            overshoot[a] = min(overshoot[a], wall);
        }
        if (faces & (1u << (2u * a + 1u))) != 0u {
            let wall = hi[a] - y[a];
            nearest = min(nearest, wall);
            overshoot[a] = min(overshoot[a], wall);
        }
    }
    return select(nearest, -length(overshoot), any(overshoot < vec3<f32>(0.0)));
}

// The engine's solid distance at node `y`: the walls' and every body's.
fn liquid_fill_solid_node(y: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, faces: u32, body_count: i32) -> f32 {
    var d = liquid_fill_wall_node(y, lo, hi, faces);
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        if sh.tree == 1u {
            let local = liquid_rotate(vec4<f32>(-bd.rotation.xyz, bd.rotation.w), y - bd.position_inv_mass.xyz);
            d = min(d, cb_phi(sh.tree_node, sh.tree_triangle, local, 4.0 * lf_cell, lf_cell));
            continue;
        }
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(y, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        d = min(d, liquid_shape_distance(sh.atlas_offset, dims, g, sh.origin_spacing.w, sh.scale_min.xyz));
    }
    return d;
}

// Whether shaped fill region `r` holds `x`: its lattice distance at or below
// 0, scaled to metres; a point the lattice does not cover is outside.
fn liquid_fill_in_shape(x: vec3<f32>, r: i32) -> bool {
    let bd = buf_regions[u32(r)];
    let shape_index = i32(bd.accel_shape.w);
    if shape_index < 0 {
        return false;
    }
    let sh = buf_shapes[u32(shape_index)];
    if sh.tree == 1u {
        let local = liquid_rotate(vec4<f32>(-bd.rotation.xyz, bd.rotation.w), x - bd.position_inv_mass.xyz);
        return cb_phi(sh.tree_node, sh.tree_triangle, local, 4.0 * lf_cell, lf_cell) <= 0.0;
    }
    let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
    let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
    return liquid_lattice_holds(g, dims) && liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w <= 0.0;
}

// Whether the engine's solid holds `x`: the solid distance read from the grid
// of nodes `h` apart that starts 1.5 cells outside the box at `box_lo`.
fn liquid_fill_in_native_solid(x: vec3<f32>, box_lo: vec3<f32>, h: f32, lo: vec3<f32>, hi: vec3<f32>, faces: u32, body_count: i32) -> bool {
    let grid_min = box_lo - vec3<f32>(1.5 * h);
    let g = (x - grid_min) / h;
    let i0 = floor(g);
    let f = g - i0;
    var d = 0.0;
    for (var c = 0u; c < 8u; c = c + 1u) {
        let o = vec3<f32>(f32(c & 1u), f32((c >> 1u) & 1u), f32((c >> 2u) & 1u));
        let w = mix(vec3<f32>(1.0) - f, f, o);
        d = d + w.x * w.y * w.z * liquid_fill_solid_node(grid_min + (i0 + o) * h, lo, hi, faces, body_count);
    }
    return d <= 0.0;
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
    closed_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    fill_region: i32,
) -> Element4 {
    lf_cell = cell_size;
    var out = Element4(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    let n = 2u * vec3<u32>(max(vec3<i32>(fill_cells_x, fill_cells_y, fill_cells_z), vec3<i32>(0)));
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
    let lo = vec3<f32>(fill_min_x, fill_min_y, fill_min_z);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell.
    let x = lo + local * cell_size;
    let solid = liquid_fill_in_native_solid(
        x, lo, cell_size,
        vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z),
        vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z),
        u32(closed_faces), body_count);
    let outside = idx >= pool_count && fill_region >= 0 && !liquid_fill_in_shape(x, fill_region);
    let radius = select(0.31017524 * cell_size, 0.0, solid || outside);
    out.position_radius = vec4<f32>(x, radius);
    out.id = idx + 1u;
    return out;
}
