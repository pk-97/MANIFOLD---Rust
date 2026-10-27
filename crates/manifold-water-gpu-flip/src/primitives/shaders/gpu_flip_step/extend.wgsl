// 512 threads per held slot (the first `tile_counts[ring_max]` slots of
// `tiles_by_ring`), one per cell and its own record, `faces_in` to
// `faces_out`: one layer of the fluid velocity's extrapolation
// (GridUtils::extrapolateGrid). A valid face (weight > 0) is copied. Every
// face on the border of its component's lattice (solid_border) is held done,
// as GridUtils::_initializeStatusGridThread holds it: kept as it is, counted
// with its value in a neighbour's mean whatever its weight, never a seed. An
// invalid inner face with a valid inner neighbour takes the mean of its valid
// and border neighbours and becomes valid (weight 1). The border rows lie in
// the solid padding, so their zeros slow the extrapolated velocity at the
// walls as the engine's do. A record outside the box's record range is no
// face of the lattice, never written, and stays canonical. A dispatch that
// covers every slot (the replayed one) has its threads past the list return.
@compute @workgroup_size(256)
fn extend_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if (gid.x >> 9u) >= tile_counts[u.ring_max] {
        return;
    }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    if !in_box_records(p) {
        return;
    }
    let lo = box_lo();
    let n = box_cells();
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let here = faces_in[at];
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, a) {
            continue;
        }
        // Faces of component a span lo..=lo + n on axis a and lo..lo + n on
        // the others.
        var top = lo + n - vec3<i32>(1);
        top[a] = lo[a] + n[a];
        out.face_velocity[a] = here.face_velocity[a];
        out.face_weight[a] = here.face_weight[a];
        if here.face_weight[a] > 0.0 || solid_border(p, a) {
            continue;
        }
        var sum = 0.0;
        var hits = 0.0;
        var seeded = false;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                if q[b] < lo[b] || q[b] > top[b] {
                    continue;
                }
                let neighbour = faces_in[ts_face(q)];
                let border = solid_border(q, a);
                if border || neighbour.face_weight[a] > 0.0 {
                    sum = sum + neighbour.face_velocity[a];
                    hits = hits + 1.0;
                    seeded = seeded || !border;
                }
            }
        }
        if seeded {
            out.face_velocity[a] = sum / hits;
            out.face_weight[a] = 1.0;
        }
    }
    faces_out[at] = out;
}
