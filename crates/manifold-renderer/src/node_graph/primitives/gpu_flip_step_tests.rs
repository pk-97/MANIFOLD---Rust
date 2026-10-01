//! GPU value proofs for the GPU FLIP water step's particle and face atoms
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)) against CPU f64
//! references.

use super::cells_with_particles::CellsWithParticles;
use super::density_source::DensitySource;
use super::extend_faces::ExtendFaces;
use super::face_divergence::FaceDivergence;
use super::face_gravity::FaceGravity;
use super::faces_to_particles::{FacesToParticles, MAX_SPREAD_CELLS, WALL_MARGIN_CELLS};
use super::liquid_fill::LiquidFill;
use super::liquid_surface_tests::{Harness, params, read};
use super::particles_to_faces::ParticlesToFaces;
use super::subtract_pressure::SubtractPressure;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidParticle};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::KnownItem;
use crate::node_graph::primitive::Primitive;

/// A lattice with unequal sides, so a swapped axis shows.
const N: [usize; 3] = [6, 5, 4];
const H: f32 = 0.25;
const MIN: [f32; 3] = [-0.5, 0.1, 0.3];

struct Stream(u64);

impl Stream {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn signed(&mut self, scale: f32) -> f32 {
        (self.unit() - 0.5) * 2.0 * scale
    }
}

fn padded() -> [usize; 3] {
    N.map(|n| n + 1)
}

fn face_len() -> usize {
    padded().iter().product()
}

fn cell_len() -> usize {
    N.iter().product()
}

fn pad_index(p: [usize; 3]) -> usize {
    let m = padded();
    p[0] + m[0] * (p[1] + m[1] * p[2])
}

fn pad_coords(i: usize) -> [usize; 3] {
    let m = padded();
    [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])]
}

fn cell_index(p: [usize; 3]) -> usize {
    p[0] + N[0] * (p[1] + N[1] * p[2])
}

fn cell_coords(c: usize) -> [usize; 3] {
    [c % N[0], (c / N[0]) % N[1], c / (N[0] * N[1])]
}

/// Face a of padded cell p exists when its other two indices are inside.
fn face_exists(p: [usize; 3], a: usize) -> bool {
    (0..3).all(|b| b == a || p[b] < N[b])
}

fn lattice(extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![
        ("nodes_x", N[0] as f32),
        ("nodes_y", N[1] as f32),
        ("nodes_z", N[2] as f32),
        ("cell_size", H),
        ("lattice_min_x", MIN[0]),
        ("lattice_min_y", MIN[1]),
        ("lattice_min_z", MIN[2]),
    ];
    all.extend_from_slice(extra);
    params(&all)
}

fn run_into<P: Primitive, T: KnownItem + bytemuck::Pod>(
    harness: &mut Harness,
    prim: &mut P,
    inputs: &[(&'static str, crate::node_graph::bindings::Slot)],
    len: usize,
    step_params: &ParamValues,
) -> Vec<T> {
    let out = harness.array::<T>(&[], len);
    let (_, errors) = harness.run(prim, inputs, &[("out", out.0)], step_params);
    assert!(errors.is_empty(), "{errors:?}");
    read(&out.1, len)
}

fn close(got: f32, want: f64, scale: f64, what: &str) {
    assert!((f64::from(got) - want).abs() <= 1e-5 * scale.max(1.0), "{what}: {got} vs {want}");
}

/// Random faces over the padded grid; faces that do not exist are zero.
/// `valid` draws weights of 0 or 1 (a projected grid) instead of positive
/// particle weights.
fn random_faces(seed: u64, valid: bool) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut face = FaceSample::default();
            for a in 0..3 {
                if face_exists(p, a) {
                    face.velocity[a] = rng.signed(2.0);
                    face.weight[a] = if valid { f32::from(u8::from(rng.unit() < 0.6)) } else { rng.unit() };
                }
            }
            face
        })
        .collect()
}

/// node.solid_faces' output: inner faces open (`solid` false), or one in
/// four closed, one in four whole and the rest a fraction, each with a solid
/// velocity; box walls closed.
fn solid_faces(seed: u64, solid: bool) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut face = FaceSample::default();
            for a in 0..3 {
                if face_exists(p, a) && p[a] > 0 && p[a] < N[a] {
                    let r = rng.unit();
                    face.weight[a] = if !solid || (0.25..0.5).contains(&r) { 1.0 } else if r < 0.25 { 0.0 } else { 0.05 + 0.95 * rng.unit() };
                    if solid {
                        face.velocity[a] = rng.signed(1.0);
                    }
                }
            }
            face
        })
        .collect()
}

fn random_water(seed: u64) -> Vec<f32> {
    let mut rng = Stream::new(seed);
    (0..cell_len()).map(|_| f32::from(u8::from(rng.unit() < 0.5))).collect()
}

/// Particles scattered through the box, a few unused slots among them.
fn random_particles(seed: u64, count: usize) -> Vec<FluidParticle> {
    let mut rng = Stream::new(seed);
    (0..count)
        .map(|i| {
            let position: [f32; 3] = std::array::from_fn(|a| MIN[a] + rng.unit() * N[a] as f32 * H);
            let velocity = [rng.signed(1.5), rng.signed(1.5), rng.signed(1.5)];
            let radius = if i % 11 == 5 { 0.0 } else { 0.08 };
            FluidParticle { position_radius: [position[0], position[1], position[2], radius], velocity, id: i as u32 + 1 }
        })
        .collect()
}

/// Counting sort by cell, stable: the contract node.sort_particles_into_cells
/// publishes.
fn cpu_sort(particles: &[FluidParticle]) -> (Vec<FluidParticle>, Vec<CellRange>) {
    let cell_of = |p: &FluidParticle| {
        let c: [usize; 3] = std::array::from_fn(|a| {
            (((p.position_radius[a] - MIN[a]) / H).floor() as i64).clamp(0, N[a] as i64 - 1) as usize
        });
        cell_index(c)
    };
    let mut order: Vec<usize> = (0..particles.len()).collect();
    order.sort_by_key(|&i| cell_of(&particles[i]));
    let sorted: Vec<FluidParticle> = order.iter().map(|&i| particles[i]).collect();
    let mut ranges = vec![CellRange::default(); cell_len()];
    for (s, p) in sorted.iter().enumerate() {
        let r = &mut ranges[cell_of(p)];
        if r.count == 0 {
            r.start = s as u32;
        }
        r.count += 1;
    }
    (sorted, ranges)
}

#[test]
fn gpu_flip_cells_with_particles_marks_occupied_bins() {
    let mut harness = Harness::new();
    let mut rng = Stream::new(0xce11);
    let ranges: Vec<CellRange> =
        (0..cell_len()).map(|c| CellRange { start: c as u32 * 3, count: u32::from(rng.unit() < 0.4) * 3 }).collect();
    let input = harness.array(&ranges, cell_len());
    let got: Vec<f32> = run_into(&mut harness, &mut CellsWithParticles::new(), &[("cell_ranges", input.0)], cell_len(), &lattice(&[]));
    for (c, (g, r)) in got.iter().zip(&ranges).enumerate() {
        assert_eq!(*g, f32::from(u8::from(r.count > 0)), "cell {c}");
    }
}

#[test]
fn gpu_flip_density_source_evens_packing_inside_and_spreads_at_the_surface() {
    let mut harness = Harness::new();
    let mut rng = Stream::new(0xde45);
    // One cell in eight empty, so the draw holds both inside and surface cells.
    let ranges: Vec<CellRange> = (0..cell_len())
        .map(|c| {
            let u = rng.unit();
            CellRange { start: c as u32 * 20, count: if u < 0.125 { 0 } else { 1 + (u * 16.0) as u32 } }
        })
        .collect();
    let inputs = [("cell_ranges", harness.array(&ranges, cell_len()).0)];
    let (rest, rate) = (8.0_f32, 2.5_f32);
    let got: Vec<f32> =
        run_into(&mut harness, &mut DensitySource::new(), &inputs, cell_len(), &lattice(&[("rest", rest), ("rate", rate)]));
    // Walls count as full: a neighbour past the lattice never makes a surface.
    // A neighbour under half full does: a stray particle is not water.
    let full = |p: [usize; 3], a: usize, side: i64| {
        let q = p[a] as i64 + side;
        if q < 0 || q >= N[a] as i64 {
            return true;
        }
        let mut r = p;
        r[a] = q as usize;
        2.0 * ranges[cell_index(r)].count as f32 >= rest
    };
    // Cells seen per (inside, crowded) kind.
    let mut kinds = [[0usize; 2]; 2];
    for (c, g) in got.iter().enumerate() {
        if ranges[c].count == 0 {
            assert_eq!(*g, 0.0, "empty cell {c}");
            continue;
        }
        let p = cell_coords(c);
        let inside = (0..3).all(|a| full(p, a, -1) && full(p, a, 1));
        let crowding = f64::from(ranges[c].count) / f64::from(rest) - 1.0;
        kinds[usize::from(inside)][usize::from(crowding > 0.0)] += 1;
        let source = if inside { crowding } else { crowding.max(0.0) };
        close(*g, -f64::from(rate) * source, 3.0, &format!("cell {c}"));
    }
    assert!(kinds.iter().flatten().all(|&k| k > 3), "the draw covers every inside/surface, crowded/sparse kind: {kinds:?}");
}

#[test]
fn gpu_flip_particles_to_faces_matches_the_tent_sum() {
    let mut harness = Harness::new();
    let particles = random_particles(0x9261, 400);
    let (sorted, ranges) = cpu_sort(&particles);
    let inputs = [
        ("sorted", harness.array(&sorted, sorted.len()).0),
        ("cell_ranges", harness.array(&ranges, ranges.len()).0),
    ];
    let got: Vec<FaceSample> = run_into(&mut harness, &mut ParticlesToFaces::new(), &inputs, face_len(), &lattice(&[]));
    let mut checked = 0;
    // Wall faces held (water moving into the wall) and kept (leaving it).
    let mut walls = [0usize; 2];
    for (i, face) in got.iter().enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            if !face_exists(p, a) {
                assert_eq!((face.velocity[a], face.weight[a]), (0.0, 0.0), "missing face {p:?}/{a}");
                continue;
            }
            let centre: [f64; 3] = std::array::from_fn(|b| p[b] as f64 + if b == a { 0.0 } else { 0.5 });
            let (mut weight, mut momentum) = (0.0f64, 0.0f64);
            for particle in particles.iter().filter(|q| q.position_radius[3] > 0.0) {
                let w: f64 = (0..3)
                    .map(|b| {
                        let q = (f64::from(particle.position_radius[b]) - f64::from(MIN[b])) / f64::from(H);
                        (1.0 - (q - centre[b]).abs()).max(0.0)
                    })
                    .product();
                weight += w;
                momentum += w * f64::from(particle.velocity[a]);
            }
            let velocity = if weight > 0.0 { momentum / weight } else { 0.0 };
            if p[a] == 0 || p[a] == N[a] {
                // A wall keeps only what leaves it, and is always valid.
                close(face.weight[a], 1.0, 1.0, &format!("wall weight {p:?}/{a}"));
                let leaving = if p[a] == 0 { velocity.max(0.0) } else { velocity.min(0.0) };
                walls[usize::from(leaving != 0.0)] += 1;
                if weight > 1e-3 || leaving == 0.0 {
                    close(face.velocity[a], leaving, 1.0, &format!("wall velocity {p:?}/{a}"));
                }
                continue;
            }
            close(face.weight[a], weight, weight, &format!("weight {p:?}/{a}"));
            // A face at the edge of a particle's reach has a tiny weight; its
            // ratio carries the f32 rounding of that weight.
            if weight > 1e-3 {
                close(face.velocity[a], velocity, 1.0, &format!("velocity {p:?}/{a}"));
                checked += 1;
            }
        }
    }
    assert!(checked > 200, "the fixture reaches most faces, got {checked}");
    assert!(walls.iter().all(|&k| k > 10), "the draw covers walls held and kept: {walls:?}");
}

#[test]
fn gpu_flip_face_gravity_adds_gravity_and_holds_the_walls() {
    let mut harness = Harness::new();
    let faces = random_faces(0x96a7, false);
    let input = harness.array(&faces, face_len());
    let (g, dt) = ([0.5f32, -9.81, 1.25], 1.0f32 / 120.0);
    let step = lattice(&[("gravity_x", g[0]), ("gravity_y", g[1]), ("gravity_z", g[2]), ("step_dt", dt)]);
    let got: Vec<FaceSample> = run_into(&mut harness, &mut FaceGravity::new(), &[("faces", input.0)], face_len(), &step);
    for (i, (face, before)) in got.iter().zip(&faces).enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            let pushed = f64::from(before.velocity[a]) + f64::from(g[a]) * f64::from(dt);
            let (velocity, weight) = if !face_exists(p, a) {
                (0.0, 0.0)
            } else if p[a] == 0 {
                (pushed.max(0.0), f64::from(before.weight[a]))
            } else if p[a] == N[a] {
                (pushed.min(0.0), f64::from(before.weight[a]))
            } else {
                (pushed, f64::from(before.weight[a]))
            };
            close(face.velocity[a], velocity, 1.0, &format!("velocity {p:?}/{a}"));
            close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
        }
    }
}

#[test]
fn gpu_flip_face_divergence_is_the_outflow_of_water_cells() {
    let mut harness = Harness::new();
    let faces = random_faces(0xd1f, false);
    let water = random_water(0x3a7e);
    for solid in [false, true] {
        let open = solid_faces(0xd2f, solid);
        let inputs = [
            ("faces", harness.array(&faces, face_len()).0),
            ("water", harness.array(&water, cell_len()).0),
            ("solid_faces", harness.array(&open, face_len()).0),
        ];
        let got: Vec<f32> = run_into(&mut harness, &mut FaceDivergence::new(), &inputs, cell_len(), &lattice(&[]));
        // A wall face counts whole; an inner face by its open fraction.
        let flux = |q: [usize; 3], a: usize| {
            let w = if q[a] == 0 || q[a] == N[a] { 1.0 } else { f64::from(open[pad_index(q)].weight[a]) };
            w * f64::from(faces[pad_index(q)].velocity[a])
        };
        for (c, g) in got.iter().enumerate() {
            let p = cell_coords(c);
            let want = if water[c] > 0.5 {
                (0..3)
                    .map(|a| {
                        let mut q = p;
                        q[a] += 1;
                        flux(q, a) - flux(p, a)
                    })
                    .sum::<f64>()
                    / f64::from(H)
            } else {
                0.0
            };
            close(*g, want, 10.0, &format!("solid {solid} cell {p:?}"));
        }
    }
}

#[test]
fn gpu_flip_subtract_pressure_projects_faces_touching_water() {
    let mut harness = Harness::new();
    let faces = random_faces(0x5b7, false);
    let water = random_water(0xa7e2);
    let mut rng = Stream::new(0x9e55);
    let pressure: Vec<f32> = water.iter().map(|&w| if w > 0.5 { rng.signed(3.0) } else { 0.0 }).collect();
    for solid in [false, true] {
        let open = solid_faces(0x5c7, solid);
        let inputs = [
            ("faces", harness.array(&faces, face_len()).0),
            ("pressure", harness.array(&pressure, cell_len()).0),
            ("water", harness.array(&water, cell_len()).0),
            ("solid_faces", harness.array(&open, face_len()).0),
        ];
        let got: Vec<FaceSample> = run_into(&mut harness, &mut SubtractPressure::new(), &inputs, face_len(), &lattice(&[]));
        let mut closed = 0;
        for (i, face) in got.iter().enumerate() {
            let p = pad_coords(i);
            for a in 0..3 {
                let (velocity, weight) = if !face_exists(p, a) {
                    (0.0, 0.0)
                } else if p[a] == 0 || p[a] == N[a] {
                    (f64::from(faces[i].velocity[a]), 1.0)
                } else {
                    let mut below = p;
                    below[a] -= 1;
                    let (up, down) = (cell_index(p), cell_index(below));
                    let u = f64::from(faces[i].velocity[a]);
                    if open[i].weight[a] <= 0.0 {
                        // A closed face moves with the solid.
                        closed += 1;
                        (f64::from(open[i].velocity[a]), 1.0)
                    } else if water[up] > 0.5 || water[down] > 0.5 {
                        (u - (f64::from(pressure[up]) - f64::from(pressure[down])) / f64::from(H), 1.0)
                    } else {
                        (u, 0.0)
                    }
                };
                close(face.velocity[a], velocity, 30.0, &format!("velocity {p:?}/{a}"));
                close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
            }
        }
        assert_eq!(closed > 20, solid, "the solid fixture closes faces, the open one none: {closed}");
    }
}

fn cpu_extend(faces: &[FaceSample]) -> Vec<FaceSample> {
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut out = FaceSample::default();
            for a in 0..3 {
                if !face_exists(p, a) {
                    continue;
                }
                out.velocity[a] = faces[i].velocity[a];
                out.weight[a] = faces[i].weight[a];
                if faces[i].weight[a] > 0.0 {
                    continue;
                }
                let (mut sum, mut hits) = (0.0f64, 0.0f64);
                for b in 0..3 {
                    for d in [-1i64, 1] {
                        let q = p[b] as i64 + d;
                        let top = if b == a { N[b] as i64 } else { N[b] as i64 - 1 };
                        if q < 0 || q > top {
                            continue;
                        }
                        let mut r = p;
                        r[b] = q as usize;
                        let neighbour = faces[pad_index(r)];
                        if neighbour.weight[a] > 0.0 {
                            sum += f64::from(neighbour.velocity[a]);
                            hits += 1.0;
                        }
                    }
                }
                if hits > 0.0 {
                    out.velocity[a] = (sum / hits) as f32;
                    out.weight[a] = 1.0;
                }
            }
            out
        })
        .collect()
}

#[test]
fn gpu_flip_extend_faces_fills_one_layer() {
    let mut harness = Harness::new();
    let faces = random_faces(0xe7e, true);
    let input = harness.array(&faces, face_len());
    let got: Vec<FaceSample> = run_into(&mut harness, &mut ExtendFaces::new(), &[("faces", input.0)], face_len(), &lattice(&[]));
    let want = cpu_extend(&faces);
    let mut filled = 0;
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        for a in 0..3 {
            close(g.velocity[a], f64::from(w.velocity[a]), 1.0, &format!("velocity {:?}/{a}", pad_coords(i)));
            assert_eq!(g.weight[a], w.weight[a], "weight {:?}/{a}", pad_coords(i));
            filled += usize::from(faces[i].weight[a] == 0.0 && w.weight[a] > 0.0);
        }
    }
    assert!(filled > 20, "the fixture fills faces, got {filled}");
}

/// The kernel's per-component trilinear sample over faces with weight > 0,
/// in f64. q is in cells from the lattice minimum.
fn cpu_sample(q: [f64; 3], field: &[FaceSample]) -> [f64; 3] {
    std::array::from_fn(|a| {
        let top: [i64; 3] = std::array::from_fn(|b| if b == a { N[b] as i64 } else { N[b] as i64 - 1 });
        let s: [f64; 3] = std::array::from_fn(|b| q[b] - if b == a { 0.0 } else { 0.5 });
        let base: [i64; 3] = std::array::from_fn(|b| (s[b].floor() as i64).clamp(0, (top[b] - 1).max(0)));
        let t: [f64; 3] = std::array::from_fn(|b| (s[b] - base[b] as f64).clamp(0.0, 1.0));
        let (mut sum, mut total) = (0.0, 0.0);
        for corner in 0..8 {
            let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let c: [usize; 3] = std::array::from_fn(|b| (base[b] + bit[b] as i64).min(top[b]) as usize);
            let face = field[pad_index(c)];
            if face.weight[a] > 0.0 {
                let w: f64 = (0..3).map(|b| if bit[b] == 1 { t[b] } else { 1.0 - t[b] }).product();
                sum += w * f64::from(face.velocity[a]);
                total += w;
            }
        }
        if total > 1e-6 { sum / total } else { 0.0 }
    })
}

#[test]
fn gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3() {
    let mut harness = Harness::new();
    let faces = random_faces(0xf1a5, true);
    let old = random_faces(0x01d5, false);
    // A third grid: velocity comes from `faces`, the move from `advect`.
    let advect = random_faces(0xad7e, true);
    let mut particles = random_particles(0x2b3, 300);
    // Two particles against the walls, so the clamp is exercised.
    particles[0].position_radius[0] = MIN[0] + 1e-4;
    particles[1].position_radius[1] = MIN[1] + N[1] as f32 * H - 1e-4;
    // Two broken particles: the move must leave them non-finite, never clamp
    // or zero them, so the tick's stats see them.
    particles[2].position_radius[0] = f32::NAN;
    particles[3].velocity[1] = f32::INFINITY;
    // A guard short enough that some RK3 stages hit it and some don't.
    let (dt, flip, max_travel) = (0.07f32, 0.9f32, 0.45f32);
    let inputs = [
        ("particles", harness.array(&particles, particles.len()).0),
        ("faces", harness.array(&faces, face_len()).0),
        ("old", harness.array(&old, face_len()).0),
        ("advect", harness.array(&advect, face_len()).0),
    ];
    let got: Vec<FluidParticle> =
        run_into(&mut harness, &mut FacesToParticles::new(), &inputs, particles.len(), &lattice(&[("step_dt", dt), ("flip", flip), ("max_travel", max_travel)]));
    let per_cell = f64::from(dt) / f64::from(H);
    // Particles whose spread is within the cap, and past it.
    let mut spreads = [0usize; 2];
    // RK3 stages within the CFL guard, and shortened by it.
    let mut stages = [0usize; 2];
    let mut guard = |v: [f64; 3]| {
        let cells = v.iter().map(|c| c * c).sum::<f64>().sqrt() * per_cell;
        let past = cells > f64::from(max_travel);
        stages[usize::from(past)] += 1;
        if past { v.map(|c| c * f64::from(max_travel) / cells) } else { v }
    };
    for (i, (g, p)) in got.iter().zip(&particles).enumerate() {
        if p.position_radius[3] <= 0.0 {
            assert_eq!(g, p, "unused slot {i} passes through");
            continue;
        }
        if i == 2 {
            assert!(!g.position_radius[0].is_finite(), "a non-finite position stays non-finite: {:?}", g.position_radius);
            continue;
        }
        if i == 3 {
            assert!(!g.velocity[1].is_finite(), "a non-finite velocity stays non-finite: {:?}", g.velocity);
            continue;
        }
        let q0: [f64; 3] = std::array::from_fn(|a| (f64::from(p.position_radius[a]) - f64::from(MIN[a])) / f64::from(H));
        let after = cpu_sample(q0, &faces);
        let k1 = guard(after);
        let k2 = guard(cpu_sample(std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]), &faces));
        let k3 = guard(cpu_sample(std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]), &faces));
        let pushed = cpu_sample(q0, &advect);
        let spread: [f64; 3] = std::array::from_fn(|a| per_cell * (pushed[a] - after[a]));
        let length = spread.iter().map(|s| s * s).sum::<f64>().sqrt();
        let scale = if length > MAX_SPREAD_CELLS { MAX_SPREAD_CELLS / length } else { 1.0 };
        spreads[usize::from(scale < 1.0)] += 1;
        let before = cpu_sample(q0, &old);
        for a in 0..3 {
            let q1 = (q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0 + scale * spread[a])
                .clamp(WALL_MARGIN_CELLS, N[a] as f64 - WALL_MARGIN_CELLS);
            let position = f64::from(MIN[a]) + q1 * f64::from(H);
            let velocity =
                f64::from(flip) * (f64::from(p.velocity[a]) + after[a] - before[a]) + (1.0 - f64::from(flip)) * after[a];
            assert!((f64::from(g.position_radius[a]) - position).abs() < 2e-5, "particle {i} position {a}: {} vs {position}", g.position_radius[a]);
            assert!((f64::from(g.velocity[a]) - velocity).abs() < 1e-4, "particle {i} velocity {a}: {} vs {velocity}", g.velocity[a]);
        }
        assert_eq!((g.position_radius[3], g.id), (p.position_radius[3], p.id), "particle {i} keeps radius and id");
    }
    assert!(spreads.iter().all(|&k| k > 10), "the draw covers spreads within and past the cap: {spreads:?}");
    assert!(stages.iter().all(|&k| k > 30), "the draw covers stages within and past the CFL guard: {stages:?}");
}

fn cpu_fill_hash(x: u32) -> u32 {
    let s = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

#[test]
fn gpu_flip_liquid_fill_places_pool_then_box() {
    let mut harness = Harness::new();
    // Sites are half cells: a 12 × 10 × 8 site lattice.
    let (pool, sites, seed, jitter) = (1u32, [[2u32, 8], [0, 4], [3, 9]], 7u32, 0.5f32);
    // 96 pool sites (12 × 1 × 8) and 90 box sites (6 × 3 × 5); the fill owns
    // storage for exactly those.
    let placed = 96 + 90;
    let out = harness.array::<FluidParticle>(&[], 1);
    let count = harness.scalar();
    let step = lattice(&[
        ("pool_sites", pool as f32),
        ("box_x0", sites[0][0] as f32),
        ("box_x1", sites[0][1] as f32),
        ("box_y0", sites[1][0] as f32),
        ("box_y1", sites[1][1] as f32),
        ("box_z0", sites[2][0] as f32),
        ("box_z1", sites[2][1] as f32),
        ("jitter", jitter),
        ("seed", seed as f32),
    ]);
    let mut fill = LiquidFill::new();
    let (scalars, errors) = harness.run(&mut fill, &[], &[("particles", out.0), ("count", count)], &step);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(scalars.iter().any(|(s, v)| *s == count && *v == ParamValue::Float(placed as f32)), "{scalars:?}");
    let storage = harness.buffer(out.0);
    assert_eq!(storage.size, (placed * std::mem::size_of::<FluidParticle>()) as u64, "the fill's storage holds the fill");
    let got: Vec<FluidParticle> = read(&storage, placed);
    // The box clipped above the pool and to the lattice: x 2..8, y 1..4, z 3..8.
    let mut cells: Vec<[u32; 3]> = Vec::new();
    for z in 0..2 * N[2] as u32 {
        for x in 0..2 * N[0] as u32 {
            cells.push([x, 0, z]);
        }
    }
    for z in 3..8 {
        for y in 1..4 {
            for x in 2..8 {
                cells.push([x, y, z]);
            }
        }
    }
    assert_eq!(cells.len(), placed);
    for (i, g) in got.iter().enumerate() {
        let s = cells[i];
        let key = (i as u32).wrapping_mul(3).wrapping_add(seed.wrapping_mul(2_654_435_761));
        for a in 0..3 {
            let unit = (cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) as f32 / 16_777_216.0;
            let local = 0.25 + 0.5 * s[a] as f32 + 0.5 * jitter * (unit - 0.5);
            let want = MIN[a] + local * H;
            assert!((g.position_radius[a] - want).abs() < 1e-5, "particle {i} axis {a}: {} vs {want}", g.position_radius[a]);
        }
        assert!((g.position_radius[3] - 0.31017 * H).abs() < 1e-6);
        assert_eq!((g.velocity, g.id), ([0.0; 3], i as u32 + 1), "particle {i}");
    }
}
