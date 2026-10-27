// Native shrinks its grid by 3h + 1e-4m, then by 0.2h. AABB::expand moves
// each side by half its argument, so the boundary sits 0.1h inside the
// walls: wall_inset + 0.1 cells from the grid's minimum, the 1e-4m inside
// wall_inset. The push target remains the native 0.2h solid buffer.
const BOUNDARY_MARGIN: f32 = 0.1;

fn boundary_edge() -> f32 {
    return u.wall_inset + BOUNDARY_MARGIN;
}

fn inside_boundary(q: vec3<f32>, n: vec3<i32>, edge: f32) -> bool {
    return all(q >= vec3<f32>(edge)) && all(q < vec3<f32>(n) - vec3<f32>(edge));
}

// Native's getNearestPointInsideAABB is called only for an outside point.
// The lower side is inclusive; the upper side is exclusive and gets the
// extra 1e-6m subtraction. Keep that epsilon out of ordinary moves.
fn clamp_boundary(q: vec3<f32>, n: vec3<i32>, edge: f32) -> vec3<f32> {
    let low = vec3<f32>(edge);
    let high = vec3<f32>(n) - vec3<f32>(edge);
    let upper_epsilon = vec3<f32>(1e-6 / u.cell_size);
    return min(max(q, low), high - upper_epsilon);
}

// Native's grid index range (Grid3d::isGridIndexInRange): the cells
// themselves, walls and padding included.
fn inside_grid(q: vec3<f32>, n: vec3<i32>) -> bool {
    return all(q >= vec3<f32>(0.0)) && all(q < vec3<f32>(n));
}

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
// validity-weight renormalisation. Grid3d::isPositionInGrid rejects samples
// outside the exclusive high edge; retain non-finite positions as failures.
fn sample(q: vec3<f32>, n: vec3<i32>, grid: u32) -> vec3<f32> {
    if !finite(q) || any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(n)) {
        return vec3<f32>(0.0);
    }
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var offset = vec3<f32>(0.5);
        offset[a] = 0.0;
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        let s = q - offset;
        let base = vec3<i32>(floor(s));
        let t = s - vec3<f32>(base);
        var sum = 0.0;
        for (var corner = 0; corner < 8; corner = corner + 1) {
            let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
            let c = base + bit;
            if all(c >= vec3<i32>(0)) && all(c <= top) {
                let face = face_record(ts_face(c), grid);
                let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
                let w = w3.x * w3.y * w3.z;
                sum = sum + w * face.face_velocity[a];
            }
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

fn solid_corner(c: vec3<i32>, n: vec3<i32>) -> f32 {
    return solid[ts_corner(clamp(c, vec3<i32>(0), n))];
}

// The solid distance in cells at q (cells from the box minimum), trilinear
// over the corner lattice (MeshLevelSet::trilinearInterpolate).
fn solid_at(q: vec3<f32>, n: vec3<i32>) -> f32 {
    let base = clamp(vec3<i32>(floor(q)), vec3<i32>(0), max(n - vec3<i32>(1), vec3<i32>(0)));
    let t = clamp(q - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
    var v = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
        let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        v = v + w3.x * w3.y * w3.z * solid_corner(base + bit, n);
    }
    return v / u.cell_size;
}

// The trilinear distance's gradient at q (Interpolation::
// trilinearInterpolateGradient): each axis's edge differences, blended
// bilinearly over the other two.
fn solid_gradient(q: vec3<f32>, n: vec3<i32>) -> vec3<f32> {
    let base = clamp(vec3<i32>(floor(q)), vec3<i32>(0), max(n - vec3<i32>(1), vec3<i32>(0)));
    let t = clamp(q - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
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
            g[a] = g[a] + wx * wy * (solid_corner(c + e, n) - solid_corner(c, n));
        }
    }
    return g;
}

// A move from q0 to q1 (cells) kept out of the solids and native safety AABB
// (FluidSimulation::_resolveCollision): march in SOLID_STEP cells; at the
// first sample inside a solid or outside the boundary, push it out along the
// distance's gradient to SOLID_BUFFER cells outside, then apply the boundary
// clamp and fallback in native order. The conservative SDF skip is used only
// when both endpoints are inside the safety AABB.
fn resolve_solid(q0: vec3<f32>, q1: vec3<f32>, n: vec3<i32>, edge: vec3<f32>) -> vec3<f32> {
    let travel = length(q1 - q0);
    let inside0 = inside_boundary(q0, n, edge.x);
    let inside1 = inside_boundary(q1, n, edge.x);
    if inside0 && inside1 && min(solid_at(q0, n), solid_at(q1, n)) > SOLID_SLOPE * travel + SOLID_STEP {
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
        let d = solid_at(current, n);
        if d < 0.0 || !inside_boundary(current, n, edge.x) {
            let g = solid_gradient(current, n);
            var kept = last;
            if length(g) > 1e-6 {
                let pushed = current - (d - SOLID_BUFFER) * normalize(g);
                if length(pushed - current) > SOLID_PUSH {
                    push_refused = 1u;
                } else if solid_at(pushed, n) >= 0.0 {
                    kept = pushed;
                }
            }
            if !inside_boundary(kept, n, edge.x) {
                let original = kept;
                kept = clamp_boundary(kept, n, edge.x);
                if solid_at(kept, n) < 0.0 || length(kept - original) > SOLID_PUSH {
                    if length(kept - original) > SOLID_PUSH {
                        push_refused = 1u;
                    }
                    return last;
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
// upstream): at most PUSH_OUT_ROUNDS steps, each brought back inside the
// walls, no further than SOLID_PUSH cells in all. A particle with no way
// out stays where it is and counts as a refused push-out.
const PUSH_OUT_ROUNDS: i32 = 3;

fn push_out(q: vec3<f32>, n: vec3<i32>, edge: vec3<f32>) -> vec3<f32> {
    var p = q;
    for (var round = 0; round < PUSH_OUT_ROUNDS; round = round + 1) {
        let d = solid_at(p, n);
        if d >= 0.0 {
            break;
        }
        let g = solid_gradient(p, n);
        if !(length(g) > 1e-6) {
            break;
        }
        p = p - (d - SOLID_BUFFER) * normalize(g);
        if !inside_boundary(p, n, edge.x) {
            p = clamp_boundary(p, n, edge.x);
        }
    }
    if solid_at(p, n) >= 0.0 && length(p - q) <= SOLID_PUSH {
        return p;
    }
    push_refused = 1u;
    return q;
}

// An open face is a sink, as the engine's is: every wall stays solid, and a
// particle within OPEN_BOUNDARY_WIDTH cells of an open face is removed
// (FluidSimulation::_openBoundaryWidth, _removeMarkerParticles). The emptied
// band is air, so the pressure solve puts the water's surface there.
// The band is measured from the wall (the engine's boundary AABB), not the
// grid's edge. On the native grid every wall stays solid, so an open face
// never seeds a pocket's air: only the band's emptied cells do.
const OPEN_BOUNDARY_WIDTH: f32 = 2.0;

fn open_band(q: vec3<f32>, n: vec3<i32>) -> bool {
    let reach = u.wall_inset + OPEN_BOUNDARY_WIDTH;
    for (var a = 0; a < 3; a = a + 1) {
        let low = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if (low && q[a] < reach) || (high && q[a] > f32(n[a]) - reach) {
            return true;
        }
    }
    return false;
}

// One thread per slot in use (`slot_record`), `sorted` to `particles_out`. A live particle
// (radius > 0) at q blends FLIP and PIC, flip · (v + new(q) − old(q)) +
// (1 − flip) · new(q), then moves by RK3 through the new faces (stages at ½
// and ¾ of step_dt, weights 2/9, 3/9, 4/9), plus the density
// projection's move. The native safety AABB and solid collision sequence is
// shared by wall-only and body scenes; a particle still inside one, where a
// moving solid swept over it, is pushed out (push_out), never removed. A
// non-finite move or velocity
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
    let n = ts_cells();
    let lo = u.box_min;
    let per_cell = adaptive_step_dt() / u.cell_size;
    let q0 = (particle.position_radius.xyz - lo) / u.cell_size;
    let after = sample(q0, n, 0u);
    let k1 = after;
    let k2 = sample(q0 + 0.5 * per_cell * k1, n, 0u);
    let k3 = sample(q0 + 0.75 * per_cell * k2, n, 0u);
    let edge = vec3<f32>(boundary_edge());
    // The density projection's move, step_dt · (spread(q) − new(q)): position
    // only, never kept as velocity. Zero rate binds `faces_in` as `spread`.
    let moved = per_cell * (sample(q0, n, 2u) - after);
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0 + moved;
    var q1 = reached;
    var radius = particle.position_radius.w;
    if finite(q1) {
        if !inside_grid(q1, n) {
            q1 = clamp_boundary(q1, n, edge.x);
        }
        q1 = resolve_solid(q0, q1, n, edge);
        if solid_at(q1, n) < 0.0 {
            q1 = push_out(q1, n, edge);
        }
        capped[2u * idx + 1u] = push_before + push_refused;
    }
    if open_band(q1, n) {
        radius = 0.0;
    }
    let before = sample(q0, n, 1u);
    out.velocity = u.flip * (particle.velocity + after - before) + (1.0 - u.flip) * after;
    if u.region_count > 0 {
        // An inflow sets the velocity of the water it holds
        // (_constrainMarkerParticleVelocities); an outflow removes the water
        // that ends the move inside it.
        let x0 = particle.position_radius.xyz;
        let inflow = region_holding(x0, 2.0, true);
        if inflow >= 0 {
            out.velocity = region_velocity(inflow, x0);
        }
        if region_holding(lo + q1 * u.cell_size, 3.0, false) >= 0 {
            radius = 0.0;
        }
    }
    out.position_radius = vec4<f32>(lo + q1 * u.cell_size, radius);
    particles_out[idx] = out;
}
