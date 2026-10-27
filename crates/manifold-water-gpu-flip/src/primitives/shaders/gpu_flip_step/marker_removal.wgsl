// FLIP Fluids _removeMarkerParticles counts before testing speed.
// Stable ranges predate speed removal; surviving ids stay unchanged.
// 512 threads a slot of C, the tiles within CELL_REACH cells of the
// particles as the substep began. A cell the move filled outside C is
// checked on the next substep, once its tile is in C.
@compute @workgroup_size(256)
fn remove_crowded_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let range = ranges[list_cell(gid.x)];
    for (var k = 250u; k < range.count; k = k + 1u) {
        emitted[range.start + k].position_radius.w = 0.0;
    }
}
