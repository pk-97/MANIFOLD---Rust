// The move works in cells counted from the particle's own world cell `lo`:
// q = (x − origin) / h − lo, so a particle starts at q in [0, 1) whatever its
// distance from the world origin, and every lookup reads world cell c + lo
// through the pooled tables. A lookup past the held tiles reads the canonical
// element: faces absent, the solid clear by three cells. There is no
// boundary: a wall is a solid like a body.

// Exponent bits, not x != x: fast math may fold a NaN comparison away.
fn finite(v: vec3<f32>) -> bool {
    let bits = bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u);
    return all(bits != vec3<u32>(0x7f800000u));
}

// grid: 0 the new faces, 1 `old`, 2 `spread`.
fn face_record(index: u32, grid: u32) -> FaceSample {
    if grid == 0u {
        return faces_in[index];
    }
    if grid == 1u {
        return old[index];
    }
    return spread[index];
}

// Native FLIP's ordinary face interpolation (macvelocityfield.cpp
// _interpolateLinearU/V/W): missing corners contribute zero, without a
// validity-weight renormalisation; a corner no tile holds is a missing one.
// Non-finite positions fail to a zero sample.
fn sample(q: vec3<f32>, lo: vec3<i32>, grid: u32) -> vec3<f32> {
    if !finite(q) {
        return vec3<f32>(0.0);
    }
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var offset = vec3<f32>(0.5);
        offset[a] = 0.0;
        let s = q - offset;
        let base = vec3<i32>(floor(s));
        let t = s - vec3<f32>(base);
        var sum = 0.0;
        for (var corner = 0; corner < 8; corner = corner + 1) {
            let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
            let face = face_record(ts_face(base + bit + lo), grid);
            let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
            let w = w3.x * w3.y * w3.z;
            sum = sum + w * face.face_velocity[a];
        }
        v[a] = sum;
    }
    return v;
}

// The march along a particle's move, in cells
// (_markerParticleStepDistanceFactor).
const SOLID_STEP: f32 = 0.1;

// A particle pushed out of a solid lands this many cells outside it
// (_solidBufferWidth).
const SOLID_BUFFER: f32 = 0.2;

// The farthest a push-out moves a particle, in cells (_CFLConditionNumber).
const SOLID_PUSH: f32 = 5.0;

// A bound on the solid distance's change per cell of travel: each axis of
// the trilinear distance changes at most one cell per cell.
const SOLID_SLOPE: f32 = 1.7320508;

// Corner c of the corner lattice, counted from world cell `lo`.
fn solid_corner(c: vec3<i32>, lo: vec3<i32>) -> f32 {
    return solid[ts_cell_at(c + lo)];
}

// The solid distance in cells at q (cells from `lo`), trilinear over the
// corner lattice (MeshLevelSet::trilinearInterpolate).
fn solid_at(q: vec3<f32>, lo: vec3<i32>) -> f32 {
    let base = vec3<i32>(floor(q));
    let t = q - vec3<f32>(base);
    var v = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
        let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        v = v + w3.x * w3.y * w3.z * solid_corner(base + bit, lo);
    }
    return v / u.cell_size;
}

// The trilinear distance's gradient at q (Interpolation::
// trilinearInterpolateGradient): each axis's edge differences, blended
// bilinearly over the other two.
fn solid_gradient(q: vec3<f32>, lo: vec3<i32>) -> vec3<f32> {
    let base = vec3<i32>(floor(q));
    let t = q - vec3<f32>(base);
    var g = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        let axes = cross_axes(a);
        var e = vec3<i32>(0);
        e[a] = 1;
        for (var k = 0; k < 4; k = k + 1) {
            var c = base;
            c[axes.x] = c[axes.x] + (k & 1);
            c[axes.y] = c[axes.y] + ((k >> 1u) & 1);
            let wx = select(1.0 - t[axes.x], t[axes.x], (k & 1) == 1);
            let wy = select(1.0 - t[axes.y], t[axes.y], ((k >> 1u) & 1) == 1);
            g[a] = g[a] + wx * wy * (solid_corner(c + e, lo) - solid_corner(c, lo));
        }
    }
    return g;
}

// A move from q0 to q1 (cells from `lo`) kept out of the solids
// (FluidSimulation::_resolveCollision): march in SOLID_STEP cells; at the
// first sample inside a solid, push it out along the distance's gradient to
// SOLID_BUFFER cells outside; a push that fails keeps the last free sample.
// The conservative SDF skip applies when both endpoints are clear of every
// solid by the travel.
fn resolve_solid(q0: vec3<f32>, q1: vec3<f32>, lo: vec3<i32>) -> vec3<f32> {
    let travel = length(q1 - q0);
    if min(solid_at(q0, lo), solid_at(q1, lo)) > SOLID_SLOPE * travel + SOLID_STEP {
        return q1;
    }
    if travel * u.cell_size < 1e-6 {
        return q1;
    }
    let steps = i32(ceil(travel / SOLID_STEP));
    let dir = (q1 - q0) / travel;
    var last = q0;
    for (var s = 0; s < steps; s = s + 1) {
        let current = select(q0 + f32(s + 1) * SOLID_STEP * dir, q1, s == steps - 1);
        let d = solid_at(current, lo);
        if d < 0.0 {
            let g = solid_gradient(current, lo);
            var kept = last;
            if length(g) > 1e-6 {
                let pushed = current - (d - SOLID_BUFFER) * normalize(g);
                if length(pushed - current) > SOLID_PUSH {
                    push_refused = 1u;
                } else if solid_at(pushed, lo) >= 0.0 {
                    kept = pushed;
                }
            }
            return kept;
        }
        last = current;
    }
    return q1;
}

// Water is never deleted at a solid. A
// particle a move leaves inside one (a body swept over it, or it sat still
// under a body) climbs the distance's gradient to SOLID_BUFFER cells
// outside, the engine's own push-out (FluidSimulation::
// _resolveSolidLevelSetUpdateCollisionsThread, written and then disabled
// upstream): at most PUSH_OUT_ROUNDS steps, no further than SOLID_PUSH cells
// in all. A particle with no way out stays where it is and counts as a
// refused push-out.
const PUSH_OUT_ROUNDS: i32 = 3;

fn push_out(q: vec3<f32>, lo: vec3<i32>) -> vec3<f32> {
    var p = q;
    for (var round = 0; round < PUSH_OUT_ROUNDS; round = round + 1) {
        let d = solid_at(p, lo);
        if d >= 0.0 {
            break;
        }
        let g = solid_gradient(p, lo);
        if !(length(g) > 1e-6) {
            break;
        }
        p = p - (d - SOLID_BUFFER) * normalize(g);
    }
    if solid_at(p, lo) >= 0.0 && length(p - q) <= SOLID_PUSH {
        return p;
    }
    push_refused = 1u;
    return q;
}

// Whether x is beyond the plane of an Open face of the Bounds, on the side
// the face opens to. Beyond it is air: the water that ends a move there is
// removed, as an outflow removes it, and the solve puts a free surface in the
// emptied cells.
fn beyond_open(x: vec3<f32>) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        if (u.open_faces & (1u << u32(2 * a))) != 0u && x[a] < u.bounds_lo[a] {
            return true;
        }
        if (u.open_faces & (1u << u32(2 * a + 1))) != 0u && x[a] > u.bounds_hi[a] {
            return true;
        }
    }
    return false;
}

// One thread per slot in use (`slot_record`), `sorted` to `particles_out`. A
// live particle (radius > 0) whose own tile has no slot (an exhausted pool;
// `tiles_tally` counts them) is removed. Any other at q blends FLIP and PIC, flip · (v + new(q) − old(q)) +
// (1 − flip) · new(q), then moves by RK3 through the new faces (stages at ½
// and ¾ of step_dt, weights 2/9, 3/9, 4/9), plus the density
// projection's move. The solid collision sequence is shared by wall-only and
// body scenes; a particle still inside a solid, where a moving solid swept
// over it, is pushed out (push_out), never removed. Then, with the move
// resolved, the particle is removed if it ends beyond the plane of an Open
// face or below the kill height (f32::MIN: none). A particle beyond a Wall
// and an Open face has been pushed to the Wall first, so it is removed only
// if it is still beyond the Open plane. A non-finite move or velocity
// is written as it is: the tick's stats must see it to halt the liquid.
// Radius and id are kept otherwise; a dead slot below the count passes through.
@compute @workgroup_size(256)
fn faces_to_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= slots_in_use() {
        return;
    }
    let particle = sorted[idx];
    var out = particle;
    let first = u.step_in_tick == 0;
    let push_before = select(capped[2u * idx + 1u], 0u, first);
    // Native RK3 never clips stage velocities; retain the public stats word.
    capped[2u * idx] = 0u;
    capped[2u * idx + 1u] = push_before;
    if !(particle.position_radius.w > 0.0) {
        particles_out[idx] = out;
        return;
    }
    let x0 = particle.position_radius.xyz;
    let lo = world_cell(x0);
    let own = ts_cell_at(lo);
    ts_home(own);
    if finite(x0) && own == ts_canon() {
        out.position_radius.w = 0.0;
        particles_out[idx] = out;
        return;
    }
    let per_cell = adaptive_step_dt() / u.cell_size;
    let q0 = (x0 - u.origin) * u.inv_cell - vec3<f32>(lo);
    let after = sample(q0, lo, 0u);
    let k1 = after;
    let k2 = sample(q0 + 0.5 * per_cell * k1, lo, 0u);
    let k3 = sample(q0 + 0.75 * per_cell * k2, lo, 0u);
    // The density projection's move, step_dt · (spread(q) − new(q)): position
    // only, never kept as velocity. Zero rate binds `faces_in` as `spread`.
    let moved = per_cell * (sample(q0, lo, 2u) - after);
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0 + moved;
    var q1 = reached;
    var radius = particle.position_radius.w;
    if finite(q1) {
        q1 = resolve_solid(q0, q1, lo);
        if solid_at(q1, lo) < 0.0 {
            q1 = push_out(q1, lo);
        }
        capped[2u * idx + 1u] = push_before + push_refused;
    }
    // The end position in metres, offset from the start so a particle that
    // stays still keeps its bits whatever its distance from the origin.
    let x1 = x0 + (q1 - q0) * u.cell_size;
    if finite(q1) && (beyond_open(x1) || x1.y < u.kill_height) {
        radius = 0.0;
    }
    let before = sample(q0, lo, 1u);
    out.velocity = u.flip * (particle.velocity + after - before) + (1.0 - u.flip) * after;
    if u.region_count > 0 {
        // An inflow sets the velocity of the water it holds
        // (_constrainMarkerParticleVelocities); an outflow removes the water
        // that ends the move inside it.
        let inflow = region_holding(x0, 2.0, true);
        if inflow >= 0 {
            out.velocity = region_velocity(inflow, x0);
        }
        if region_holding(x1, 3.0, false) >= 0 {
            radius = 0.0;
        }
    }
    out.position_radius = vec4<f32>(x1, radius);
    particles_out[idx] = out;
}
