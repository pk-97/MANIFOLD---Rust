// 512 threads per held slot (the first `tile_counts[ring_max]` slots of
// `tiles_by_ring`), one per cell and its own record, `faces_in` to
// `faces_out`: one layer of the fluid velocity's extrapolation
// (GridUtils::extrapolateGrid). A valid face (weight > 0) is copied. An
// invalid face with a valid neighbour takes the mean of its valid
// neighbours and becomes valid (weight 1). A face in a wall's solid is an
// ordinary face here: the solid constraint sets its velocity afterwards. A
// dispatch that covers every slot (the replayed one) has its threads past the
// list return.
@compute @workgroup_size(256)
fn extend_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if (gid.x >> 9u) >= tile_counts[u.ring_max] {
        return;
    }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let here = faces_in[at];
    for (var a = 0; a < 3; a = a + 1) {
        out.face_velocity[a] = here.face_velocity[a];
        out.face_weight[a] = here.face_weight[a];
        if here.face_weight[a] > 0.0 {
            continue;
        }
        var sum = 0.0;
        var hits = 0.0;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                let neighbour = faces_in[ts_face(q)];
                if neighbour.face_weight[a] > 0.0 {
                    sum = sum + neighbour.face_velocity[a];
                    hits = hits + 1.0;
                }
            }
        }
        if hits > 0.0 {
            out.face_velocity[a] = sum / hits;
            out.face_weight[a] = 1.0;
        }
    }
    faces_out[at] = out;
}
