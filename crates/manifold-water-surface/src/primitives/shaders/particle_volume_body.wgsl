var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.particle_volume — fusable BUFFER body, GATHER. One thread per pool
// element of the display's world tiles (`buf_tiles`, `pool_tiles` tiles).
// Element p is display node p, at (p - 1/2) cells, the low corner of cell p.
// Negative-inside port of FLIP Fluids ParticleMesher: the value starts at
// 3 * blob_reach (Particle Scale times the largest particle radius) and takes the nearest kernel sphere's |x - c| - r over every
// blob whose inclusive floor(c - 1.5r)..floor(c + 1.5r)+1 grid box holds the
// node. The blobs are sorted into the display cells (`buf_cell_ranges`, one
// range per pool element), so a node reads only the cells whose blobs can
// pass that test: those from floor(node - 1 - u) to floor(node + 1 + u), u
// being 1.5 * blob_reach in cells. The reach bounds the largest blob radius;
// it sets the search, never a quality limit. A node inside a solid (`buf_solid`
// is a node array on the same tiles) is never inside the liquid. A free slot
// and the canonical element hold the far value, FIELD_FAR_CELLS (3) cells.

// The most cells a search window spans on an axis. The host refuses a reach of
// more than 16 cells of support, whose window spans at most 36.
const PV_MAX_SPAN: i32 = 40;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const PV_FAR_CELLS: f32 = 3.0;

fn body(idx: u32, count: u32, cell_size: f32, particle_scale: f32, sim_cell_size: f32, particle_radius: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    let blob_reach = particle_scale * particle_radius * sim_cell_size;
    let h = cell_size;
    if idx >= ts_canon() {
        return PV_FAR_CELLS * h;
    }
    if ts_slot_entry(idx >> 9u) == WT_NONE {
        return PV_FAR_CELLS * h;
    }
    ts_home(idx);
    let node = ts_cell_coord(idx);
    let p = (vec3<f32>(node) - vec3<f32>(0.5)) * h;

    // The slack covers rounding in the sort's cell product and in the support
    // test below; it only widens the search, the tests stay exact.
    let u = 1.5 * blob_reach / h;
    let q = vec3<f32>(node);
    let slack = vec3<f32>(1.0e-6) * (abs(q) + vec3<f32>(u + 2.0));
    let lo = vec3<i32>(floor(q - vec3<f32>(u + 1.0) - slack));
    let hi = vec3<i32>(floor(q + vec3<f32>(u + 1.0) + slack));
    let span = clamp(hi - lo + vec3<i32>(1), vec3<i32>(0), vec3<i32>(PV_MAX_SPAN));
    let canon = ts_canon();

    var phi = 3.0 * blob_reach;
    for (var dz = 0; dz < span.z; dz = dz + 1) {
        for (var dy = 0; dy < span.y; dy = dy + 1) {
            for (var dx = 0; dx < span.x; dx = dx + 1) {
                let e = ts_cell_at(lo + vec3<i32>(dx, dy, dz));
                if e >= canon {
                    continue;
                }
                let range = buf_cell_ranges[e];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let blob = buf_blobs[k];
                    let reach = blob.w;
                    // Native support is a grid-aligned box including its outer
                    // interpolation node. Spherical rejection loses the corners.
                    let support = 1.5 * reach;
                    let first = vec3<i32>(floor((blob.xyz - vec3<f32>(support)) / h + vec3<f32>(0.5)));
                    let last = vec3<i32>(floor((blob.xyz + vec3<f32>(support)) / h + vec3<f32>(0.5))) + vec3<i32>(1);
                    if !(reach > 0.0) || any(node < first) || any(node > last) {
                        continue;
                    }
                    phi = min(phi, length(p - blob.xyz) - reach);
                }
            }
        }
    }
    if buf_solid[idx] < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}
