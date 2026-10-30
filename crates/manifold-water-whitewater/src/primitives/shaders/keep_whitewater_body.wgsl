var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.keep_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// binned, cell_ranges, order, solid and the world-tile table. FLIP's
// _removeDiffuseParticles (diffuseparticlesimulation.cpp:2761) with every
// side colliding: 1 for a slot the tick keeps, 0 for one it removes. A slot
// goes when it is empty (kind 3), its lifetime is at or below 0, its position
// is not finite, it lies inside the solid (the node array read trilinearly is
// below 0; a tile the table does not hold reads the canonical element) or an
// enabled outflow mesh, it has left the scene where the liquid's move removes
// water (beyond the plane of an Open face of the Bounds, or below the kill
// height), or its cell already holds `cap` kept particles earlier in the
// pool. A wall is a solid.
//
// FLIP counts kept particles per cell in pool order, so slot i is kept by
// the cap exactly when fewer than `cap` slots before it share its cell and
// pass every other check. The cell is the world cell of the position, worked
// out the same way for every slot here. The sort's bins are only a search
// index (one range per element of a pooled field, a cell in a tile the table
// does not hold in the canonical range, which no cell reads): a fast-math bin
// may sit one off the cell, so the 27 bins around the cell hold every slot
// sharing it. When those bins hold no more than `cap` slots in all, nothing is
// counted.
//
// ABI: `pool` and `binned` share the WhitewaterParticle struct (Element),
// `cell_ranges` is CellRange (Element2), `order` u32, `solid` f32.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// The Open faces (bit 2d the low face of axis d), the Bounds corners and the
// kill height of this dispatch, set by `body`.
var<private> kw_open: u32;
var<private> kw_lo: vec3<f32>;
var<private> kw_hi: vec3<f32>;
var<private> kw_kill: f32;

// Beyond the plane of an Open face, on the side it opens to, or below the
// kill height: the liquid move's removal rule.
fn kw_left(p: vec3<f32>) -> bool {
    for (var a = 0u; a < 3u; a = a + 1u) {
        if (kw_open & (1u << (2u * a))) != 0u && p[a] < kw_lo[a] {
            return true;
        }
        if (kw_open & (1u << (2u * a + 1u))) != 0u && p[a] > kw_hi[a] {
            return true;
        }
    }
    return p.y < kw_kill;
}

fn kw_node(n: vec3<i32>) -> f32 {
    return buf_solid[ts_cell_at(n)];
}

fn kw_solid(q: vec3<f32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * kw_node(base + vec3<i32>(ww_corner(corner)));
    }
    return d;
}

// Same posed mesh distance and strict boundary as GPU FLIP region_holding.
// Ported from FLIP Fluids fluidsimulation.cpp::_updateOutflowMeshFluidSource.
fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}
fn cb_node(i: u32) -> vec4<f32> { let n = buf_collider_nodes[i]; return vec4<f32>(n.x, n.y, n.z, n.w); }
fn cb_triangle(i: u32) -> vec4<f32> { let t = buf_collider_triangles[i]; return vec4<f32>(t.x, t.y, t.z, t.w); }
var<private> kw_cell: f32;
fn kw_drained(position: vec3<f32>, region_count: f32, region_offset: f32, tick_seconds: f32) -> bool {
    for (var r = 0u; r < u32(region_count); r = r + 1u) {
        let row = u32(region_offset) + r;
        if row >= arrayLength(&buf_regions) { break; }
        let bd = buf_regions[row];
        if bd.angular_velocity.w != 3.0 || bd.accel_shape.w < 0.0 { continue; }
        let shape = u32(bd.accel_shape.w);
        if shape >= arrayLength(&buf_shapes) { continue; }
        let sh = buf_shapes[shape];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let p = fma(bd.linear_velocity.xyz, vec3<f32>(tick_seconds), bd.position_inv_mass.xyz);
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, tick_seconds);
        if sh.tree == 1u {
            let local = liquid_rotate(vec4<f32>(-q.xyz, q.w), position - p);
            if cb_phi(sh.tree_node, sh.tree_triangle, local, 4.0 * kw_cell, kw_cell) < 0.0 { return true; }
            continue;
        }
        let g = liquid_lattice_coord(position, p, q, sh.origin_spacing, sh.scale_min.xyz);
        if liquid_lattice_holds(g, dims) && liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w < 0.0 { return true; }
    }
    return false;
}

// Every check but the cap; the world cell in .xyz, 1 in .w when the slot passes.
fn kw_check(e: Element, h: f32, region_count: f32, region_offset: f32, tick_seconds: f32) -> vec4<i32> {
    let p = e.position_lifetime.xyz;
    // By its bits: fast math may fold a NaN comparison away.
    let bits = vec3<u32>(bitcast<u32>(p.x), bitcast<u32>(p.y), bitcast<u32>(p.z)) & vec3<u32>(0x7f800000u);
    if (e.kind == 3u || e.kind > 4u) || !(e.position_lifetime.w > 0.0) || any(bits == vec3<u32>(0x7f800000u)) {
        return vec4<i32>(0);
    }
    if kw_solid(ww_cell_position(p, h)) < 0.0 {
        return vec4<i32>(0);
    }
    if kw_drained(p, region_count, region_offset, tick_seconds) || kw_left(p) { return vec4<i32>(0); }
    return vec4<i32>(ts_world_cell(p, vec3<f32>(-0.5 * h), 1.0 / h), 1);
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    cap: f32,
    region_count: f32, region_offset: f32, tick_seconds: f32,
    cell_size: f32,
    pool_tiles: f32,
    open_faces: i32,
    bounds_lo_x: f32, bounds_lo_y: f32, bounds_lo_z: f32,
    bounds_hi_x: f32, bounds_hi_y: f32, bounds_hi_z: f32,
    kill_height: f32,
) -> u32 {
    ww_pool_tiles = u32(pool_tiles);
    kw_open = u32(open_faces);
    kw_lo = vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z);
    kw_hi = vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z);
    kw_kill = kill_height;
    kw_cell = cell_size;
    let h = cell_size;
    let mine = kw_check(e_pool, h, region_count, region_offset, tick_seconds);
    if mine.w == 0 {
        return 0u;
    }
    let limit = u32(max(round(cap), 0.0));
    ts_home(ts_cell_at(mine.xyz));
    let n_order = arrayLength(&buf_order);
    var near = 0u;
    for (var k = 0u; k < 27u; k = k + 1u) {
        let bin = ts_cell_at(mine.xyz + vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1));
        if bin == ts_canon() {
            continue;
        }
        near = near + buf_cell_ranges[bin].count;
    }
    if near <= limit {
        return 1u;
    }
    var rank = 0u;
    for (var k = 0u; k < 27u; k = k + 1u) {
        let bin = ts_cell_at(mine.xyz + vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1));
        if bin == ts_canon() {
            continue;
        }
        let range = buf_cell_ranges[bin];
        let end = min(range.start + range.count, n_order);
        for (var s = range.start; s < end; s = s + 1u) {
            let member = buf_order[s];
            if member >= idx || member >= arrayLength(&buf_binned) {
                continue;
            }
            let other = kw_check(buf_binned[member], h, region_count, region_offset, tick_seconds);
            if other.w == 1 && all(other.xyz == mine.xyz) {
                rank = rank + 1u;
            }
        }
    }
    return u32(rank < limit);
}
