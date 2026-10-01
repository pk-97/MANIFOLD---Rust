// node.faces_to_particles — fusable BUFFER body, GATHER. One thread per
// particle (FluidParticle → Element). A live particle (radius > 0) at q, in
// cells from the lattice minimum, samples each face grid trilinearly per
// component over the faces with weight > 0, renormalised by their weights
// (0 when none). Its new velocity blends FLIP and PIC:
// flip · (v + new(q) − old(q)) + (1 − flip) · new(q). It then moves by RK3
// through `faces` (stages at ½ and ¾ of step_dt, weights 2/9, 3/9, 4/9, each
// stage at most max_travel cells long), plus
// the spread step_dt · (advect(q) − faces(q)), at most
// FACES_TO_PARTICLES_MAX_SPREAD cells long, and is kept
// FACES_TO_PARTICLES_WALL_MARGIN cells inside each wall. Radius and id are
// kept; unused slots pass through. A non-finite move or velocity is written
// as it is, never clamped or zeroed: the tick's node.liquid_stats must see it
// to halt the liquid (LIQUID_SOLVER_SEAM_DESIGN.md D4 amendment 2). Every
// index is clamped as an integer, so a non-finite position reads in bounds.
// `faces`, `old` and `advect` (FaceSample → Element2) are gathered
// through buf_faces, buf_old and buf_advect; a grid shorter than the
// lattice's leaves particles as they were.

// A moved particle stays 0.2 cells inside each box wall, as the FLIP Fluids
// engine keeps its particles off its solids (`_solidBufferWidth`). The wall
// faces themselves carry only velocity leaving the wall, in `faces` and
// `old` alike, so a particle on a wall leaves at the water's speed and the
// FLIP change there is the step's own. Mirrored by
// `faces_to_particles::WALL_MARGIN_CELLS`.
const FACES_TO_PARTICLES_WALL_MARGIN: f32 = 0.2;

// `advect` is `faces` plus a density solve's correction. A correction
// longer than half a cell carries a particle past the cell it was spreading
// from and crowds the next one. Mirrored by `faces_to_particles::MAX_SPREAD_CELLS`.
const FACES_TO_PARTICLES_MAX_SPREAD: f32 = 0.5;

// The CFL guard: one RK3 stage moves at most max_travel cells. The faces are
// extended far enough for that travel, so every stage samples valid faces;
// water faster than the step was built for keeps its speed and moves only
// max_travel cells this step. A non-finite v stays non-finite.
fn faces_to_particles_guard(v: vec3<f32>, per_cell: f32, max_travel: f32) -> vec3<f32> {
    let cells = length(v) * per_cell;
    return select(v, v * (max_travel / cells), cells > max_travel);
}

// Exponent bits, not x != x: fast math may fold a NaN comparison away.
fn faces_to_particles_finite(v: vec3<f32>) -> bool {
    let bits = bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u);
    return all(bits != vec3<u32>(0x7f800000u));
}

// grid: 0 `faces`, 1 `old`, 2 `advect`.
fn faces_to_particles_face(index: u32, grid: u32) -> Element2 {
    if grid == 0u {
        return buf_faces[index];
    }
    if grid == 1u {
        return buf_old[index];
    }
    return buf_advect[index];
}

fn faces_to_particles_sample(q: vec3<f32>, n: vec3<i32>, grid: u32) -> vec3<f32> {
    let m = n + vec3<i32>(1);
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var offset = vec3<f32>(0.5);
        offset[a] = 0.0;
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        let s = q - offset;
        let base = clamp(vec3<i32>(floor(s)), vec3<i32>(0), max(top - vec3<i32>(1), vec3<i32>(0)));
        let t = clamp(s - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
        var sum = 0.0;
        var total = 0.0;
        for (var corner = 0; corner < 8; corner = corner + 1) {
            let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
            let c = min(base + bit, top);
            let face = faces_to_particles_face(u32(c.x + m.x * (c.y + m.y * c.z)), grid);
            if face.face_weight[a] > 0.0 {
                let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
                let w = w3.x * w3.y * w3.z;
                sum = sum + w * face.face_velocity[a];
                total = total + w;
            }
        }
        v[a] = select(0.0, sum / max(total, 1e-30), total > 1e-6);
    }
    return v;
}

fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    step_dt: f32,
    flip: f32,
    max_travel: f32,
) -> Element {
    var out = e_particles;
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let padded = u32(m.x) * u32(m.y) * u32(m.z);
    let shortest = min(min(arrayLength(&buf_faces), arrayLength(&buf_old)), arrayLength(&buf_advect));
    if !(e_particles.position_radius.w > 0.0) || padded > shortest {
        return out;
    }
    let lo = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let per_cell = step_dt / cell_size;
    let q0 = (e_particles.position_radius.xyz - lo) / cell_size;
    let after = faces_to_particles_sample(q0, n, 0u);
    let k1 = faces_to_particles_guard(after, per_cell, max_travel);
    let k2 = faces_to_particles_guard(faces_to_particles_sample(q0 + 0.5 * per_cell * k1, n, 0u), per_cell, max_travel);
    let k3 = faces_to_particles_guard(faces_to_particles_sample(q0 + 0.75 * per_cell * k2, n, 0u), per_cell, max_travel);
    let spread = per_cell * (faces_to_particles_sample(q0, n, 2u) - after);
    let spread_cells = length(spread);
    let capped = select(spread, spread * (FACES_TO_PARTICLES_MAX_SPREAD / spread_cells), spread_cells > FACES_TO_PARTICLES_MAX_SPREAD);
    let edge = vec3<f32>(FACES_TO_PARTICLES_WALL_MARGIN);
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0 + capped;
    let q1 = select(reached, clamp(reached, edge, vec3<f32>(n) - edge), faces_to_particles_finite(reached));
    let before = faces_to_particles_sample(q0, n, 1u);
    let velocity = flip * (e_particles.velocity + after - before) + (1.0 - flip) * after;
    out.position_radius = vec4<f32>(lo + q1 * cell_size, e_particles.position_radius.w);
    out.velocity = velocity;
    return out;
}
