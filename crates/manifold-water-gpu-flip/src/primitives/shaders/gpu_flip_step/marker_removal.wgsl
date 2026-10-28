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

// ---- The sheet seeding's water budget ----
// Seeding adds particles to thin sheets; without a matching removal the
// density correction spreads them and the water grows. The sheeting's stats
// (`ledger`) carry the particles owed: its `tally` adds each substep's
// births to word 4, and after the move each owing cell here gives one back.
// A cell owes when it holds at least OWED_CROWD particles (REST_DENSITY; the
// density correction holds full cells there, so a stricter crowd runs out)
// and lies deeper than OWED_DEPTH_CELLS, the sheet candidates' depth, so the
// surface never loses a particle and the density correction refills the
// cell from round it, lowering the water a little. Each
// owing cell gives one with chance owed / owing cells, at most one a cell a
// substep, so the water pays its debt at the rate the sheets drew it and a
// debt left while the water is all sheet is paid once it gathers deep.
// Word 4 is the debt (i32 bits), word 5 the owing cells this substep, word 6
// the particles given back since the stage was reserved.
@group(0) @binding(71) var<storage, read_write> ledger: array<atomic<u32>>;

const OWED_CROWD: u32 = 8u;
// MAX_SHEET_DEPTH_CELLS.
const OWED_DEPTH_CELLS: f32 = 2.0;

fn owed() -> i32 {
    return bitcast<i32>(atomicLoad(&ledger[4]));
}

// `phi` is bound to the sheeter's distance, true well past this depth.
fn owing(idx: u32) -> bool {
    return ranges[idx].count >= OWED_CROWD && phi[idx] < -OWED_DEPTH_CELLS * u.cell_size;
}

// One thread, before the substep's sheeting: no owing cells counted yet, and
// nothing owed at the start of a run.
@compute @workgroup_size(1)
fn owed_clear() {
    if !clock_active() { return; }
    atomicStore(&ledger[5], 0u);
    if u.tick_index <= 0 && u.step_in_tick <= 0 {
        atomicStore(&ledger[4], 0u);
    }
}

@compute @workgroup_size(256)
fn owed_count(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || owed() <= 0 { return; }
    if owing(list_cell(gid.x)) {
        atomicAdd(&ledger[5], 1u);
    }
}

// The cell's last live particle: the sort is stable, so its newest arrival.
@compute @workgroup_size(256)
fn remove_owed_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let debt = owed();
    let cells = atomicLoad(&ledger[5]);
    if debt <= 0 || cells == 0u { return; }
    let idx = list_cell(gid.x);
    if !owing(idx) { return; }
    let draw = emit_unit(idx ^ emit_hash(u32(u.tick_index) * 64u + u32(u.step_in_tick) + 0x0bedu));
    if draw >= f32(debt) / f32(cells) { return; }
    let range = ranges[idx];
    for (var k = range.count; k > 0u; k = k - 1u) {
        let at = range.start + k - 1u;
        if emitted[at].position_radius.w > 0.0 {
            emitted[at].position_radius.w = 0.0;
            atomicSub(&ledger[4], 1u);
            atomicAdd(&ledger[6], 1u);
            return;
        }
    }
}
