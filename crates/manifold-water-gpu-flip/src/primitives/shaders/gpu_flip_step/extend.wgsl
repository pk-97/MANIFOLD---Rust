// 512 threads per held slot (the first `tile_counts[ring_max]` slots of
// `tiles_by_ring`), one per cell and its own record, `faces_in` to
// `faces_out`: one layer of the fluid velocity's extrapolation
// (GridUtils::extrapolateGrid). A valid face (weight > 0) is copied. An
// invalid face with a valid neighbour takes the mean of its valid
// neighbours and becomes valid (weight 1). A face in a wall's solid is an
// ordinary face here: the solid constraint sets its velocity afterwards. A
// dispatch that covers every slot (the replayed one) has its threads past the
// list return. The six neighbours are read once for all three axes, and not
// at all where every axis is valid.
@compute @workgroup_size(256)
fn extend_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if (gid.x >> 9u) >= tile_counts[u.ring_max] {
        return;
    }
    let at = list_cell(gid.x);
    let here = faces_in[at];
    var out = FaceSample(vec4<f32>(here.face_velocity.xyz, 0.0), vec4<f32>(here.face_weight.xyz, 0.0));
    let valid = here.face_weight.xyz > vec3<f32>(0.0);
    if all(valid) {
        faces_out[at] = out;
        return;
    }
    let p = ts_face_coord(at);
    var sum = vec3<f32>(0.0);
    var hits = vec3<f32>(0.0);
    for (var b = 0; b < 3; b = b + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[b] = p[b] + d;
            let neighbour = faces_in[ts_face(q)];
            let known = neighbour.face_weight.xyz > vec3<f32>(0.0);
            sum = sum + select(vec3<f32>(0.0), neighbour.face_velocity.xyz, known);
            hits = hits + select(vec3<f32>(0.0), vec3<f32>(1.0), known);
        }
    }
    for (var a = 0; a < 3; a = a + 1) {
        if !valid[a] && hits[a] > 0.0 {
            out.face_velocity[a] = sum[a] / hits[a];
            out.face_weight[a] = 1.0;
        }
    }
    faces_out[at] = out;
}
