// FLIP Fluids _removeMarkerParticles counts before testing speed.
// Stable ranges predate speed removal; surviving ids stay unchanged.
@compute @workgroup_size(256)
fn remove_crowded_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if gid.x >= ts_cell_count() { return; }
    let p = ts_box_cell(gid.x);
    if !ts_cell_stored(p) { return; }
    let range = ranges[ts_cell_at(p)];
    for (var k = 250u; k < range.count; k = k + 1u) {
        emitted[range.start + k].position_radius.w = 0.0;
    }
}
