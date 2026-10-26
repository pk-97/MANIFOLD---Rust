// Rest density: eight evenly placed particles per cell, each weighed by the
// tent kernel at the cell's centre, sum to 8.
const REST_DENSITY: f32 = 8.0;

// What a solid neighbour cell at offset d would weigh at the centre if it
// held eight evenly placed particles, as the paper samples solids with
// particles: Π over axes of 1.5 at offset 0 and 0.25 at ±1. A face neighbour
// weighs 0.5625, an edge one 0.09375, a corner one 0.015625; with the cell's
// own and its water neighbours they sum to REST_DENSITY. blub's live branch
// keeps only the face term; without the rest a seeded cell at a wall reads
// 5.5% light and a still pool creeps toward the walls.
// A cell whose centre is this many cells clear of every body has no site
// inside one; its rest sites lie within 0.44 cells of the centre.
const SOLID_SITE_REACH: f32 = 1.75;

fn solid_neighbour_density(d: vec3<i32>) -> f32 {
    let w = select(vec3<f32>(1.5), vec3<f32>(0.25), d != vec3<i32>(0));
    return w.x * w.y * w.z;
}

// The source's clamp: one projection moves a particle at most about half a
// cell.
const MAX_DENSITY_ERROR: f32 = 0.5;

// Kugelstadt et al. 2019's density source (see gpu_flip_step.rs). One thread
// per cell, the sorted particles to `cell_out`. In a water cell, ρ is the sum
// of the tent weights Π(1 − |c − q|) of the particles within a cell of its
// centre c, plus what solid rest sites would weigh: solid_neighbour_density
// for each neighbour outside the box, each body site's tent weight. Beside
// a face neighbour holding no particles ρ is at least REST_DENSITY, since a part full
// surface cell is not thin water. Out is −rate · clamp(ρ / ρ0 − 1, ±½),
// so the solve's pressure gradient moves particles out of crowded cells and
// into sparse ones; 0 outside the water.
@compute @workgroup_size(256)
fn density_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = c_cell_index(gid.x);
    if idx == TS_NONE {
        return;
    }
    if u.narrow_band == 2u && !narrow_cell(idx) {
        cell_out[idx] = 0.0;
        return;
    }
    if !(water[idx] > 0.5) {
        cell_out[idx] = 0.0;
        return;
    }
    let n = ts_cells();
    let m = n + vec3<i32>(1);
    let p = ts_cell_coord(idx);
    let centre = vec3<f32>(p) + vec3<f32>(0.5);
    let slots = u.capacity;
    var density = 0.0;
    let first = max(p - vec3<i32>(1), vec3<i32>(0));
    let last = min(p + vec3<i32>(1), n - vec3<i32>(1));
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let range = ranges[ts_cell_at(vec3<i32>(x, y, z))];
                let start = min(range.start, slots);
                let end = start + min(range.count, slots - start);
                for (var s = start; s < end; s = s + 1u) {
                    let particle = sorted[s];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let q = (particle.position_radius.xyz - u.box_min) / u.cell_size;
                    // A particle the solid has swept over is removed by this
                    // step's move; the solid's rest sites below already count
                    // that volume, so counting it too reads as crowding.
                    // Outside the one-cell support its contribution is exactly
                    // zero. Keep nonfinite queries on the original path.
                    if finite(q) && any(abs(centre - q) >= vec3<f32>(1.0)) {
                        continue;
                    }
                    if u.body_count > 0 && solid_at(q, n) < 0.0 {
                        continue;
                    }
                    let w = clamp(vec3<f32>(1.0) - abs(centre - q), vec3<f32>(0.0), vec3<f32>(1.0));
                    density = density + w.x * w.y * w.z;
                }
            }
        }
    }
    // The paper's air is a neighbour cell holding no particles: the level-set
    // mask also covers empty cells just above the surface, and their missing
    // particles would otherwise read as a thin surface layer.
    var beside_air = false;
    for (var z = -1; z <= 1; z = z + 1) {
        for (var y = -1; y <= 1; y = y + 1) {
            for (var x = -1; x <= 1; x = x + 1) {
                let d = vec3<i32>(x, y, z);
                let q = p + d;
                // Air is read on the six face neighbours only, as blub's live
                // branch does (density_projection_gather_error.comp:182-184).
                let face = abs(d.x) + abs(d.y) + abs(d.z) == 1;
                if any(q < vec3<i32>(0)) || any(q >= n) {
                    density = density + solid_neighbour_density(d);
                    continue;
                }
                var inside_body = false;
                if u.body_count > 0 {
                    let distance = solid_centre(q, m);
                    inside_body = distance < 0.0;
                    // Bodies are sampled per rest site, so a cell a body only
                    // cuts weighs its solid part; liquid_fill seeds exactly
                    // the sites outside the solid, so the two make the full
                    // lattice at rest.
                    if distance < SOLID_SITE_REACH * u.cell_size {
                        for (var k = 0; k < 8; k = k + 1) {
                            let site = vec3<f32>(q) + vec3<f32>(0.25) + 0.5 * vec3<f32>(vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1));
                            if solid_at(site, n) < 0.0 {
                                let w = clamp(vec3<f32>(1.0) - abs(centre - site), vec3<f32>(0.0), vec3<f32>(1.0));
                                density = density + w.x * w.y * w.z;
                            }
                        }
                    }
                }
                if face && !inside_body && ranges[ts_cell_at(q)].count == 0u {
                    beside_air = true;
                }
            }
        }
    }
    if beside_air {
        density = max(density, REST_DENSITY);
    }
    let error = clamp(density / REST_DENSITY - 1.0, -MAX_DENSITY_ERROR, MAX_DENSITY_ERROR);
    var rate = u.rate;
    if clock_live() {
        rate = 1.0 / adaptive_step_dt();
    }
    cell_out[idx] = -rate * error;
}
