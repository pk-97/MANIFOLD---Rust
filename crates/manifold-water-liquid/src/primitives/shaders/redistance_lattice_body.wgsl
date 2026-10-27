var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.redistance_lattice — fusable BUFFER body, GATHER. One thread per pool
// element of the display's world tiles (`buf_tiles`, `pool_tiles` tiles):
// the exact Euclidean distance, in metres, from the node to the
// piecewise-linear marching-cubes zero surface of `levelset`, capped at the
// band (band_cells cell sizes). This is a geometric distance rebuild, not a
// blur. The search box contains every marching-cubes cell intersecting the
// band-radius ball; there is no fixed stencil/slider cap. `levelset` is a
// node array: element ts_cell_at(p) holds node p, and a node in a tile the
// table does not hold reads the canonical element, far from the surface. The
// canonical element and a free slot hold the canonical level set.
fn rd_segment(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let v = b - a;
    let vv = dot(v, v);
    if vv == 0.0 { return dot(p-a, p-a); }
    let d = p - (a + clamp(dot(p-a, v)/vv, 0.0, 1.0)*v);
    return dot(d,d);
}
fn rd_triangle(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> f32 {
    let ab = b-a;
    let ac = c-a;
    let n = cross(ab, ac);
    let nn = dot(n,n);
    var best = min(rd_segment(p,a,b), min(rd_segment(p,b,c), rd_segment(p,c,a)));
    if nn > 0.0 {
        let q = p-a;
        let u = dot(cross(q,ac), n)/nn;
        let v = dot(cross(ab,q), n)/nn;
        if u >= 0.0 && v >= 0.0 && u+v <= 1.0 {
            best = min(best, dot(q,n)*dot(q,n)/nn);
        }
    }
    return best;
}
fn body(idx: u32, count: u32, cell_size: f32, band_cells: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    // FIELD_FAR_CELLS (scene_object) cells: the level set where no tile is stored.
    let far = 3.0 * cell_size;
    if idx >= ts_canon() || ts_slot_entry(idx >> 9u) == WT_NONE { return far; }
    ts_home(idx);
    let at = ts_cell_coord(idx);
    let original = buf_levelset[idx];
    let band_m = band_cells * cell_size;
    // Positions are metres from this node, so the node is the origin.
    let p = vec3<f32>(0.0);
    let reach = i32(ceil(band_cells));
    let low = at - vec3<i32>(reach + 1);
    let high = at + vec3<i32>(reach);
    var best = band_m*band_m;
    for (var z=low.z; z<=high.z; z=z+1) {
        for (var y=low.y; y<=high.y; y=y+1) {
            for (var x=low.x; x<=high.x; x=x+1) {
                let cell = vec3<i32>(x,y,z);
                let box_low = vec3<f32>(cell-at)*cell_size;
                let box_high = vec3<f32>(cell-at+vec3<i32>(1))*cell_size;
                let delta = max(max(box_low-p,p-box_high),vec3<f32>(0.0));
                if dot(delta,delta) > best { continue; }
                var corner: array<f32,8>;
                var case_index = 0u;
                for (var c=0u; c<8u; c=c+1u) {
                    corner[c] = buf_levelset[ts_cell_at(cell+vec3<i32>(MC_CORNERS[c]))];
                    if corner[c] < 0.0 { case_index = case_index | (1u << c); }
                }
                let triangles = MC_TRIANGLE_COUNT[case_index];
                if triangles == 0u { continue; }
                var vertices: array<vec3<f32>,12>;
                // An edge crosses where the case's inside bits differ: one
                // classification per corner, so a corner of exactly zero can
                // never be inside for the case and outside for its edges
                // (which would leave a triangle vertex unset at the node).
                for (var e=0u; e<12u; e=e+1u) {
                    let ca = MC_EDGE_A[e];
                    let cb = MC_EDGE_B[e];
                    if ((case_index >> ca) & 1u) != ((case_index >> cb) & 1u) {
                        let va = corner[ca];
                        let d = va - corner[cb];
                        let t = select(0.5, va / d, d != 0.0);
                        let a = cell+vec3<i32>(MC_CORNERS[ca])-at;
                        let b = cell+vec3<i32>(MC_CORNERS[cb])-at;
                        vertices[e] = (vec3<f32>(a)+t*vec3<f32>(b-a))*cell_size;
                    }
                }
                for (var t=0u; t<triangles; t=t+1u) {
                    best = min(best,rd_triangle(p,vertices[mc_edge(case_index,3u*t)],
                        vertices[mc_edge(case_index,3u*t+1u)],vertices[mc_edge(case_index,3u*t+2u)]));
                }
            }
        }
    }
    return select(sqrt(best),-sqrt(best),original < 0.0);
}
