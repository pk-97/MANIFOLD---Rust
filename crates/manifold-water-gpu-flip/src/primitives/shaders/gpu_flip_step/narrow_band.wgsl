// The narrow band (Ferstl et al. 2016, Narrow Band FLIP for Liquid
// Simulations, CGF 35(2), equations (3) and (4)); gpu_flip_step/narrow_band.rs
// encodes it. Particles live only in a band `band_cells` deep under the
// surface; deeper water is the grid's. The grid interior `nb_grid` is the
// last substep's distance advected to its end; the step's φ is the particles'
// united with it shrunk by a cell, its faces deep in the water the grid's
// advected faces. After the move a particle deeper than the band is removed,
// and a cell entering the band from the deep is reseeded. Every pooled field
// here is read and written through the world tiles like the step's own.

// The grid interior: the substep's distance, advected along its velocity to
// its end. Persistent in held slots; a fresh slot and the canonical element
// hold the canonical φ.
@group(0) @binding(65) var<storage, read_write> nb_grid: array<f32>;
// The grid's faces: the substep's velocity advected along itself, weight 1
// where the advected interior holds water.
@group(0) @binding(66) var<storage, read_write> nb_grid_faces: array<FaceSample>;
// The substep's distance (the united φ redistanced, gpu_flip_sheet_distance),
// written over the held slots.
@group(0) @binding(67) var<storage, read> nb_distance: array<f32>;
// Per slot, the lowest local index of a cell whose interior holds water:
// the next build's kept point (world_tiles_kept.wgsl).
@group(0) @binding(68) var<storage, read_write> nb_kept: array<atomic<u32>>;
// Word 0: the identity epoch + 1 the interior was last advected under.
@group(0) @binding(69) var<storage, read_write> nb_status: array<u32>;
// Per pool element, the sites the cell reseeds, then their inclusive scan.
@group(0) @binding(70) var<storage, read_write> nb_scan: array<u32>;

const NB_ON: u32 = 1u;
// The step after Narrow Band is switched off: the band runs once more and
// refills the water the grid holds with particles.
const NB_RESTORE: u32 = 2u;

// R: the band's depth in metres.
fn nb_band() -> f32 {
    return u.band_cells * u.cell_size;
}

// r: faces shallower than this, in cells, keep the particles' velocity.
fn nb_face_cells() -> f32 {
    return u.band_cells - 1.0;
}

// The interior was advected by an earlier substep of this liquid: not the
// epoch's first substep, and not since a recovery reseeded the identity.
fn nb_grid_valid() -> bool {
    return (u.tick_index > 0 || u.step_in_tick > 0) && nb_status[0] == birth_identity[1] + 1u;
}

// Pool index idx of the distance (field 0) or the interior (field 1). The
// distance holds no canonical element: an absent tile reads as air.
fn nb_cell(field: u32, idx: u32) -> f32 {
    if field == 0u {
        if idx == ts_canon() {
            return CANON_PHI_CELLS * u.cell_size;
        }
        return nb_distance[idx];
    }
    return nb_grid[idx];
}

// A cell field at q, in cells from world cell lo (cell centres at + ½),
// trilinear.
fn nb_sample(field: u32, q: vec3<f32>, lo: vec3<i32>) -> f32 {
    let s = q - vec3<f32>(0.5);
    let base = vec3<i32>(floor(s));
    let t = s - vec3<f32>(base);
    var sum = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
        let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        sum = sum + w3.x * w3.y * w3.z * nb_cell(field, ts_cell_at(base + bit + lo));
    }
    return sum;
}

// Below the band and clear of every solid by the band: the grid's water.
fn nb_deep(value: f32, q: vec3<f32>, lo: vec3<i32>) -> bool {
    return value < -nb_band() && solid_at(q, lo) > u.band_cells;
}

// Cell p (pool index idx) is the grid's water this substep.
fn nb_deep_cell(idx: u32, p: vec3<i32>) -> bool {
    return u.narrow_band != 0u && nb_deep(nb_cell(0u, idx), vec3<f32>(0.5), p);
}

// The RK4 backtrace of q (cells from lo) through the new faces over this
// substep's dt, the paper's surface tracking order.
fn nb_backtrace(q: vec3<f32>, lo: vec3<i32>) -> vec3<f32> {
    let dt = adaptive_step_dt() / u.cell_size;
    let a = sample(q, lo, 0u);
    let b = sample(q - 0.5 * dt * a, lo, 0u);
    let c = sample(q - 0.5 * dt * b, lo, 0u);
    let d = sample(q - dt * c, lo, 0u);
    return q - (dt / 6.0) * (a + 2.0 * b + 2.0 * c + d);
}

// Face a of a record, in cells from its cell: at 0 on axis a, ½ on the others.
fn nb_face_q(a: i32) -> vec3<f32> {
    var q = vec3<f32>(0.5);
    q[a] = 0.0;
    return q;
}

// 512 threads a fresh slot: the interior and its faces canonical.
@compute @workgroup_size(256)
fn nb_fresh(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_fresh_list[gid.x >> 9u], gid.x);
    nb_grid[idx] = CANON_PHI_CELLS * u.cell_size;
    nb_grid_faces[idx] = CANONICAL_FACE;
}

// One thread, once per interior storage: the canonical element.
@compute @workgroup_size(1)
fn nb_canon() {
    nb_grid[ts_canon()] = CANON_PHI_CELLS * u.cell_size;
    nb_grid_faces[ts_canon()] = CANONICAL_FACE;
}

// 512 threads a slot of C, after the particle distance into `cell_out`: the
// union with the interior shrunk by a cell (Ferstl equation 3). A non-finite
// interior never wins the comparison.
@compute @workgroup_size(256)
fn nb_union(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || !nb_grid_valid() { return; }
    let idx = list_cell(gid.x);
    let grid = nb_grid[idx] + u.cell_size;
    if grid < cell_out[idx] {
        cell_out[idx] = grid;
    }
}

// 512 threads a slot of C, after the gather into `faces_out`: a face deeper
// than r and clear of the solids by r takes the grid's face, and so does one
// no particle reached (Ferstl equation 4).
@compute @workgroup_size(256)
fn nb_combine(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || !nb_grid_valid() { return; }
    let idx = list_cell(gid.x);
    let lo = ts_cell_coord(idx);
    var out = faces_out[idx];
    let grid = nb_grid_faces[idx];
    let r = nb_face_cells();
    for (var a = 0; a < 3; a = a + 1) {
        let q = nb_face_q(a);
        let particles = nb_sample(0u, q, lo) >= -r * u.cell_size || solid_at(q, lo) <= r;
        if !particles || !(out.face_weight[a] > 0.0) {
            out.face_velocity[a] = grid.face_velocity[a];
            out.face_weight[a] = grid.face_weight[a];
        }
    }
    faces_out[idx] = out;
}

// 512 threads a held slot, after the move: the interior is the substep's
// distance at each cell centre's backtrace through the new faces.
@compute @workgroup_size(256)
fn nb_advect_phi(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let lo = ts_cell_coord(idx);
    nb_grid[idx] = nb_sample(0u, nb_backtrace(vec3<f32>(0.5), lo), lo);
    if gid.x == 0u {
        nb_status[0] = birth_identity[1] + 1u;
    }
}

// 512 threads a held slot, after nb_advect_phi: each face of the record the
// new faces' velocity at its backtrace, known where the interior holds water.
@compute @workgroup_size(256)
fn nb_advect_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let lo = ts_cell_coord(idx);
    var out = CANONICAL_FACE;
    for (var a = 0; a < 3; a = a + 1) {
        let q = nb_face_q(a);
        if nb_sample(1u, q, lo) < 0.0 {
            out.face_velocity[a] = sample(nb_backtrace(q, lo), lo, 0u)[a];
            out.face_weight[a] = 1.0;
        }
    }
    nb_grid_faces[idx] = out;
}

// One thread a slot in use of the sorted particles, after the move's sort: a
// particle the interior holds deeper than the band, clear of the solids, is
// removed.
@compute @workgroup_size(256)
fn nb_delete(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= slots_in_use() {
        return;
    }
    let p = emitted[idx];
    if !(p.position_radius.w > 0.0) {
        return;
    }
    let x = p.position_radius.xyz;
    let lo = world_cell(x);
    ts_home(ts_cell_at(lo));
    let q = (x - u.origin) * u.inv_cell - vec3<f32>(lo);
    if nb_deep(nb_sample(1u, q, lo), q, lo) {
        emitted[idx].position_radius.w = 0.0;
    }
}

// Site k of a cell, in cells from its low corner: the quarter sites the fill
// seeds, cell + 0.25 + 0.5·bit.
fn nb_site(k: u32) -> vec3<f32> {
    let bit = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u);
    return vec3<f32>(0.25) + 0.5 * vec3<f32>(bit);
}

// The sites cell p (pool index idx) reseeds, a bit each: a cell entering the
// band, deep by the substep's distance and inside the band by the advected
// interior (every water cell a cell deep when restoring), gets its free
// octants that the interior holds a cell deep and no solid holds, up to eight
// particles with its own. The write pass recomputes the same bits.
fn nb_reseed_sites(idx: u32, p: vec3<i32>) -> u32 {
    let grid = nb_grid[idx];
    if grid > -u.cell_size {
        return 0u;
    }
    if u.narrow_band != NB_RESTORE && (nb_cell(0u, idx) > -nb_band() || grid <= -nb_band()) {
        return 0u;
    }
    let range = cell_range(idx);
    var occupied = 0u;
    var live = 0u;
    for (var j = 0u; j < range.count; j = j + 1u) {
        let particle = emitted[range.start + j];
        if !(particle.position_radius.w > 0.0) {
            continue;
        }
        live = live + 1u;
        let q = (particle.position_radius.xyz - u.origin) * u.inv_cell - vec3<f32>(p);
        let sub = clamp(vec3<i32>(floor(2.0 * q)), vec3<i32>(0), vec3<i32>(1));
        occupied = occupied | (1u << u32(sub.x + 2 * sub.y + 4 * sub.z));
    }
    var sites = 0u;
    var selected = 0u;
    for (var k = 0u; k < 8u; k = k + 1u) {
        if selected + live >= 8u {
            break;
        }
        let q = nb_site(k);
        if (occupied & (1u << k)) == 0u && nb_sample(1u, q, p) <= -u.cell_size && solid_at(q, p) > 0.0 {
            sites = sites | (1u << k);
            selected = selected + 1u;
        }
    }
    return sites;
}

// 512 threads a slot of C, after the move's sort (`nb_scan` cleared): each
// cell's reseed count, scanned next.
@compute @workgroup_size(256)
fn nb_reseed_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    nb_scan[idx] = countOneBits(nb_reseed_sites(idx, ts_cell_coord(idx)));
}

// 512 threads a slot of C, after the scan and the birth reservation: each
// reseeded site a particle at rest in the new faces' velocity, at slot live +
// rank past the sorted prefix, the reservation's ids. A stage the pool refused
// writes nothing.
@compute @workgroup_size(256)
fn nb_reseed_write(@builtin(global_invocation_id) gid: vec3<u32>) {
    if birth_identity[3] != 0u { return; }
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    var before = 0u;
    if idx > 0u { before = nb_scan[idx - 1u]; }
    if nb_scan[idx] == before {
        return;
    }
    let p = ts_cell_coord(idx);
    let sites = nb_reseed_sites(idx, p);
    let last = ranges[ts_pool_len() - 1u];
    var rank = before;
    for (var k = 0u; k < 8u; k = k + 1u) {
        if (sites & (1u << k)) == 0u {
            continue;
        }
        let slot = last.start + last.count + rank;
        if slot < birth_identity[7] {
            let q = nb_site(k);
            let x = u.origin + (vec3<f32>(p) + q) * u.cell_size;
            // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, as the fill's.
            emitted[slot] = FluidParticle(vec4<f32>(x, 0.31017524 * u.cell_size), sample(q, p, 0u), birth_identity[2] + rank);
        }
        rank = rank + 1u;
    }
}

// One thread, after nb_reseed_write: the reseeds the pool had no free slot
// for, summed over the tick into the solver tail (stats word 31).
@compute @workgroup_size(1)
fn nb_reseed_tally() {
    if !clock_active() || birth_identity[3] != 0u { return; }
    let requested = nb_scan[ts_pool_len() - 1u];
    let last = ranges[ts_pool_len() - 1u];
    let live = last.start + last.count;
    let in_use = birth_identity[7];
    let births = select(0u, in_use - live, in_use > live);
    capped[RESEED_REFUSED_TAIL] += requested - births;
}

// P threads: every slot keeps no cell until nb_kept says otherwise.
@compute @workgroup_size(256)
fn nb_kept_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || gid.x >= ts_pool_tiles() { return; }
    atomicStore(&nb_kept[gid.x], WT_NONE);
}

// 512 threads a held slot, after nb_kept_clear: the lowest cell of the slot
// the interior holds water in.
@compute @workgroup_size(256)
fn nb_keep(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    if nb_grid[idx] < 0.0 {
        atomicMin(&nb_kept[idx >> 9u], idx & 511u);
    }
}
