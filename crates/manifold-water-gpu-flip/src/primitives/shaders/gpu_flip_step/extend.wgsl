// One thread per face record, `faces_in` to `faces_out`: one layer of the
// fluid velocity's extrapolation (GridUtils::extrapolateGrid). A valid face
// (weight > 0) is copied. Every face on the border of its component's
// lattice (solid_border) is held done, as GridUtils::
// _initializeStatusGridThread holds it: kept as it is, counted with its
// value in a neighbour's mean whatever its weight, never a seed. An invalid
// inner face with a valid inner neighbour takes the mean of its valid and
// border neighbours and becomes valid (weight 1). The border rows lie in
// the solid padding, so their zeros slow the extrapolated velocity at the
// walls as the engine's do.
@compute @workgroup_size(256)
fn extend_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let here = faces_in[idx];
    for (var a = 0; a < 3; a = a + 1) {
        // Faces of component a span 0..=n on axis a and 0..n on the others.
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        if any(p > top) {
            continue;
        }
        out.face_velocity[a] = here.face_velocity[a];
        out.face_weight[a] = here.face_weight[a];
        if here.face_weight[a] > 0.0 || solid_border(p, a, n) {
            continue;
        }
        var sum = 0.0;
        var hits = 0.0;
        var seeded = false;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                if q[b] < 0 || q[b] > top[b] {
                    continue;
                }
                let neighbour = faces_in[flatten(q, m)];
                let border = solid_border(q, a, n);
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
    faces_out[idx] = out;
}

