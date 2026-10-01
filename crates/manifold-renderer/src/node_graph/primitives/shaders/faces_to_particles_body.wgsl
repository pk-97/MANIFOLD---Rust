// node.faces_to_particles — fusable BUFFER body, GATHER. One thread per
// particle (FluidParticle → Element). A live particle (radius > 0) at q, in
// cells from the lattice minimum, samples each face grid trilinearly per
// component over the faces with weight > 0, renormalised by their weights
// (0 when none). Its new velocity blends FLIP and PIC:
// flip · (v + new(q) − old(q)) + (1 − flip) · new(q). It then moves by RK3
// through `advect` (stages at ½ and ¾ of step_dt, weights 2/9, 3/9, 4/9) and
// is kept FACES_TO_PARTICLES_WALL_MARGIN cells inside each wall. Radius and id are
// kept; unused slots pass through. A non-finite move or velocity is written
// as it is, never clamped or zeroed: the tick's node.liquid_stats must see it
// to halt the liquid (LIQUID_SOLVER_SEAM_DESIGN.md D4 amendment 2). Every
// index is clamped as an integer, so a non-finite position reads in bounds.
// `faces`, `old` and `advect` (FaceSample → Element2) are gathered
// through buf_faces, buf_old and buf_advect; a grid shorter than the
// lattice's leaves particles as they were.

// The box walls sit on faces whose velocity is 0, so the grid's velocity
// into a wall falls linearly to 0 across the last cell, and a particle d
// cells off a wall moves away at d times the next face's speed. Held 0.001
// cells off, water that hits the lid needs about 0.4 s at 1 m/s to get one
// cell clear, so it hangs there; from 0.2 cells it takes about 0.1 s. The
// FLIP Fluids engine keeps particles the same 0.2 cells off its solids
// (`_solidBufferWidth`). Mirrored by `faces_to_particles::WALL_MARGIN_CELLS`.
const FACES_TO_PARTICLES_WALL_MARGIN: f32 = 0.2;

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

fn faces_to_particles_sample(q: vec3<f32>, n: vec3<i32>, grid: u32, skip_mode: u32) -> vec3<f32> {
    let skip_lid = (skip_mode & 1u) != 0u;
    let skip_walls = (skip_mode & 4u) != 0u;
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
            if face.face_weight[a] > 0.0 && !(skip_lid && a == 1 && c.y == n.y) && !(skip_walls && (c[a] == 0 || c[a] == n[a])) {
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
    flip_in: f32,
) -> Element {
    var out = e_particles;
    let mode = u32(floor(flip_in));
    let flip = flip_in - f32(mode);
    let skip_lid = mode;
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
    let k1 = faces_to_particles_sample(q0, n, 2u, skip_lid);
    let k2 = faces_to_particles_sample(q0 + 0.5 * per_cell * k1, n, 2u, skip_lid);
    let k3 = faces_to_particles_sample(q0 + 0.75 * per_cell * k2, n, 2u, skip_lid);
    let edge = vec3<f32>(select(FACES_TO_PARTICLES_WALL_MARGIN, 0.001, (mode & 2u) != 0u));
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0;
    let q1 = select(reached, clamp(reached, edge, vec3<f32>(n) - edge), faces_to_particles_finite(reached));
    let after = faces_to_particles_sample(q0, n, 0u, skip_lid);
    let before = faces_to_particles_sample(q0, n, 1u, skip_lid);
    let velocity = flip * (e_particles.velocity + after - before) + (1.0 - flip) * after;
    out.position_radius = vec4<f32>(lo + q1 * cell_size, e_particles.position_radius.w);
    out.velocity = velocity;
    return out;
}
