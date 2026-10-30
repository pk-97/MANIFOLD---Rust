// One region row's half-cell sites (gpu_flip_domain/region_sites.rs
// RegionSites): the world half cell of its first site, the sites of the
// earlier rows of its tick (its first index in the tick's enumeration),
// the sites per axis and their product. World half cell J sits at
// (J / 2 − 1/4)·h on each axis, the fill's lattice and the engine's
// (_addNewFluidCellsThread): cell p holds half cells 2p and 2p + 1.
struct RegionSites {
    min: vec3<i32>,
    first: u32,
    size: vec3<u32>,
    count: u32,
};

// One per region row, tick-major like `regions`.
@group(0) @binding(44) var<storage, read> region_sites: array<RegionSites>;

// A site of this substep's tick: its region and world half cell. Thread idx
// of the tick's enumeration (row after row, x fastest in a box) belongs to
// region r when first <= idx < first + count; r is −1 past the tick's sites.
struct EmitSite {
    region: i32,
    half: vec3<i32>,
};

fn emit_site(idx: u32) -> EmitSite {
    let first = (u.tick_index - u.first_tick) * u.region_count;
    for (var r = 0; r < u.region_count; r = r + 1) {
        let row = first + r;
        if row < 0 || row >= u.region_rows {
            continue;
        }
        let box = region_sites[u32(row)];
        if idx < box.first || idx - box.first >= box.count {
            continue;
        }
        let j = idx - box.first;
        let local = vec3<u32>(j % box.size.x, (j / box.size.x) % box.size.y, j / (box.size.x * box.size.y));
        return EmitSite(r, box.min + vec3<i32>(local));
    }
    return EmitSite(-1, vec3<i32>(0));
}

// The world position of half cell J.
fn emit_half_position(j: vec3<i32>) -> vec3<f32> {
    return fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.origin);
}

// The inflow that emits at thread idx's site: the site's own region when it
// is the first inflow holding the site (so a site two inflows' boxes share
// is emitted once), −1 otherwise.
fn emit_inflow(site: EmitSite) -> i32 {
    if site.region < 0 {
        return -1;
    }
    let inflow = region_holding(emit_half_position(site.half), 2.0, true);
    return select(-1, inflow, inflow == site.region);
}

// Whether a live sorted particle already sits in half cell j: the engine
// skips a site whose subcell of its particle mask is set
// (_addNewFluidCells), so a source tops its volume up and never stacks. A
// cell whose tile has no slot holds no sorted particle of its own.
fn emit_site_taken(j: vec3<i32>) -> bool {
    let cell = j >> vec3<u32>(1u);
    if !ts_cell_stored(cell) {
        return false;
    }
    let range = ranges[ts_cell_at(cell)];
    for (var k = 0u; k < range.count; k = k + 1u) {
        let x = sorted[range.start + k].position_radius.xyz;
        let sub = vec3<i32>(floor(2.0 * (x - u.origin) * u.inv_cell));
        if all(sub == j) {
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

// The site's particle in world space: the site, and where the inflow holds
// it deeper than a cell, moved uniformly up to emit_jitter cells each way
// (_addNewFluidCellsThread 8795-8804, _jitterMarkerParticlePosition 4409),
// keyed by thread idx and the substep so each substep draws afresh. The
// move stays inside the site's half cell.
fn emit_position(idx: u32, site: EmitSite, inflow: i32) -> vec3<f32> {
    let x = emit_half_position(site.half);
    if u.emit_jitter <= 0.0 || inflow < 0 || region_distance(inflow, x) >= -u.cell_size {
        return x;
    }
    let substep = u32(u.tick_index) * 64u + u32(u.step_in_tick);
    let key = idx * 3u + substep * 2654435761u;
    let unit = vec3<f32>(emit_unit(key), emit_unit(key + 1u), emit_unit(key + 2u));
    return x + u.cell_size * u.emit_jitter * (2.0 * unit - vec3<f32>(1.0));
}

// One thread per site of the tick's enumeration (`emit_sites` threads): 1
// when an inflow emits there this substep, the site inside its inflow
// (distance at or below 0) and outside every solid and wall (solid distance
// above 0), with its half cell empty.
@compute @workgroup_size(256)
fn emit_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= u.emit_sites {
        return;
    }
    let site = emit_site(idx);
    let inflow = emit_inflow(site);
    var flag = 0u;
    if inflow >= 0 && !emit_site_taken(site.half) {
        // The solid test is at the jittered position, as the engine's.
        if solid_distance(emit_position(idx, site, inflow)) > 0.0 {
            flag = 1u;
        }
    }
    emit_scan[idx] = flag;
}

// One thread per site of the tick, after the flags' inclusive scan: a flagged
// site writes a new particle at rest in its inflow's velocity to slot
// live + rank of the sorted particles, live being the sorted prefix's end.
// A rank at or past `n`, the slots in use the birth stage left in the identity
// (`reserve` grants what the pool holds), emits nothing: a full pool shows as
// the stats' live count reaching the slots, never as an error.
@compute @workgroup_size(256)
fn emit_write(@builtin(global_invocation_id) gid: vec3<u32>) {
    if birth_identity[3] != 0u { return; }
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= u.emit_sites {
        return;
    }
    var before = 0u;
    if idx > 0u { before = emit_scan[idx - 1u]; }
    if emit_scan[idx] == before {
        return;
    }
    // The last key's range ends at the live total.
    let last = ranges[ts_pool_len() - 1u];
    let slot = last.start + last.count + before;
    if slot >= birth_identity[7] {
        return;
    }
    let site = emit_site(idx);
    let inflow = emit_inflow(site);
    let p = emit_position(idx, site, inflow);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, as the fill's.
    emitted[slot] = FluidParticle(vec4<f32>(p, birth_radius(u.cell_size, region_water(inflow))), region_velocity(inflow, p), birth_identity[2] + before);
}

// One thread, on the tick's first substep before any birth stage: the
// tick's birth words (inflow births, inflow and sheet births the pool had no
// slot for) and the particles removed for want of a tile slot start at
// zero, whether or not this tick runs inflows or sheeting, so a stage
// switched off leaves no stale refusal on the card.
@compute @workgroup_size(1)
fn birth_tally_reset() {
    capped[INFLOW_BIRTHS_TAIL] = 0u;
    capped[INFLOW_REFUSED_TAIL] = 0u;
    capped[SHEET_REFUSED_TAIL] = 0u;
    capped[TILE_REMOVED_TAIL] = 0u;
    capped[RESEED_REFUSED_TAIL] = 0u;
}

// One thread, after emit_write: the substep's inflow births (the slots in use
// the stage added, `n` less the live end) and the ones the pool had no free
// slot for, summed over the tick into the solver tail (stats words 27 and 28).
// A refusal is how a full pool reaches the setup check.
@compute @workgroup_size(1)
fn emit_tally() {
    if !clock_active() || birth_identity[3] != 0u { return; }
    let requested = emit_scan[u.emit_sites - 1u];
    let last = ranges[ts_pool_len() - 1u];
    let live = last.start + last.count;
    let in_use = birth_identity[7];
    let births = select(0u, in_use - live, in_use > live);
    capped[INFLOW_BIRTHS_TAIL] += births;
    capped[INFLOW_REFUSED_TAIL] += requested - births;
}

@group(0) @binding(47) var<storage, read> birth_identity: array<u32>;
