// node.preserve_foam — fusable BUFFER body, COINCIDENT pool, GATHER
// cell_ranges, order and binned. FLIP's _updateFoamPreservation
// (diffuseparticlesimulation.cpp:2123): each foam particle, dead or alive,
// gains rate·clamp((n − min)/max(max − min, 1e-6), 0, 1)·dt of lifetime, n
// the foam in its cell, dead foam included.
//
// The cell is the sort's bin. The bin is worked out again here, and fast-math
// rounding can land a particle on a bin face one bin off the sort's, so the
// body looks for its own index in that bin's members and, failing that, in
// the 26 around it: the count is always the bin the sort put it in. A foam
// slot the sort did not bin (a position that is not finite) gains nothing.
//
// ABI: `pool` and `binned` share the WhitewaterParticle struct (Element),
// `cell_ranges` is CellRange (Element2), `order` u32.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

const PF_FOAM: u32 = 1u;

// Foam among bin b's members, and whether `idx` is one of them.
fn pf_scan(b: u32, idx: u32) -> vec2<u32> {
    var foam = 0u;
    var found = 0u;
    if b >= arrayLength(&buf_cell_ranges) {
        return vec2<u32>(0u, 0u);
    }
    let range = buf_cell_ranges[b];
    let end = min(range.start + range.count, arrayLength(&buf_order));
    for (var s = range.start; s < end; s = s + 1u) {
        let member = buf_order[s];
        if member == idx {
            found = 1u;
        }
        if member < arrayLength(&buf_binned) && buf_binned[member].kind == PF_FOAM {
            foam = foam + 1u;
        }
    }
    return vec2<u32>(foam, found);
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    enabled: f32,
    dt: f32,
    rate: f32,
    min_density: f32,
    max_density: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
) -> Element {
    var out = e_pool;
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if enabled <= 0.5 || e_pool.kind != PF_FOAM || any(bins < vec3<i32>(1)) || !(cell_size > 0.0) {
        return out;
    }
    let bin_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * vec3<f32>(size_x, size_y, size_z);
    let raw = vec3<i32>(floor((e_pool.position_lifetime.xyz - bin_min) * (1.0 / cell_size)));
    let home = clamp(raw, vec3<i32>(0), bins - vec3<i32>(1));
    var hit = vec2<u32>(0u, 0u);
    for (var n = 0u; n < 27u && hit.y == 0u; n = n + 1u) {
        // The home bin first, then its neighbours.
        let k = (n + 13u) % 27u;
        let b = home + vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1);
        if any(b < vec3<i32>(0)) || any(b >= bins) {
            continue;
        }
        hit = pf_scan(u32(b.x) + u32(bins.x) * (u32(b.y) + u32(bins.y) * u32(b.z)), idx);
    }
    if hit.y == 0u {
        return out;
    }
    let d = clamp((f32(hit.x) - min_density) * (1.0 / max(max_density - min_density, 1e-6)), 0.0, 1.0);
    out.position_lifetime.w = e_pool.position_lifetime.w + rate * d * dt;
    return out;
}
