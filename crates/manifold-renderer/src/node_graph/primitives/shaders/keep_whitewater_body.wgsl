// node.keep_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// binned, cell_ranges, order and solid. FLIP's _removeDiffuseParticles
// (diffuseparticlesimulation.cpp:2761) with every side colliding and every
// boundary closed: 1 for a slot the tick keeps, 0 for one it removes. A
// slot goes when it is empty (kind 3, the header too), its lifetime is at
// or below 0, its position is not finite, it lies outside FLIP's boundary
// box (1.625 cells and 0.5e-6 m in from the grid, lower faces inside) or
// inside the solid (the node lattice read trilinearly is below 0), or its
// cell already holds `cap` kept particles earlier in the pool.
//
// FLIP counts kept particles per cell in pool order, so slot i is kept by
// the cap exactly when fewer than `cap` slots before it share its cell and
// pass every other check. The cell is FLIP's, floor of the local position
// over the cell size, worked out the same way for every slot here. The
// sort's bins are only a search index: a fast-math bin may sit one off the
// cell, so the 27 bins around the cell hold every slot sharing it. When
// those bins hold no more than `cap` slots in all, nothing is counted.
//
// ABI: `pool` and `binned` share the WhitewaterParticle struct (Element),
// `cell_ranges` is CellRange (Element2), `order` u32, `solid` f32.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

const KW_BOX_INSET: f32 = 1.625;
const KW_BOX_EPSILON: f32 = 0.5e-6;

fn kw_node(n: vec3<i32>, nodes: vec3<u32>) -> f32 {
    if any(n < vec3<i32>(0)) || any(n >= vec3<i32>(nodes)) {
        return 0.0;
    }
    let u = vec3<u32>(n);
    return buf_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
}

fn kw_solid(q: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * kw_node(base + vec3<i32>(ww_corner(corner)), nodes);
    }
    return d;
}

// Every check but the cap; the cell in .xyz, 1 in .w when the slot passes.
fn kw_check(e: Element, origin: vec3<f32>, h: f32, lo: vec3<f32>, hi: vec3<f32>, nodes: vec3<u32>) -> vec4<i32> {
    let p = e.position_lifetime.xyz - origin;
    // By its bits: fast math may fold a NaN comparison away.
    let bits = vec3<u32>(bitcast<u32>(p.x), bitcast<u32>(p.y), bitcast<u32>(p.z)) & vec3<u32>(0x7f800000u);
    if e.kind > 2u || !(e.position_lifetime.w > 0.0) || any(bits == vec3<u32>(0x7f800000u)) {
        return vec4<i32>(0);
    }
    if !(all(p >= lo) && all(p < hi)) || kw_solid(p / h, nodes) < 0.0 {
        return vec4<i32>(0);
    }
    return vec4<i32>(vec3<i32>(floor(p / h)), 1);
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cap: f32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
) -> u32 {
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) {
        return 0u;
    }
    let cells = nodes - vec3<u32>(1u);
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let origin = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let lo = vec3<f32>(KW_BOX_INSET * h + KW_BOX_EPSILON);
    let hi = vec3<f32>(cells) * h - lo;
    let mine = kw_check(e_pool, origin, h, lo, hi, nodes);
    if mine.w == 0 {
        return 0u;
    }
    let limit = u32(max(round(cap), 0.0));
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if any(bins < vec3<i32>(1)) {
        return u32(limit > 0u);
    }
    let home = clamp(mine.xyz, vec3<i32>(0), bins - vec3<i32>(1));
    let n_ranges = arrayLength(&buf_cell_ranges);
    let n_order = arrayLength(&buf_order);
    var near = 0u;
    for (var k = 0u; k < 27u; k = k + 1u) {
        let b = home + vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1);
        if any(b < vec3<i32>(0)) || any(b >= bins) {
            continue;
        }
        let flat = u32(b.x) + u32(bins.x) * (u32(b.y) + u32(bins.y) * u32(b.z));
        if flat < n_ranges {
            near = near + buf_cell_ranges[flat].count;
        }
    }
    if near <= limit {
        return 1u;
    }
    var rank = 0u;
    for (var k = 0u; k < 27u; k = k + 1u) {
        let b = home + vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1);
        if any(b < vec3<i32>(0)) || any(b >= bins) {
            continue;
        }
        let flat = u32(b.x) + u32(bins.x) * (u32(b.y) + u32(bins.y) * u32(b.z));
        if flat >= n_ranges {
            continue;
        }
        let range = buf_cell_ranges[flat];
        let end = min(range.start + range.count, n_order);
        for (var s = range.start; s < end; s = s + 1u) {
            let member = buf_order[s];
            if member >= idx || member >= arrayLength(&buf_binned) {
                continue;
            }
            let other = kw_check(buf_binned[member], origin, h, lo, hi, nodes);
            if other.w == 1 && all(other.xyz == mine.xyz) {
                rank = rank + 1u;
            }
        }
    }
    return u32(rank < limit);
}
