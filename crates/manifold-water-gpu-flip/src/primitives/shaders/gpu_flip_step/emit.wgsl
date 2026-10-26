// Half-cell sites per axis, 2n: site j at (1/4 + j/2) cells, the fill's
// lattice and the engine's (_addNewFluidCellsThread).
fn emit_sites() -> vec3<u32> {
    return 2u * u.n;
}

fn emit_site(idx: u32) -> vec3<u32> {
    let s = emit_sites();
    return vec3<u32>(idx % s.x, (idx / s.x) % s.y, idx / (s.x * s.y));
}

// Whether a live sorted particle already sits in site j's half cell: the
// engine skips a site whose subcell of its particle mask is set
// (_addNewFluidCells), so a source tops its volume up and never stacks.
fn emit_site_taken(j: vec3<u32>) -> bool {
    let cell = vec3<i32>(j / 2u);
    let range = ranges[ts_dense_cell(cell)];
    for (var k = 0u; k < range.count; k = k + 1u) {
        let x = sorted[range.start + k].position_radius.xyz;
        let sub = vec3<i32>(floor(2.0 * (x - u.box_min) / u.cell_size));
        if all(sub == vec3<i32>(j)) {
            return true;
        }
    }
    return false;
}

fn emit_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn emit_unit(x: u32) -> f32 {
    return f32(emit_hash(x) >> 8u) / 16777216.0;
}

// Site idx's particle in world space: the site, and where the inflow holds it
// deeper than a cell, moved uniformly up to emit_jitter cells each way
// (_addNewFluidCellsThread 8795-8804, _jitterMarkerParticlePosition 4409),
// keyed by the site and the substep so each substep draws afresh. The move
// stays inside the site's half cell.
fn emit_position(idx: u32, inflow: i32) -> vec3<f32> {
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    if u.emit_jitter <= 0.0 || inflow < 0 || region_distance(inflow, x) >= -u.cell_size {
        return x;
    }
    let substep = u32(u.tick_index) * 64u + u32(u.step_in_tick);
    let key = idx * 3u + substep * 2654435761u;
    let unit = vec3<f32>(emit_unit(key), emit_unit(key + 1u), emit_unit(key + 2u));
    return x + u.cell_size * u.emit_jitter * (2.0 * unit - vec3<f32>(1.0));
}

// One thread per half-cell site: 1 when an inflow emits there this substep,
// the site inside an inflow (distance at or below 0) and outside every solid
// and wall (solid distance above 0), with its half cell empty.
@compute @workgroup_size(256)
fn emit_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    let s = emit_sites();
    if idx >= s.x * s.y * s.z {
        return;
    }
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    let inflow = region_holding(x, 2.0, true);
    var flag = 0u;
    if inflow >= 0 && !emit_site_taken(j) {
        // The solid test is at the jittered position, as the engine's.
        let p = emit_position(idx, inflow);
        if solid_at((p - u.box_min) / u.cell_size, ts_cells()) > 0.0 {
            flag = 1u;
        }
    }
    emit_scan[idx] = flag;
}

// One thread per half-cell site, after the flags' inclusive scan: a flagged
// site writes a new particle at rest in its inflow's velocity to slot
// live + rank of the sorted particles, live being the sorted prefix's end.
// A rank past the pool's slots emits nothing: a full pool shows as the
// stats' live count reaching the slots, never as an error.
@compute @workgroup_size(256)
fn emit_write(@builtin(global_invocation_id) gid: vec3<u32>) {
    if birth_identity[3] != 0u { return; }
    if !clock_active() { return; }
    let idx = gid.x;
    let s = emit_sites();
    if idx >= s.x * s.y * s.z {
        return;
    }
    var before = 0u;
    if idx > 0u { before = emit_scan[idx - 1u]; }
    if emit_scan[idx] == before {
        return;
    }
    let n = ts_cells();
    let last = ranges[ts_dense_cell(n - vec3<i32>(1))];
    let slot = last.start + last.count + before;
    if slot >= u.capacity {
        return;
    }
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    let inflow = region_holding(x, 2.0, true);
    let p = emit_position(idx, inflow);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, as the fill's.
    emitted[slot] = FluidParticle(vec4<f32>(p, 0.31017524 * u.cell_size), region_velocity(inflow, p), birth_identity[2] + before);
}

// One thread, on the tick's first substep before any birth stage: the
// tick's birth words (inflow births, inflow and sheet births the pool had no
// slot for) start at zero, whether or not this tick runs inflows or sheeting,
// so a stage switched off leaves no stale refusal on the card.
@compute @workgroup_size(1)
fn birth_tally_reset() {
    capped[INFLOW_BIRTHS_TAIL] = 0u;
    capped[INFLOW_REFUSED_TAIL] = 0u;
    capped[SHEET_REFUSED_TAIL] = 0u;
}

// One thread, after emit_write: the substep's inflow births and the ones the
// pool had no free slot for, summed over the tick into the solver tail
// (stats words 27 and 28). A refusal is how a full pool reaches the setup
// check.
@compute @workgroup_size(1)
fn emit_tally() {
    if !clock_active() || birth_identity[3] != 0u { return; }
    let s = emit_sites();
    let requested = emit_scan[s.x * s.y * s.z - 1u];
    let n = ts_cells();
    let last = ranges[ts_dense_cell(n - vec3<i32>(1))];
    let live = last.start + last.count;
    let room = select(0u, u.capacity - live, u.capacity > live);
    let births = min(requested, room);
    capped[INFLOW_BIRTHS_TAIL] += births;
    capped[INFLOW_REFUSED_TAIL] += requested - births;
}

@group(0) @binding(47) var<storage, read> birth_identity: array<u32>;
