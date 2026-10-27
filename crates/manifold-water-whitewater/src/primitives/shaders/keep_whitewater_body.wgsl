var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.keep_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// binned, cell_ranges, order, solid and the world-tile table. FLIP's
// _removeDiffuseParticles (diffuseparticlesimulation.cpp:2761) with every
// side colliding: 1 for a slot the tick keeps, 0 for one it removes. A slot
// goes when it is empty (kind 3), its lifetime is at or below 0, its position
// is not finite, it lies inside the solid (the node array read trilinearly is
// below 0; a tile the table does not hold reads the canonical element) or an
// enabled outflow mesh, or its cell already holds `cap` kept particles
// earlier in the pool. There is no boundary box: a wall is a solid.
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
    if kw_drained(p, region_count, region_offset, tick_seconds) { return vec4<i32>(0); }
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
) -> u32 {
    ww_pool_tiles = u32(pool_tiles);
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
