//! GPU value proofs for node.gpu_flip_step's particle, face and solid passes
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)), each entry of the
//! step's shader run on its own against a CPU f64 reference.
//!
//! The CPU references port FLIP Fluids rules (MIT, Copyright (C) 2026 Ryan L.
//! Guy & Dennis Fassbaender; see THIRD_PARTY_NOTICES.md): velocityadvector.cpp
//! (the Wyvill sum), particlelevelset.cpp (the particle distance),
//! levelsetutils.cpp and meshlevelset.cpp (open fractions),
//! fluidsimulation.cpp (solid face velocity, the constraint),
//! pressuresolver.cpp (the pressure subtraction).

use manifold_gpu::GpuBuffer;

use super::gpu_flip_atom_tests::{FACE_FLOATS, assert_close, face_grid_len, random_values};
use super::gpu_flip_step::{StepParams, dispatch_pass};
use super::liquid_fill::LiquidFill;
use super::liquid_surface_tests::{Harness, params, read};
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidParticle};
use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape, body_pose_at, pack_distance_atlas};
use crate::node_graph::liquid::fields::FieldLattice;
use crate::node_graph::liquid::lattice::PADDING_NODES;
use crate::node_graph::parameters::ParamValue;

/// A lattice with unequal sides, so a swapped axis shows.
const N: [usize; 3] = [6, 5, 4];
const H: f32 = 0.25;
const MIN: [f32; 3] = [-0.5, 0.1, 0.3];

/// The move's wall cap in cells: the shader's WALL_MARGIN.
const WALL_MARGIN_CELLS: f64 = 0.2;

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

/// The step's params over this file's lattice, no impulse tick, every tank
/// face closed.
fn lattice() -> StepParams {
    StepParams { n: N.map(|n| n as u32), box_min: MIN, cell_size: H, impulse_tick: -1, closed_faces: 63, ..StepParams::default() }
}

/// One entry of the step's shader with its buffers bound by number.
struct Pass {
    device: crate::TestDevice,
    bound: Vec<(u32, GpuBuffer)>,
}

impl Pass {
    fn new() -> Self {
        Self { device: crate::test_device(), bound: Vec::new() }
    }

    /// Binds a buffer holding `values` at `binding`.
    fn bind<T: bytemuck::Pod>(&mut self, binding: u32, values: &[T]) -> &mut Self {
        let buffer = self.device.create_buffer_shared((size_of_val(values) as u64).max(16));
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: shared buffer sized for `values`; no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        self.bound.push((binding, buffer));
        self
    }

    /// Runs `entry` over `threads` threads and reads `len` records of
    /// `out`: the bound buffer for a pass in place, else a zeroed one.
    fn run<T: bytemuck::Pod>(&mut self, entry: &str, params: &StepParams, out: u32, len: usize, threads: usize) -> Vec<T> {
        if !self.bound.iter().any(|(binding, _)| *binding == out) {
            self.bind(out, &vec![T::zeroed(); len]);
        }
        let buffers: Vec<(u32, &GpuBuffer)> = self.bound.iter().map(|(binding, buffer)| (*binding, buffer)).collect();
        dispatch_pass(&self.device, entry, params, &buffers, threads as u64);
        let buffer = &self.bound.iter().find(|(binding, _)| *binding == out).expect("bound").1;
        read(buffer, len)
    }

    /// `len` records of the buffer bound at `binding`.
    fn bound<T: bytemuck::Pod>(&self, binding: u32, len: usize) -> Vec<T> {
        read(&self.bound.iter().find(|(b, _)| *b == binding).expect("bound").1, len)
    }
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

/// The open fractions pass's output: inner faces open (`solid` false), or one in
/// four closed, one in four whole and the rest a fraction; box walls closed;
/// each cell's open volume in weight w, whole without a solid.
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
                }
            }
            if (0..3).all(|a| p[a] < N[a]) {
                face.weight[3] = if solid { rng.unit() } else { 1.0 };
            }
            face
        })
        .collect()
}

/// The solid face velocity pass's output for `open`: a solid velocity and a
/// friction on every inner face a solid cuts, zero elsewhere.
fn solid_velocity(seed: u64, open: &[FaceSample]) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    open.iter()
        .enumerate()
        .map(|(i, record)| {
            let p = pad_coords(i);
            let mut face = FaceSample::default();
            for a in 0..3 {
                if face_exists(p, a) && p[a] > 0 && p[a] < N[a] && record.weight[a] < 1.0 {
                    face.velocity[a] = rng.signed(1.0);
                    face.weight[a] = rng.unit();
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

/// Counting sort by cell, stable: the contract the step's sort keeps.
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
fn gpu_flip_water_is_negative_phi() {
    let mut rng = Stream::new(0xce11);
    // Draws straddle zero, with the eps snap's ±0.005h values among them.
    let phi: Vec<f32> = (0..cell_len())
        .map(|c| match c % 4 {
            0 => 0.005,
            1 => -0.005,
            _ => rng.unit() * 2.0 - 1.0,
        })
        .collect();
    let got: Vec<f32> = Pass::new().bind(7, &phi).run("water_from_phi", &lattice(), 5, cell_len(), cell_len());
    for (c, (g, f)) in got.iter().zip(&phi).enumerate() {
        assert_eq!(*g, f32::from(u8::from(*f < 0.0)), "cell {c}");
    }
}

#[test]
fn gpu_flip_particles_to_faces_matches_the_wyvill_sum() {
    // Dense covers most faces and the walls; sparse leaves faces no particle
    // reaches.
    for (name, count) in [("dense", 400), ("sparse", 12)] {
        let particles = random_particles(0x9261, count);
        let (sorted, ranges) = cpu_sort(&particles);
        let step = StepParams { capacity: sorted.len() as u32, ..lattice() };
        let got: Vec<FaceSample> =
            Pass::new().bind(1, &ranges).bind(2, &sorted).run("particles_to_faces", &step, 4, face_len(), face_len());
        let (checked, invalid, walls) = check_wyvill_faces(&particles, &got);
        if name == "dense" {
            assert!(checked > 200, "the fixture reaches most faces, got {checked}");
            assert!(walls[1] > 10, "the draw reaches wall faces with moving water: {walls:?}");
        } else {
            assert!(checked > 5 && invalid > 50, "{name}: {checked} faces reached, {invalid} not");
        }
    }
}

/// Checks the particles to faces pass against the engine's kernel
/// (velocityadvector.cpp, in cells: r = √3/2). Returns the faces whose
/// velocity was checked, the faces no particle reaches, and the wall faces
/// held and kept.
fn check_wyvill_faces(particles: &[FluidParticle], got: &[FaceSample]) -> (usize, usize, [usize; 2]) {
    let rsq = 0.75f64;
    let wyvill = |d2: f64| {
        if d2 < rsq { 1.0 - 4.0 / 9.0 * d2.powi(3) / rsq.powi(3) + 17.0 / 9.0 * d2 * d2 / (rsq * rsq) - 22.0 / 9.0 * d2 / rsq } else { 0.0 }
    };
    let (mut checked, mut invalid) = (0, 0);
    // Wall faces the particles reach still or moving, all held at 0.
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
                let d2: f64 = (0..3)
                    .map(|b| {
                        let q = (f64::from(particle.position_radius[b]) - f64::from(MIN[b])) / f64::from(H);
                        (q - centre[b]).powi(2)
                    })
                    .sum();
                let w = wyvill(d2);
                weight += w;
                momentum += w * f64::from(particle.velocity[a]);
            }
            // A sum near the 1e-6 validity line can land on either side in f32.
            if (1e-7..1e-5).contains(&weight) {
                continue;
            }
            let valid = weight > 1e-6;
            let velocity = if valid { momentum / weight } else { 0.0 };
            if p[a] == 0 || p[a] == N[a] {
                // A wall is closed: velocity 0 whatever the particles carry,
                // and always valid.
                close(face.weight[a], 1.0, 1.0, &format!("wall weight {p:?}/{a}"));
                walls[usize::from(velocity != 0.0)] += 1;
                assert_eq!(face.velocity[a], 0.0, "wall velocity {p:?}/{a}");
                continue;
            }
            if !valid {
                // Too little weight to trust: extension fills the face.
                assert_eq!((face.velocity[a], face.weight[a]), (0.0, 0.0), "invalid face {p:?}/{a}");
                invalid += 1;
                continue;
            }
            close(face.weight[a], weight, weight, &format!("weight {p:?}/{a}"));
            // Near the rim the kernel is 1 minus terms summing to about 1, so
            // each particle's f32 weight is off by ~1e-7 absolute; a ratio is
            // held to 1e-5 only where the weight dwarfs that.
            if weight > 0.05 {
                close(face.velocity[a], velocity, 1.0, &format!("velocity {p:?}/{a}"));
                checked += 1;
            }
        }
    }
    (checked, invalid, walls)
}

#[test]
fn gpu_flip_face_gravity_adds_gravity_and_holds_the_walls() {
    let faces = random_faces(0x96a7, false);
    let (g, dt) = ([0.5f32, -9.81, 1.25], 1.0f32 / 120.0);
    let step = StepParams { gravity: g, step_dt: dt, ..lattice() };
    let got: Vec<FaceSample> = Pass::new()
        .bind(3, &faces)
        .bind(12, &[0.0f32; 4])
        .bind(13, &[0.0f32; 4])
        .run("face_gravity", &step, 4, face_len(), face_len());
    for (i, (face, before)) in got.iter().zip(&faces).enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            let pushed = f64::from(before.velocity[a]) + f64::from(g[a]) * f64::from(dt);
            let (velocity, weight) = if !face_exists(p, a) {
                (0.0, 0.0)
            } else if p[a] == 0 || p[a] == N[a] {
                (0.0, f64::from(before.weight[a]))
            } else {
                (pushed, f64::from(before.weight[a]))
            };
            close(face.velocity[a], velocity, 1.0, &format!("velocity {p:?}/{a}"));
            close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
        }
    }
}

/// The scene's forces read the tick's lattice at each face's centre; the
/// impulses land on step 0 of the impulse tick only.
#[test]
fn gpu_flip_face_gravity_adds_the_scene_forces_and_impulses() {
    let faces = random_faces(0x5ce7, false);
    let field = FieldLattice::covering(MIN, H, padded().map(|n| n as u32));
    let nodes = field.nodes();
    let mut rng = Stream::new(0xf0c3);
    let mut lattice_values = |count: usize| -> Vec<[f32; 4]> {
        (0..count).map(|_| [rng.signed(4.0), rng.signed(4.0), rng.signed(4.0), 0.0]).collect()
    };
    // Two force lattices from tick 10; the step runs tick 11, so it reads the second.
    let forces = lattice_values(2 * field.node_count());
    let impulses = lattice_values(field.node_count());
    let flat = |values: &[[f32; 4]]| values.iter().flatten().copied().collect::<Vec<f32>>();
    let (g, dt) = ([0.5f32, -9.81, 1.25], 1.0f32 / 120.0);
    for step in [0u32, 1] {
        let params = StepParams {
            gravity: g,
            step_dt: dt,
            tick_index: 11,
            step_in_tick: step as i32,
            field_nodes: nodes,
            field_spacing: field.spacing(),
            force_lattices: 2,
            first_tick: 10,
            impulse_tick: 11,
            ..lattice()
        };
        let got: Vec<FaceSample> = Pass::new()
            .bind(3, &faces)
            .bind(12, &flat(&forces))
            .bind(13, &flat(&impulses))
            .run("face_gravity", &params, 4, face_len(), face_len());
        for (i, (face, before)) in got.iter().zip(&faces).enumerate() {
            let p = pad_coords(i);
            for a in 0..3 {
                if !face_exists(p, a) {
                    close(face.velocity[a], 0.0, 1.0, &format!("velocity {p:?}/{a}"));
                    continue;
                }
                let x: [f32; 3] = std::array::from_fn(|d| {
                    MIN[d] + H * if d == a { p[d] as f32 } else { p[d] as f32 + 0.5 }
                });
                let force = field.sample(&forces[field.node_count()..], x)[a];
                let impulse = if step == 0 { field.sample(&impulses, x)[a] } else { 0.0 };
                let pushed = f64::from(before.velocity[a])
                    + (f64::from(g[a]) + f64::from(force)) * f64::from(dt)
                    + f64::from(impulse);
                let velocity = if p[a] == 0 || p[a] == N[a] { 0.0 } else { pushed };
                close(face.velocity[a], velocity, 1.0, &format!("step {step} velocity {p:?}/{a}"));
                close(face.weight[a], f64::from(before.weight[a]), 1.0, &format!("weight {p:?}/{a}"));
            }
        }
    }
}

#[test]
fn gpu_flip_face_divergence_is_the_outflow_of_water_cells() {
    let faces = random_faces(0xd1f, false);
    let water = random_water(0x3a7e);
    for solid in [false, true] {
        let open = solid_faces(0xd2f, solid);
        let moving = solid_velocity(0xd3f, &open);
        let got: Vec<f32> = Pass::new()
            .bind(3, &faces)
            .bind(6, &water)
            .bind(10, &open)
            .bind(11, &moving)
            .run("divergence", &lattice(), 5, cell_len(), cell_len());
        let mut pushed = 0;
        for (c, g) in got.iter().enumerate() {
            let p = cell_coords(c);
            // A wall face is closed and carries nothing; an inner face counts
            // by its open fraction, plus the solid's (c − w)·v_s, c the
            // cell's open volume.
            let centre = f64::from(open[pad_index(p)].weight[3]);
            let flux = |q: [usize; 3], a: usize| {
                if q[a] == 0 || q[a] == N[a] {
                    return 0.0;
                }
                let w = f64::from(open[pad_index(q)].weight[a]);
                w * f64::from(faces[pad_index(q)].velocity[a]) + (centre - w) * f64::from(moving[pad_index(q)].velocity[a])
            };
            if water[c] > 0.5 && (0..3).any(|a| moving[pad_index(p)].velocity[a] != 0.0) {
                pushed += 1;
            }
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
        assert_eq!(pushed > 10, solid, "the solid fixture moves faces of water cells: {pushed}");
    }
}

/// Distances as the ghost rows can see them: water cells from −0.6h to 0.2h
/// (some above the −0.005h the solve takes at most), air from −0.3h to 2.9h
/// (some below the 0 it takes at least).
fn random_phi(water: &[f32], seed: u64) -> Vec<f32> {
    let mut rng = Stream::new(seed);
    water.iter().map(|&w| H * if w > 0.5 { -0.6 + 0.8 * rng.unit() } else { -0.3 + 3.2 * rng.unit() }).collect()
}

/// Zero distances leave air pressure at zero, the plain Dirichlet projection;
/// real distances give the air side the ghost pressure
/// clamp(max(φ_air, 0) / (min(φ_water, −0.005h) + 1e-9), ±25) · p_water.
#[test]
fn gpu_flip_subtract_pressure_projects_faces_touching_water() {
    let faces = random_faces(0x5b7, false);
    let water = random_water(0xa7e2);
    let mut rng = Stream::new(0x9e55);
    let pressure: Vec<f32> = water.iter().map(|&w| if w > 0.5 { rng.signed(3.0) } else { 0.0 }).collect();
    let ghost = |air: usize, wet: usize, phi: &[f32]| {
        let surface = f64::from(phi[wet]).min(-0.005 * f64::from(H));
        (f64::from(phi[air]).max(0.0) / (surface + 1e-9)).clamp(-25.0, 25.0) * f64::from(pressure[wet])
    };
    // Faces whose air side took a ghost pressure that is not zero.
    let mut ghosts = 0;
    let draws = [
        (false, vec![0.0; cell_len()]),
        (false, random_phi(&water, 0x9e56)),
        (true, random_phi(&water, 0x9e57)),
    ];
    for (solid, phi) in draws {
        let open = solid_faces(0x5c7, solid);
        let got: Vec<FaceSample> = Pass::new()
            .bind(20, &faces)
            .bind(10, &open)
            .bind(6, &water)
            .bind(8, &pressure)
            .bind(7, &phi)
            .run("subtract_pressure", &StepParams { ghost: 1, ..lattice() }, 20, face_len(), face_len());
        let mut closed = 0;
        for (i, face) in got.iter().enumerate() {
            let p = pad_coords(i);
            for a in 0..3 {
                let (velocity, weight) = if !face_exists(p, a) {
                    (0.0, 0.0)
                } else if p[a] == 0 || p[a] == N[a] {
                    (0.0, 1.0)
                } else {
                    let mut below = p;
                    below[a] -= 1;
                    let (up, down) = (cell_index(p), cell_index(below));
                    let (wet_up, wet_down) = (water[up] > 0.5, water[down] > 0.5);
                    let u = f64::from(faces[i].velocity[a]);
                    let p_up = if wet_up { f64::from(pressure[up]) } else { ghost(up, down, &phi) };
                    let p_down = if wet_down { f64::from(pressure[down]) } else { ghost(down, up, &phi) };
                    if open[i].weight[a] <= 0.0 {
                        // A closed face keeps its velocity for the
                        // constraint to replace.
                        closed += 1;
                        (u, 1.0)
                    } else {
                        if wet_up != wet_down && (if wet_up { p_down } else { p_up }) != 0.0 {
                            ghosts += 1;
                        }
                        if wet_up || wet_down { (u - (p_up - p_down) / f64::from(H), 1.0) } else { (u, 0.0) }
                    }
                };
                // Ghost pressures reach 25 × 3, a step of 300 over h.
                close(face.velocity[a], velocity, 400.0, &format!("velocity {p:?}/{a}"));
                close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
            }
        }
        // The projection leaves exactly the residual of the rows the solve
        // inverted: div(out) = div(faces) − L p, L the pressure solver's
        // ghost rows. A θ that differs from the matrix's (the engine's 1e-6
        // here) leaves part of the surface pressure as divergence.
        let h = f64::from(H);
        let div = |f: &[FaceSample], c: [usize; 3]| -> f64 {
            (0..3)
                .map(|a| {
                    let mut up = c;
                    up[a] += 1;
                    f64::from(f[pad_index(up)].velocity[a]) - f64::from(f[pad_index(c)].velocity[a])
                })
                .sum::<f64>()
                / h
        };
        for c in (0..cell_len()).filter(|&c| water[c] > 0.5) {
            let q = cell_coords(c);
            let centre = f64::from(phi[c]).min(-0.005 * h);
            let mut lp = 0.0;
            for a in 0..3 {
                for side in [-1i64, 1] {
                    let at = q[a] as i64 + side;
                    if at < 0 || at >= N[a] as i64 {
                        continue;
                    }
                    let mut r = q;
                    r[a] = at as usize;
                    // A closed face kept its velocity: no pressure crossed it.
                    let mut face = q;
                    face[a] = q[a].max(r[a]);
                    if open[pad_index(face)].weight[a] <= 0.0 {
                        continue;
                    }
                    let j = cell_index(r);
                    let theta = (f64::from(phi[j]).max(0.0) / (centre + 1e-9)).clamp(-25.0, 25.0);
                    let p_j = if water[j] > 0.5 { f64::from(pressure[j]) } else { theta * f64::from(pressure[c]) };
                    lp += (p_j - f64::from(pressure[c])) / (h * h);
                }
            }
            let left = div(&got, q);
            // The walls are closed: the divergence the solve saw carries no
            // wall flux, as `divergence` writes it.
            let mut walled = faces.clone();
            for (i, face) in walled.iter_mut().enumerate() {
                let p = pad_coords(i);
                for a in 0..3 {
                    if p[a] == 0 || p[a] == N[a] {
                        face.velocity[a] = 0.0;
                    }
                }
            }
            let want = div(&walled, q) - lp;
            // f32 faces up to ~300 round by ~3e-5 each; six over h stays under 1e-3.
            assert!((left - want).abs() <= 5e-3, "divergence left in cell {q:?}: {left} vs {want}");
        }
        assert_eq!(closed > 20, solid, "the solid fixture closes faces, the open one none: {closed}");
    }
    assert!(ghosts > 20, "the draw puts ghost pressures on many faces: {ghosts}");
}

/// The engine's particle radius at its default scale: half a cell's diagonal.
fn sdf_radius() -> f64 {
    0.5 * 3f64.sqrt() * f64::from(H)
}

/// The cells a particle at `q` reaches along axis `a`: the engine's box,
/// floor((q ± 2r − min) / h), before the lattice cuts it.
fn scatter_box(q: f64, a: usize) -> [i64; 2] {
    let (h, search) = (f64::from(H), 2.0 * sdf_radius());
    let from = q - f64::from(MIN[a]);
    [((from - search) / h).floor() as i64, ((from + search) / h).floor() as i64]
}

/// The engine's level set: fill with 3h, scatter |centre − p| − r from each
/// live particle into every cell of its box, snap |φ| < 0.005h.
fn cpu_scatter_distance(particles: &[FluidParticle]) -> Vec<f64> {
    let h = f64::from(H);
    let mut phi = vec![3.0 * h; cell_len()];
    for particle in particles.iter().filter(|q| q.position_radius[3] > 0.0) {
        let q: [f64; 3] = std::array::from_fn(|a| f64::from(particle.position_radius[a]));
        let reach: [[i64; 2]; 3] = std::array::from_fn(|a| {
            let [lo, hi] = scatter_box(q[a], a);
            [lo.max(0), hi.min(N[a] as i64 - 1)]
        });
        for z in reach[2][0]..=reach[2][1] {
            for y in reach[1][0]..=reach[1][1] {
                for x in reach[0][0]..=reach[0][1] {
                    let c = [x as usize, y as usize, z as usize];
                    let centre: [f64; 3] = std::array::from_fn(|a| f64::from(MIN[a]) + (c[a] as f64 + 0.5) * h);
                    let d = (0..3).map(|a| (centre[a] - q[a]).powi(2)).sum::<f64>().sqrt() - sdf_radius();
                    let slot = &mut phi[cell_index(c)];
                    *slot = slot.min(d);
                }
            }
        }
    }
    snap(phi)
}

fn snap(phi: Vec<f64>) -> Vec<f64> {
    let eps = 0.005 * f64::from(H);
    phi.into_iter().map(|v| if v.abs() < eps { if v > 0.0 { eps } else { -eps } } else { v }).collect()
}

/// Whether a live particle sits in cell `c` or its 26 neighbours.
fn has_near_particle(c: usize, sorted: &[FluidParticle], ranges: &[CellRange]) -> bool {
    let p = cell_coords(c);
    (p[2].saturating_sub(1)..=(p[2] + 1).min(N[2] - 1)).any(|z| {
        (p[1].saturating_sub(1)..=(p[1] + 1).min(N[1] - 1)).any(|y| {
            (p[0].saturating_sub(1)..=(p[0] + 1).min(N[0] - 1)).any(|x| {
                let r = ranges[cell_index([x, y, z])];
                sorted[r.start as usize..(r.start + r.count) as usize].iter().any(|q| q.position_radius[3] > 0.0)
            })
        })
    })
}

/// The particle distance pass's own reading: the min over the 125 bins around
/// each cell, from the sort's ranges, keeping a particle only when the cell
/// is inside its box; 3h with no live particle in the 27 bins.
fn cpu_gather_distance(sorted: &[FluidParticle], ranges: &[CellRange]) -> Vec<f64> {
    let h = f64::from(H);
    let phi = (0..cell_len())
        .map(|c| {
            let p = cell_coords(c);
            let centre: [f64; 3] = std::array::from_fn(|a| f64::from(MIN[a]) + (p[a] as f64 + 0.5) * h);
            let mut phi = 3.0 * h;
            if !has_near_particle(c, sorted, ranges) {
                return phi;
            }
            for z in p[2].saturating_sub(2)..=(p[2] + 2).min(N[2] - 1) {
                for y in p[1].saturating_sub(2)..=(p[1] + 2).min(N[1] - 1) {
                    for x in p[0].saturating_sub(2)..=(p[0] + 2).min(N[0] - 1) {
                        let r = ranges[cell_index([x, y, z])];
                        for particle in &sorted[r.start as usize..(r.start + r.count) as usize] {
                            let q: [f64; 3] = std::array::from_fn(|a| f64::from(particle.position_radius[a]));
                            let inside = (0..3).all(|a| {
                                let [lo, hi] = scatter_box(q[a], a);
                                (lo..=hi).contains(&(p[a] as i64))
                            });
                            if particle.position_radius[3] > 0.0 && inside {
                                let d = (0..3).map(|a| (centre[a] - q[a]).powi(2)).sum::<f64>().sqrt();
                                phi = phi.min(d - sdf_radius());
                            }
                        }
                    }
                }
            }
            phi
        })
        .collect();
    snap(phi)
}

/// The gather is the engine's scatter, cell for cell, wherever a live
/// particle sits within one cell; elsewhere it reads 3h. A cell holding a
/// live particle is always inside the liquid. A particle 0.499h from its
/// cell's centre along each axis (0.0017h inside its ball's reach) puts
/// that cell at −0.005h and the empty cell across the corner, 0.0017h
/// outside, at +0.005h. In "boxed", cell (3, 1, 1) has a particle in its 27
/// bins at 1.73h, and one two cells out along x at 1.53h whose box stops at
/// cell 2: the cell reads 1.73h, as the engine's does.
#[test]
fn gpu_flip_particle_distance_is_the_engines_level_set() {
    let h = f64::from(H);
    let particle = |q: [f32; 3]| FluidParticle { position_radius: [q[0], q[1], q[2], 0.08], velocity: [0.0; 3], id: 1 };
    let centre = |p: [usize; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + (p[a] as f32 + 0.5) * H) };
    let at = |cells: [f32; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + cells[a] * H) };
    let corner = centre([2, 1, 1]).map(|v| v + 0.499 * H);
    let sets = [
        ("random", random_particles(0xd157, 70)),
        ("corner", vec![particle(corner)]),
        ("boxed", vec![particle(at([2.001, 0.001, 0.001])), particle(at([1.1, 1.5, 1.5]))]),
    ];
    for (name, particles) in sets {
        let (sorted, ranges) = cpu_sort(&particles);
        let want = cpu_gather_distance(&sorted, &ranges);
        let engine = cpu_scatter_distance(&particles);
        let mut far = 0;
        for (c, (a, b)) in want.iter().zip(&engine).enumerate() {
            if has_near_particle(c, &sorted, &ranges) {
                assert!((a - b).abs() < 1e-12, "{name} cell {:?}: gather {a} vs scatter {b}", cell_coords(c));
            } else {
                assert_eq!(*a, 3.0 * h, "{name} cell {:?}: no particle within a cell", cell_coords(c));
                far += usize::from(*b < 3.0 * h);
            }
        }
        if name == "corner" {
            assert!(far > 0, "the lone particle reaches cells two out, which read 3h here");
        }
        let step = StepParams { capacity: sorted.len() as u32, ..lattice() };
        let got: Vec<f32> =
            Pass::new().bind(1, &ranges).bind(2, &sorted).run("particle_distance", &step, 5, cell_len(), cell_len());
        let empty = ranges.iter().filter(|r| r.count == 0).count();
        assert!(empty > 10 && empty < cell_len(), "{name}: the draw has empty and full cells ({empty} empty)");
        for (c, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!((f64::from(*g) - w).abs() <= 1e-5, "{name} cell {:?}: {g} vs {w}", cell_coords(c));
            let r = ranges[c];
            if sorted[r.start as usize..(r.start + r.count) as usize].iter().any(|q| q.position_radius[3] > 0.0) {
                assert!(*w <= -0.005 * h, "{name}: occupied cell {:?} reads {w}", cell_coords(c));
            }
        }
        let read = |c: [usize; 3], want: f64| (f64::from(got[cell_index(c)]) - want).abs() <= 1e-6;
        if name == "corner" {
            assert!(read([2, 1, 1], -0.005 * h), "the particle's cell snaps down");
            assert!(read([3, 2, 2], 0.005 * h), "the empty cell across the corner snaps up");
        }
        if name == "boxed" {
            let corner_reach = (3.0 * (1.499 * h).powi(2)).sqrt() - sdf_radius();
            assert!(read([3, 1, 1], corner_reach), "cell 3 takes the near particle, not the boxed-out one");
            assert!(read([2, 1, 1], 1.4 * h - sdf_radius()), "cell 2 is inside the far particle's box");
        }
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
    let faces = random_faces(0xe7e, true);
    let got: Vec<FaceSample> = Pass::new().bind(3, &faces).run("extend_faces", &lattice(), 4, face_len(), face_len());
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
    let faces = random_faces(0xf1a5, true);
    let old = random_faces(0x01d5, false);
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
    let step = StepParams { step_dt: dt, flip, max_travel, particles: particles.len() as u32, ..lattice() };
    let mut pass = Pass::new();
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles)
        .bind(3, &faces)
        .bind(17, &old)
        // Spread equal to the new faces: no density move.
        .bind(18, &faces)
        .bind(22, &vec![7u32; 2 * particles.len()])
        .run("faces_to_particles", &step, 19, particles.len(), particles.len());
    // Step 0 of the tick starts the counts over the stale 7s.
    let capped: Vec<u32> = pass.bound(22, 2 * particles.len());
    let per_cell = f64::from(dt) / f64::from(H);
    // RK3 stages within the CFL guard, and shortened by it.
    let stages = [std::cell::Cell::new(0usize), std::cell::Cell::new(0usize)];
    let guard = |v: [f64; 3]| {
        let cells = v.iter().map(|c| c * c).sum::<f64>().sqrt() * per_cell;
        let past = cells > f64::from(max_travel);
        stages[usize::from(past)].set(stages[usize::from(past)].get() + 1);
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
        let past_before = stages[1].get();
        let after = cpu_sample(q0, &faces);
        let k1 = guard(after);
        let k2 = guard(cpu_sample(std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]), &faces));
        let k3 = guard(cpu_sample(std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]), &faces));
        let before = cpu_sample(q0, &old);
        assert_eq!(capped[2 * i] as usize, stages[1].get() - past_before, "particle {i}: guarded stages");
        assert_eq!(capped[2 * i + 1], 0, "particle {i}: no bodies, no refused push");
        for a in 0..3 {
            let q1 = (q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0)
                .clamp(WALL_MARGIN_CELLS, N[a] as f64 - WALL_MARGIN_CELLS);
            let position = f64::from(MIN[a]) + q1 * f64::from(H);
            let velocity =
                f64::from(flip) * (f64::from(p.velocity[a]) + after[a] - before[a]) + (1.0 - f64::from(flip)) * after[a];
            assert!((f64::from(g.position_radius[a]) - position).abs() < 2e-5, "particle {i} position {a}: {} vs {position}", g.position_radius[a]);
            assert!((f64::from(g.velocity[a]) - velocity).abs() < 1e-4, "particle {i} velocity {a}: {} vs {velocity}", g.velocity[a]);
        }
        assert_eq!((g.position_radius[3], g.id), (p.position_radius[3], p.id), "particle {i} keeps radius and id");
    }
    let stages = stages.map(std::cell::Cell::into_inner);
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
    // The fill reads the padded lattice the domain publishes: the box N at
    // MIN, grown by the padding on every side.
    let pad = PADDING_NODES as f32;
    let step = params(&[
        ("nodes_x", N[0] as f32 + 1.0 + 2.0 * pad),
        ("nodes_y", N[1] as f32 + 1.0 + 2.0 * pad),
        ("nodes_z", N[2] as f32 + 1.0 + 2.0 * pad),
        ("cell_size", H),
        ("lattice_min_x", MIN[0] - pad * H),
        ("lattice_min_y", MIN[1] - pad * H),
        ("lattice_min_z", MIN[2] - pad * H),
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

// ── Solids ─────────────────────────────────────────────────────────────────

/// The solids' lattice: 5×4×3 cells a quarter metre apart.
const SOLID_N: [usize; 3] = [5, 4, 3];
const SOLID_H: f32 = 0.25;
const SOLID_MIN: [f32; 3] = [-0.6, 0.0, -0.4];
const SOLID_TICK: f32 = 0.05;

fn solid_records() -> usize {
    SOLID_N.iter().map(|v| v + 1).product()
}

fn solid_lattice() -> StepParams {
    StepParams { n: SOLID_N.map(|n| n as u32), box_min: SOLID_MIN, cell_size: SOLID_H, impulse_tick: -1, ..StepParams::default() }
}

/// In-box neighbours of cell `c`, each with the axis and side of the face to it.
fn solid_neighbours(c: usize, n: [usize; 3]) -> Vec<(usize, isize)> {
    let p = [c % n[0], (c / n[0]) % n[1], c / (n[0] * n[1])];
    let mut out = Vec::with_capacity(6);
    for a in 0..3 {
        if p[a] > 0 {
            out.push((a, -1));
        }
        if p[a] + 1 < n[a] {
            out.push((a, 1));
        }
    }
    out
}

/// Inner faces one in four closed, one in four whole, the rest a fraction;
/// every face of cell `isolated` closed. Box walls closed.
fn random_open_faces(n: [usize; 3], seed: u64, isolated: usize) -> Vec<f32> {
    let m = n.map(|v| v + 1);
    let mut faces = vec![0.0; face_grid_len(n)];
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if (0..3).all(|b| b == a || p[b] < n[b]) && p[a] > 0 && p[a] < n[a] {
                faces[i * FACE_FLOATS + 4 + a] = 1.0;
            }
        }
    }
    let draw = random_values(faces.len(), seed);
    for (i, v) in faces.iter_mut().enumerate() {
        if *v > 0.0 {
            let r = draw[i] + 0.5;
            *v = if r < 0.25 { 0.0 } else if r < 0.5 { 1.0 } else { 0.05 + 0.95 * (r - 0.5) * 2.0 };
        }
    }
    for (a, d) in solid_neighbours(isolated, n) {
        let mut p = [isolated % n[0], (isolated / n[0]) % n[1], isolated / (n[0] * n[1])];
        if d > 0 {
            p[a] += 1;
        }
        faces[(p[0] + m[0] * (p[1] + m[1] * p[2])) * FACE_FLOATS + 4 + a] = 0.0;
    }
    faces
}

/// FLIP Fluids' LevelsetUtils::fractionInside for a segment, in f64.
fn engine_segment(left: f64, right: f64) -> f64 {
    if left < 0.0 && right < 0.0 {
        1.0
    } else if left < 0.0 {
        left / (left - right)
    } else if right < 0.0 {
        right / (right - left)
    } else {
        0.0
    }
}

/// FLIP Fluids' LevelsetUtils::fractionInside for a square, in f64, with the
/// branch it took (inside corners, and for two diagonal corners the middle's
/// sign) so a test can show it reached every case.
fn engine_square(bl: f64, br: f64, tl: f64, tr: f64) -> (f64, (usize, bool)) {
    let inside = [bl, tl, br, tr].iter().filter(|&&v| v < 0.0).count();
    let mut list = [bl, br, tr, tl];
    let mut middle_inside = false;
    let fraction = match inside {
        4 => 1.0,
        3 => {
            while list[0] < 0.0 {
                list.rotate_left(1);
            }
            let side0 = 1.0 - engine_segment(list[0], list[3]);
            let side1 = 1.0 - engine_segment(list[0], list[1]);
            1.0 - 0.5 * side0 * side1
        }
        2 => {
            while list[0] >= 0.0 || !(list[1] < 0.0 || list[2] < 0.0) {
                list.rotate_left(1);
            }
            if list[1] < 0.0 {
                0.5 * (engine_segment(list[0], list[3]) + engine_segment(list[1], list[2]))
            } else if 0.25 * (list[0] + list[1] + list[2] + list[3]) < 0.0 {
                middle_inside = true;
                let side1 = 1.0 - engine_segment(list[0], list[3]);
                let side3 = 1.0 - engine_segment(list[2], list[3]);
                let side2 = 1.0 - engine_segment(list[2], list[1]);
                let side0 = 1.0 - engine_segment(list[0], list[1]);
                1.0 - (0.5 * side1 * side3 + 0.5 * side0 * side2)
            } else {
                let side0 = engine_segment(list[0], list[1]);
                let side1 = engine_segment(list[0], list[3]);
                let side2 = engine_segment(list[2], list[1]);
                let side3 = engine_segment(list[2], list[3]);
                0.5 * side0 * side1 + 0.5 * side2 * side3
            }
        }
        1 => {
            while list[0] >= 0.0 {
                list.rotate_left(1);
            }
            0.5 * engine_segment(list[0], list[3]) * engine_segment(list[0], list[1])
        }
        _ => 0.0,
    };
    let diagonal = inside == 2 && !((bl < 0.0) == (br < 0.0) || (bl < 0.0) == (tl < 0.0));
    (fraction, (inside, diagonal && middle_inside))
}

/// FLIP Fluids' LevelsetUtils::volumeFraction for a tetrahedron, in f64,
/// sorted by the engine's five-swap network.
fn engine_tet(p: [f64; 4]) -> f64 {
    let [mut a, mut b, mut c, mut d] = p;
    for (x, y) in [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)] {
        let mut v = [a, b, c, d];
        if v[x] > v[y] {
            v.swap(x, y);
        }
        [a, b, c, d] = v;
    }
    let tet = |a: f64, b: f64, c: f64, d: f64| a * a * a / ((a - b) * (a - c) * (a - d));
    if d <= 0.0 {
        1.0
    } else if c <= 0.0 {
        1.0 - tet(d, c, b, a)
    } else if b <= 0.0 {
        let (p, q, r, s) = (a / (a - c), a / (a - d), b / (b - d), b / (b - c));
        p * q * (1.0 - s) + q * (1.0 - r) * s + r * s
    } else if a <= 0.0 {
        tet(a, b, c, d)
    } else {
        0.0
    }
}

/// FLIP Fluids' MeshLevelSet::_getCellWeight: the fraction of a cell inside
/// the solid, c[i + 2j + 4k] the distance at corner (i, j, k).
fn engine_cube(c: [f64; 8]) -> f64 {
    if c.iter().all(|&v| v < 0.0) {
        return 1.0;
    }
    if c.iter().all(|&v| v >= 0.0) {
        return 0.0;
    }
    let [p000, p100, p010, p110, p001, p101, p011, p111] = c;
    (engine_tet([p000, p001, p101, p011])
        + engine_tet([p000, p101, p100, p110])
        + engine_tet([p000, p010, p011, p110])
        + engine_tet([p101, p011, p111, p110])
        + 2.0 * engine_tet([p000, p011, p101, p110])
        + engine_tet([p100, p101, p001, p111])
        + engine_tet([p100, p001, p000, p010])
        + engine_tet([p100, p110, p111, p010])
        + engine_tet([p001, p111, p011, p010])
        + 2.0 * engine_tet([p100, p111, p001, p010]))
        / 12.0
}

/// The open fraction of every face from a corner lattice, as
/// FluidSimulation::_updateWeightGridThread takes MeshLevelSet's face
/// weights: U from (i,j,k), (i,j+1,k), (i,j,k+1), (i,j+1,k+1); V from
/// (i,j,k), (i,j,k+1), (i+1,j,k), (i+1,j,k+1); W from (i,j,k), (i,j+1,k),
/// (i+1,j,k), (i+1,j+1,k). Box walls closed.
fn cpu_open_fractions(phi: &[f32], n: [usize; 3], tolerance: f64) -> (Vec<f64>, Vec<(usize, bool)>) {
    let m = n.map(|v| v + 1);
    let at = |i: usize, j: usize, k: usize| f64::from(phi[i + m[0] * (j + m[1] * k)]);
    let mut out = vec![0.0; m.iter().product::<usize>() * FACE_FLOATS];
    let mut branches = Vec::new();
    for k in 0..m[2] {
        for j in 0..m[1] {
            for i in 0..m[0] {
                let p = [i, j, k];
                if (0..3).all(|b| p[b] < n[b]) {
                    let corners: [f64; 8] = std::array::from_fn(|c| at(i + (c & 1), j + ((c >> 1) & 1), k + (c >> 2)));
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 7] = (1.0 - engine_cube(corners)).clamp(0.0, 1.0);
                }
                for a in 0..3 {
                    if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                        continue;
                    }
                    let corners = match a {
                        0 => [at(i, j, k), at(i, j + 1, k), at(i, j, k + 1), at(i, j + 1, k + 1)],
                        1 => [at(i, j, k), at(i, j, k + 1), at(i + 1, j, k), at(i + 1, j, k + 1)],
                        _ => [at(i, j, k), at(i, j + 1, k), at(i + 1, j, k), at(i + 1, j + 1, k)],
                    };
                    let (mut inside, branch) = engine_square(corners[0], corners[1], corners[2], corners[3]);
                    if corners.iter().all(|c| c.abs() <= tolerance) {
                        inside = 0.5;
                    }
                    branches.push(branch);
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 4 + a] = (1.0 - inside).clamp(0.0, 1.0);
                }
            }
        }
    }
    (out, branches)
}

/// The step's open fractions against FLIP Fluids' face weights ported to f64
/// here: random corner distances reach every fractionInside case, and one
/// face's corners all within the interface tolerance is half open.
#[test]
fn gpu_flip_open_fractions_match_the_engine() {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let offset = 1.5_f32;
    let mut phi: Vec<f32> = random_values(m.iter().product(), 0x50f).iter().map(|v| v * 0.6).collect();
    // The U face at padded (2, 1, 1): corners (2,1,1), (2,2,1), (2,1,2), (2,2,2).
    for (i, j, k) in [(2, 1, 1), (2, 2, 1), (2, 1, 2), (2, 2, 2)] {
        phi[i + m[0] * (j + m[1] * k)] = 1.0e-7;
    }
    let tolerance = 8.0 * f64::from(f32::EPSILON) * (f64::from(SOLID_H) * 5.0 + f64::from(offset));
    let step = StepParams { box_offset: offset, ..solid_lattice() };
    let got: Vec<f32> = Pass::new().bind(9, &phi).run("open_fractions", &step, 4, face_grid_len(n), solid_records());
    let (want, branches) = cpu_open_fractions(&phi, n, tolerance);
    for case in [(0, false), (1, false), (2, false), (2, true), (3, false), (4, false)] {
        assert!(branches.contains(&case), "the fixture reaches fractionInside case {case:?}");
    }
    assert!(branches.iter().any(|&(inside, middle)| inside == 2 && !middle), "an adjacent or outside-middle pair");
    assert_eq!(want[(2 + m[0] * (1 + m[1])) * FACE_FLOATS + 4], 0.5, "the planted face is on the interface");
    let open_volume: Vec<f64> = want.chunks(FACE_FLOATS).map(|r| r[7]).collect();
    assert!(open_volume.iter().any(|&v| v > 0.0 && v < 1.0), "a cell cut by the solid");
    assert_close(&got, &want, "open fractions");
}

/// Two moving, turning bodies whose lattices overlap, one scaled and one
/// dynamic, and a disabled row; their distance lattices as the atlas stores
/// them (half precision) and as f32.
struct Solids {
    bodies: Vec<LiquidBody>,
    shapes: Vec<LiquidShape>,
    atlas: Vec<u32>,
    distances: Vec<f32>,
}

fn solids() -> Solids {
    let shapes = vec![
        LiquidShape {
            origin_spacing: [-0.47, -0.43, -0.41, 0.3],
            dims_x: 4,
            dims_y: 4,
            dims_z: 4,
            atlas_offset: 0,
            scale_min: [1.0, 1.0, 1.0, 1.0],
        },
        LiquidShape {
            origin_spacing: [-0.39, -0.42, -0.44, 0.4],
            dims_x: 3,
            dims_y: 3,
            dims_z: 3,
            atlas_offset: 64,
            scale_min: [1.2, 1.0, 1.1, 1.0],
        },
    ];
    let distances: Vec<f32> =
        random_values(64 + 27, 0x50d).iter().map(|v| half::f16::from_f32(v * 0.9).to_f32()).collect();
    let mut atlas = Vec::new();
    pack_distance_atlas(&distances, &mut atlas);
    let body = |position: [f32; 3], velocity: [f32; 4], angular: [f32; 3], shape: f32| LiquidBody {
        position_inv_mass: [position[0], position[1], position[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        linear_velocity: velocity,
        angular_velocity: [angular[0], angular[1], angular[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, shape],
        ..LiquidBody::default()
    };
    let mut bodies = vec![
        body([-0.21, 0.38, -0.02], [0.7, -0.3, 0.2, 0.4], [0.0, 0.3, 1.5], 0.0),
        body([0.33, 0.61, 0.03], [-0.5, 0.1, 0.1, 0.9], [0.8, 0.0, -0.2], 1.0),
        body([0.0, 0.5, 0.0], [3.0, 3.0, 3.0, 0.7], [0.0; 3], -1.0),
    ];
    // The second body is dynamic: it moves at its predicted velocity.
    bodies[1].position_inv_mass[3] = 0.5;
    bodies[1].accel_shape = [0.4, -9.8, 0.2, 1.0];
    bodies[1].inv_inertia_x = [0.3, 0.0, 0.0, 1.1];
    bodies[1].inv_inertia_y = [0.0, 0.3, 0.0, -0.6];
    bodies[1].inv_inertia_z = [0.0, 0.0, 0.3, 0.4];
    Solids { bodies, shapes, atlas, distances }
}

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c = cross(u, t);
    std::array::from_fn(|i| v[i] + q[3] * t[i] + c[i])
}

/// The closest enabled body at x after `SOLID_TICK` (its row and signed
/// distance), as liquid_collider.wgsl samples a lattice; the margin by which
/// x clears every lattice edge and the gap to the runner-up, so the fixture
/// can show no f32 rounding decides either.
fn closest(solids: &Solids, x: [f64; 3]) -> (Option<usize>, f64) {
    let mut best: Option<(usize, f64)> = None;
    let mut margin = f64::INFINITY;
    let mut gaps = Vec::new();
    for (row, body) in solids.bodies.iter().enumerate() {
        let shape_index = body.accel_shape[3];
        if shape_index < 0.0 {
            continue;
        }
        let shape = solids.shapes[shape_index as usize];
        let (position, q) = body_pose_at(body, SOLID_TICK);
        let inverse = [-f64::from(q[0]), -f64::from(q[1]), -f64::from(q[2]), f64::from(q[3])];
        let local = rotate(inverse, std::array::from_fn(|i| x[i] - f64::from(position[i])));
        let dims = [shape.dims_x, shape.dims_y, shape.dims_z].map(|d| d as usize);
        let g: [f64; 3] = std::array::from_fn(|i| {
            (local[i] / f64::from(shape.scale_min[i]) - f64::from(shape.origin_spacing[i])) / f64::from(shape.origin_spacing[3])
        });
        for i in 0..3 {
            margin = margin.min(g[i].abs()).min((g[i] - (dims[i] - 1) as f64).abs());
        }
        if !(0..3).all(|i| g[i] >= 0.0 && g[i] <= (dims[i] - 1) as f64) {
            continue;
        }
        let base: [usize; 3] = std::array::from_fn(|i| (g[i].floor() as usize).min(dims[i] - 2));
        let f: [f64; 3] = std::array::from_fn(|i| g[i] - base[i] as f64);
        let mut d = 0.0;
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let w: f64 = (0..3).map(|i| if o[i] == 1 { f[i] } else { 1.0 - f[i] }).product();
            let at = shape.atlas_offset as usize + (base[0] + o[0]) + dims[0] * ((base[1] + o[1]) + dims[1] * (base[2] + o[2]));
            d += w * f64::from(solids.distances[at]);
        }
        d *= f64::from(shape.scale_min[3]);
        gaps.push(d);
        if best.is_none_or(|(_, nearest)| d < nearest) {
            best = Some((row, d));
        }
    }
    gaps.sort_by(f64::total_cmp);
    let gap = if gaps.len() > 1 { gaps[1] - gaps[0] } else { f64::INFINITY };
    (best.map(|(row, _)| row), margin.min(gap))
}

/// The velocity and spin a face sees on body `row`: as uploaded when
/// prescribed; when dynamic, predicted over `SOLID_TICK` plus M⁻¹ times the
/// reaction so far (8 floats per body: linear, 0, angular, 0).
fn moving_velocity(solids: &Solids, row: usize, reaction: &[f32]) -> ([f64; 3], [f64; 3]) {
    let body = &solids.bodies[row];
    let mut v: [f64; 3] = std::array::from_fn(|a| f64::from(body.linear_velocity[a]));
    let mut w: [f64; 3] = std::array::from_fn(|a| f64::from(body.angular_velocity[a]));
    if body.position_inv_mass[3] > 0.0 {
        let t = f64::from(SOLID_TICK);
        let spin = [body.inv_inertia_x[3], body.inv_inertia_y[3], body.inv_inertia_z[3]];
        let inertia = [body.inv_inertia_x, body.inv_inertia_y, body.inv_inertia_z];
        let push = &reaction[8 * row..8 * row + 8];
        for a in 0..3 {
            v[a] += f64::from(body.accel_shape[a]) * t + f64::from(body.position_inv_mass[3]) * f64::from(push[a]);
            let turn: f64 = (0..3).map(|c| f64::from(inertia[a][c]) * f64::from(push[4 + c])).sum();
            w[a] += f64::from(spin[a]) * t + turn;
        }
    }
    (v, w)
}

/// The step's solid face velocity in f64: the closest body's rigid velocity
/// at each cut inner face's centre, the mean friction at its four corners,
/// and the owner code Σ (body + 1)·256^axis in velocity w. Also the smallest
/// margin any query had (see `closest`) and how many faces a body moved.
fn cpu_solid_face_velocity(open: &[f64], solids: &Solids, reaction: &[f32]) -> (Vec<f64>, f64, usize) {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let h = f64::from(SOLID_H);
    let min = SOLID_MIN.map(f64::from);
    let mut out = vec![0.0; face_grid_len(n)];
    let mut margin = f64::INFINITY;
    let mut moved = 0;
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] || open[i * FACE_FLOATS + 4 + a] >= 1.0 {
                continue;
            }
            let mut centre: [f64; 3] = std::array::from_fn(|b| min[b] + (p[b] as f64 + 0.5) * h);
            centre[a] = min[a] + p[a] as f64 * h;
            let (row, clear) = closest(solids, centre);
            margin = margin.min(clear);
            if let Some(row) = row {
                let (position, _) = body_pose_at(&solids.bodies[row], SOLID_TICK);
                let r: [f64; 3] = std::array::from_fn(|b| centre[b] - f64::from(position[b]));
                let (v, w) = moving_velocity(solids, row, reaction);
                let spin = [w[1] * r[2] - w[2] * r[1], w[2] * r[0] - w[0] * r[2], w[0] * r[1] - w[1] * r[0]];
                out[i * FACE_FLOATS + a] = v[a] + spin[a];
                out[i * FACE_FLOATS + 3] += (row + 1) as f64 * 256_f64.powi(a as i32);
                moved += 1;
            }
            let (b, c) = match a {
                0 => (1, 2),
                1 => (2, 0),
                _ => (1, 0),
            };
            let mut friction = 0.0;
            for k in 0..4 {
                let mut q = p;
                q[b] += k & 1;
                q[c] += (k >> 1) & 1;
                let (row, clear) = closest(solids, std::array::from_fn(|d| min[d] + q[d] as f64 * h));
                margin = margin.min(clear);
                if let Some(row) = row {
                    friction += f64::from(solids.bodies[row].linear_velocity[3]);
                }
            }
            out[i * FACE_FLOATS + 4 + a] = 0.25 * friction;
        }
    }
    (out, margin, moved)
}

/// The step's solid face velocity against the rigid velocity, corner
/// friction and owner codes computed here, on random cut faces under two
/// overlapping turning bodies, one dynamic with a reaction so far. The
/// bodies are a second tick's rows, behind a first tick's that would move
/// every face differently.
#[test]
fn gpu_flip_solid_face_velocity_matches_cpu() {
    let solids = solids();
    let open = random_open_faces(SOLID_N, 0x5fa, 0);
    let count = solids.bodies.len();
    let reaction: Vec<f32> = random_values(8 * count, 0x5fb).iter().map(|v| v * 0.4).collect();
    let mut rows = solids.bodies.clone();
    for body in &mut rows {
        body.linear_velocity[0] += 5.0;
        body.position_inv_mass[1] -= 0.2;
    }
    rows.extend(solids.bodies.iter().copied());
    let step = StepParams {
        body_count: count as i32,
        rows: rows.len() as i32,
        tick_seconds: SOLID_TICK,
        shapes_len: solids.shapes.len() as u32,
        ..solid_lattice()
    };
    let got: Vec<f32> = Pass::new()
        .bind(10, &open)
        .bind(14, &rows)
        .bind(15, &solids.shapes)
        .bind(16, &solids.atlas)
        .bind(21, &reaction)
        .run("solid_face_velocity", &step, 4, face_grid_len(SOLID_N), solid_records());
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let (want, margin, moved) = cpu_solid_face_velocity(&open64, &solids, &reaction);
    assert!(margin > 1.0e-4, "no query sits on a lattice edge or a tie between bodies: {margin}");
    assert!(moved > 10, "the bodies reach cut faces: {moved}");
    let frictions: Vec<f64> = want.chunks(FACE_FLOATS).flat_map(|r| r[4..7].to_vec()).collect();
    assert!(frictions.iter().any(|&f| f > 0.0 && f < 0.4), "a face with corners outside every body");
    assert!(frictions.iter().any(|&f| f > 0.4), "corners reaching the second body");
    let codes: Vec<f64> = want.chunks(FACE_FLOATS).map(|r| r[3]).collect();
    assert!(codes.iter().any(|&c| c > 256.0), "a record owned on two axes");
    for (record, (g, w)) in got.chunks(FACE_FLOATS).zip(want.chunks(FACE_FLOATS)).enumerate() {
        assert_eq!(f64::from(g[3]), w[3], "owner code of record {record}");
    }
    assert_close(&got, &want, "solid face velocity");
}

/// The step's solid constraint in f64.
fn cpu_constrain(faces: &[f32], open: &[f64], moving: &[f64], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out: Vec<f64> = faces.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) {
                continue;
            }
            let (v, w) = (i * FACE_FLOATS + a, i * FACE_FLOATS + 4 + a);
            // A box wall is the static domain's closed face.
            if p[a] == 0 || p[a] == n[a] {
                out[v] = 0.0;
                continue;
            }
            let solid = moving[v];
            if open[w] <= 0.0 {
                out[v] = solid;
            } else if open[w] < 1.0 {
                out[v] = moving[w] * solid + (1.0 - moving[w]) * f64::from(faces[v]);
            }
        }
    }
    out
}

#[test]
fn gpu_flip_constrain_solid_faces_matches_cpu() {
    let n = SOLID_N;
    let faces = random_values(face_grid_len(n), 0xc51);
    let open = random_open_faces(n, 0xc52, 0);
    let moving: Vec<f32> = random_values(face_grid_len(n), 0xc53)
        .chunks(FACE_FLOATS)
        .flat_map(|r| [r[0], r[1], r[2], r[3], r[4] + 0.5, r[5] + 0.5, r[6] + 0.5, r[7]])
        .collect();
    let got: Vec<f32> = Pass::new()
        .bind(20, &faces)
        .bind(10, &open)
        .bind(11, &moving)
        .run("constrain_solid_faces", &solid_lattice(), 20, face_grid_len(n), solid_records());
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let moving64: Vec<f64> = moving.iter().map(|&v| f64::from(v)).collect();
    let want = cpu_constrain(&faces, &open64, &moving64, n);
    assert!(open.contains(&0.0) && open.iter().any(|&w| w > 0.0 && w < 1.0), "closed and cut faces");
    assert_close(&got, &want, "constrain solid faces");
}

/// The engine's sealed pockets on CPU (PressureSolver::_conditionSolidVelocityField):
/// water cells link through a face open at least 1e-6; a region reaches air
/// through a linked dry neighbour or an open tank face (`mask`); a cell of an
/// unreached region of more than one cell is isolated.
fn cpu_isolated(water: &[f32], open: &[FaceSample], mask: u32) -> Vec<bool> {
    let wet = |c: [usize; 3]| water[cell_index(c)] > 0.5;
    let neighbours = |c: [usize; 3]| {
        let mut out = Vec::new();
        for a in 0..3 {
            for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])].into_iter().flatten() {
                let mut q = c;
                q[a] = d;
                let mut f = c;
                f[a] = c[a].max(d);
                if open[pad_index(f)].weight[a] >= 1e-6 {
                    out.push(q);
                }
            }
        }
        out
    };
    let mut reach: Vec<bool> = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            let face = (0..3).any(|a| (mask >> (2 * a) & 1 == 0 && c[a] == 0) || (mask >> (2 * a + 1) & 1 == 0 && c[a] == N[a] - 1));
            wet(c) && (face || neighbours(c).iter().any(|&q| !wet(q)))
        })
        .collect();
    let mut queue: Vec<usize> = (0..cell_len()).filter(|&i| reach[i]).collect();
    while let Some(i) = queue.pop() {
        for q in neighbours(cell_coords(i)) {
            let j = cell_index(q);
            if wet(q) && !reach[j] {
                reach[j] = true;
                queue.push(j);
            }
        }
    }
    (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            wet(c) && !reach[i] && neighbours(c).iter().any(|&q| wet(q))
        })
        .collect()
}

/// Runs the pocket passes over `water`, `open` and the solid velocity
/// `moving`, rounds until one changes nothing; returns the conditioned
/// velocity and the rounds that changed a cell.
fn gpu_pockets(water: &[f32], open: &[FaceSample], moving: &[FaceSample], mask: u32) -> (Vec<FaceSample>, usize) {
    let params = StepParams { closed_faces: mask, ..lattice() };
    let lines = (N[1] * N[2]).max(N[0] * N[2]).max(N[0] * N[1]);
    let mut pass = Pass::new();
    pass.bind(6, water).bind(10, open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; 11]).bind(4, moving);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, 11, 1);
    let mut rounds = 0;
    loop {
        pass.run::<u32>("pocket_round", &params, 24, 11, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            pass.run::<u32>(sweep, &params, 24, 11, lines);
        }
        if pass.bound::<u32>(24, 11)[9] == 0 {
            break;
        }
        rounds += 1;
        assert!(rounds <= cell_len(), "the spread never settled");
    }
    pass.run::<u32>("pocket_check", &params, 24, 11, cell_len());
    assert_eq!(pass.bound::<u32>(24, 11)[10], 0, "a settled spread leaves no sealed cell linked to air");
    let got = pass.run::<FaceSample>("pocket_condition", &params, 4, face_len(), face_len());
    (got, rounds)
}

#[test]
fn gpu_flip_sealed_pockets_zero_the_solid_velocity_as_the_engine() {
    let mut zeroed = 0;
    // A random solid seals nothing off, so half its faces are closed again
    // and nearly every cell is water: walls of closed faces split the water
    // into pockets, some reaching a dry cell or open face and some not.
    for (seed, mask) in [(0x5e1, 63), (0x5e2, 63), (0x5e3, 63 & !(1 << 3)), (0x5e4, 63 & !1)] {
        let mut rng = Stream::new(seed);
        let water: Vec<f32> = (0..cell_len()).map(|_| f32::from(u8::from(rng.unit() < 0.97))).collect();
        let mut open = solid_faces(seed + 1, true);
        for face in &mut open {
            for a in 0..3 {
                if rng.unit() < 0.5 {
                    face.weight[a] = 0.0;
                }
            }
        }
        let moving: Vec<FaceSample> = (0..face_len())
            .map(|i| {
                let p = pad_coords(i);
                let mut face = FaceSample::default();
                for a in 0..3 {
                    if face_exists(p, a) {
                        face.velocity[a] = 0.5 + rng.unit();
                    }
                }
                face.velocity[3] = 7.0;
                face
            })
            .collect();
        let isolated = cpu_isolated(&water, &open, mask);
        let (got, rounds) = gpu_pockets(&water, &open, &moving, mask);
        for i in 0..face_len() {
            let p = pad_coords(i);
            for a in 0..3 {
                if !face_exists(p, a) {
                    continue;
                }
                let mut below = p;
                let low = p[a] > 0 && {
                    below[a] = p[a] - 1;
                    isolated[cell_index(below)]
                };
                let high = p[a] < N[a] && isolated[cell_index(p)];
                let want = if low || high { 0.0 } else { moving[i].velocity[a] };
                zeroed += usize::from(low || high);
                assert_eq!(got[i].velocity[a], want, "seed {seed:#x} mask {mask}: record {p:?} axis {a}");
            }
            assert_eq!(got[i].velocity[3], 7.0, "the owner code stays");
            assert_eq!(got[i].weight, moving[i].weight, "the friction stays");
        }
        println!("GPU FLIP pockets seed {seed:#x} mask {mask}: {} isolated cells, {rounds} rounds", isolated.iter().filter(|&&s| s).count());
    }
    assert!(zeroed > 0, "some pocket was sealed");
}

#[test]
fn gpu_flip_pocket_spread_reports_an_unfinished_cap() {
    // Every cell water and every link open, air only past the open -X face:
    // with no round run, sealed cells still link to air.
    let params = StepParams { closed_faces: 63 & !1, ..lattice() };
    let water = vec![1.0f32; cell_len()];
    let open = solid_faces(0x5e9, false);
    let mut pass = Pass::new();
    pass.bind(6, &water).bind(10, &open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; 11]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, 11, 1);
    pass.run::<u32>("pocket_check", &params, 24, 11, cell_len());
    assert_eq!(pass.bound::<u32>(24, 11)[10], 1, "an unfinished spread is flagged");
    pass.bind(22, &[5u32, 6, 7, 9]);
    let first: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 0, ..params }, 22, 4, 1);
    assert_eq!(first, [5, 6, 7, 1], "the tick's first step sets the word");
    let second: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 1, ..params }, 22, 4, 1);
    assert_eq!(second, [5, 6, 7, 2], "later steps add to it");
    assert_eq!(super::gpu_flip_step::pocket_rounds([6, 5, 4]), 6, "the cap is the longest side");
}
