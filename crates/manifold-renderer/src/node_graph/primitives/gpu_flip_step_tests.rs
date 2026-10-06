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

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::gpu_flip_atom_tests::{FACE_FLOATS, assert_close, face_grid_len, random_values};
use super::gpu_flip_step::{POCKET_GATE_WORDS, StepParams, dispatch_pass, tile_total};
use super::liquid_fill::LiquidFill;
use super::liquid_surface_tests::{Harness, params, read};
use super::liquid_stats::with_stats_layout;
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidParticle};
use crate::node_graph::liquid::bodies::{
    BodySupports, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape, SUPPORT_VEC4S, body_pose_at, pack_distance_atlas, pack_supports,
    unpack_supports,
};
use crate::node_graph::liquid::coupling::coupled_start;
use manifold_physics::coupled_motion::{Held, MAX_SUPPORT_POINTS, Mobility, SupportPoint, constrained_mobility, coupled_state_at};
use crate::node_graph::liquid::fields::{FieldLattice, LIQUID_FIELD};
use crate::node_graph::liquid::lattice::PADDING_NODES;
use crate::node_graph::parameters::ParamValue;

/// A lattice with unequal sides, so a swapped axis shows.
const N: [usize; 3] = [6, 5, 4];
const H: f32 = 0.25;
const MIN: [f32; 3] = [-0.5, 0.1, 0.3];

/// The native solid push target in cells.
const WALL_MARGIN_CELLS: f64 = 0.2;
const BOUNDARY_MARGIN_CELLS: f64 = 0.1;
/// The fixtures' walls sit on the lattice edge: their step's wall_inset is 0.
const FIXTURE_WALL_INSET: f64 = 0.0;
const MOVE_EPS_METRES: f64 = 1.0e-6;
const SOLID_STEP_CELLS: f64 = 0.1;
const SOLID_PUSH_CELLS: f64 = 5.0;

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

fn wall_solid(n: [usize; 3], h: f32) -> Vec<f32> {
    let m = n.map(|v| v + 1);
    (0..m.iter().product())
        .map(|i| {
            let q = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
            let distance = (0..3).map(|a| (q[a] as f32).min(n[a] as f32 - q[a] as f32)).fold(f32::INFINITY, f32::min);
            distance * h
        })
        .collect()
}

fn wall_phi(q: [f64; 3], n: [usize; 3]) -> f64 {
    (0..3).map(|a| q[a].min(n[a] as f64 - q[a])).fold(f64::INFINITY, f64::min)
}

fn wall_gradient(q: [f64; 3], n: [usize; 3]) -> [f64; 3] {
    let mut axis = 0;
    let mut value = f64::INFINITY;
    let mut sign = 1.0;
    for a in 0..3 {
        let low = q[a];
        let high = n[a] as f64 - q[a];
        if low < value {
            value = low;
            axis = a;
            sign = 1.0;
        }
        if high < value {
            value = high;
            axis = a;
            sign = -1.0;
        }
    }
    let mut out = [0.0; 3];
    out[axis] = sign;
    out
}

fn boundary_edge() -> f64 {
    FIXTURE_WALL_INSET + BOUNDARY_MARGIN_CELLS
}

fn inside_boundary_cpu(q: [f64; 3], n: [usize; 3], edge: f64) -> bool {
    (0..3).all(|a| q[a] >= edge && q[a] < n[a] as f64 - edge)
}

fn clamp_boundary_cpu(mut q: [f64; 3], n: [usize; 3], edge: f64, h: f64) -> [f64; 3] {
    for a in 0..3 {
        q[a] = q[a].max(edge).min(n[a] as f64 - edge - MOVE_EPS_METRES / h);
    }
    q
}

/// Independent f64 port of FluidSimulation::_resolveCollision for the box
/// wall SDF. It keeps the native endpoint preclamp, march predicate, push,
/// upper-exclusive clamp and fallback ordering visible to the proof.
fn native_wall_move(q0: [f64; 3], q1: [f64; 3], n: [usize; 3], h: f64) -> [f64; 3] {
    native_collision_move(q0, q1, n, h, |q| wall_phi(q, n), |q| wall_gradient(q, n).map(|v| v * h))
}

// Source sequence without the GPU's conservative distance shortcut. phi is
// in cells; gradient holds the native unscaled lattice edge differences.
fn native_collision_move(
    q0: [f64; 3], mut q1: [f64; 3], n: [usize; 3], h: f64,
    phi: impl Fn([f64; 3]) -> f64, gradient: impl Fn([f64; 3]) -> [f64; 3],
) -> [f64; 3] {
    let edge = boundary_edge();
    if (0..3).any(|a| q1[a] < 0.0 || q1[a] >= n[a] as f64) {
        q1 = clamp_boundary_cpu(q1, n, edge, h);
    }
    let travel = (0..3).map(|a| (q1[a] - q0[a]).powi(2)).sum::<f64>().sqrt();
    if travel * h < MOVE_EPS_METRES {
        return q1;
    }
    let steps = (travel / SOLID_STEP_CELLS).ceil() as usize;
    let dir: [f64; 3] = std::array::from_fn(|a| (q1[a] - q0[a]) / travel);
    let mut last = q0;
    for s in 0..steps {
        let current: [f64; 3] = if s == steps - 1 {
            q1
        } else {
            std::array::from_fn(|a| q0[a] + (s + 1) as f64 * SOLID_STEP_CELLS * dir[a])
        };
        let d = phi(current);
        if d < 0.0 || !inside_boundary_cpu(current, n, edge) {
            let g = gradient(current);
            let g_len = (0..3).map(|a| g[a] * g[a]).sum::<f64>().sqrt();
            let mut kept = last;
            if g_len > 1e-6 {
                let pushed = std::array::from_fn(|a| current[a] - (d - WALL_MARGIN_CELLS) * g[a] / g_len);
                let push_distance = (0..3).map(|a| (pushed[a] - current[a]).powi(2)).sum::<f64>().sqrt();
                if push_distance <= SOLID_PUSH_CELLS && phi(pushed) >= 0.0 {
                    kept = pushed;
                }
            }
            if !inside_boundary_cpu(kept, n, edge) {
                let original = kept;
                kept = clamp_boundary_cpu(kept, n, edge, h);
                let clamp_distance = (0..3).map(|a| (kept[a] - original[a]).powi(2)).sum::<f64>().sqrt();
                if phi(kept) < 0.0 || clamp_distance > SOLID_PUSH_CELLS {
                    return last;
                }
            }
            return kept;
        }
        last = current;
    }
    q1
}

#[test]
fn native_wall_oracle_covers_margin_crossing_tangent_and_grid_escape() {
    let n = N;
    let h = f64::from(H);
    let e = boundary_edge();
    let y = 2.5;
    let stationary = native_wall_move([0.15, y, y], [0.15, y, y], n, h);
    assert_eq!(stationary, [0.15, y, y], "stationary point between .1h and .2h is unchanged");
    let approach = native_wall_move([0.15, y, y], [0.05, y, y], n, h);
    assert!((approach[0] - 0.2).abs() < 1e-12, "wall approach pushes to .2h: {approach:?}");
    let tangent = native_wall_move([0.15, y, y], [0.15, y + 0.4, y], n, h);
    assert_eq!(tangent[0], 0.15, "tangent move keeps its wall distance");
    let upper = native_wall_move([n[0] as f64 - 0.15, y, y], [n[0] as f64 - 0.05, y, y], n, h);
    assert!((upper[0] - (n[0] as f64 - 0.2)).abs() < 1e-12, "upper wall push: {upper:?}");
    let escaped = native_wall_move([2.0, y, y], [-2.0, y, y], n, h);
    assert!((escaped[0] - e).abs() < 1e-12, "grid escape clamps to the inclusive lower safety edge: {escaped:?}");
    let upper_escape = native_wall_move([n[0] as f64 - 2.0, y, y], [n[0] as f64 + 2.0, y, y], n, h);
    let upper_bound = n[0] as f64 - e - MOVE_EPS_METRES / h;
    assert!((upper_escape[0] - upper_bound).abs() < 1e-12, "upper grid escape uses the exclusive AABB epsilon: {upper_escape:?}");
    let tiny = native_wall_move([0.05, y, y], [0.050002, y, y], n, h);
    assert_eq!(tiny, [0.050002, y, y], "native movement epsilon is in metres");
    let fallback = native_collision_move([0.05, y, y], [0.05, y + 0.1, y], n, h, |_| 1.0, |_| [0.0; 3]);
    assert_eq!(fallback, [e, y, y], "zero-gradient fallback still receives the boundary clamp");
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

    /// Binds the tile list at 29 as every tile in order, so a pass over the
    /// cell set C covers the whole lattice; returns its thread count (512 a
    /// tile).
    fn every_tile(&mut self) -> usize {
        let tiles: Vec<u32> = (0..tile_total(N.map(|n| n as u32)) as u32).collect();
        self.bind(29, &tiles);
        tiles.len() * 512
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
    let mut pass = Pass::new();
    let threads = pass.every_tile();
    let got: Vec<f32> = pass.bind(7, &phi).run("water_from_phi", &lattice(), 5, cell_len(), threads);
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
        // The pass owns the cells' records; the records past the lattice
        // are the fill's constants.
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let filled: Vec<FaceSample> = Pass::new().run("tiles_fill", &step, 4, face_len(), face_len());
        let got: Vec<FaceSample> =
            pass.bind(1, &ranges).bind(2, &sorted).bind(4, &filled).run("particles_to_faces", &step, 4, face_len(), threads);
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
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let got: Vec<f32> = pass
            .bind(3, &faces)
            .bind(6, &water)
            .bind(10, &open)
            .bind(11, &moving)
            .run("divergence", &lattice(), 5, cell_len(), threads);
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
/// clamp(max(φ_air, 0) / (min(φ_water, −0.005h) + 1e-6), ±25) · p_water.
#[test]
fn gpu_flip_subtract_pressure_projects_faces_touching_water() {
    let faces = random_faces(0x5b7, false);
    let water = random_water(0xa7e2);
    let mut rng = Stream::new(0x9e55);
    let pressure: Vec<f32> = water.iter().map(|&w| if w > 0.5 { rng.signed(3.0) } else { 0.0 }).collect();
    let ghost = |air: usize, wet: usize, phi: &[f32]| {
        let surface = f64::from(phi[wet]).min(-0.005 * f64::from(H));
        (f64::from(phi[air]).max(0.0) / (surface + 1e-6)).clamp(-25.0, 25.0) * f64::from(pressure[wet])
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
                        // Pressure subtraction clears closed faces; the later
                        // solid constraint writes their velocity independently.
                        closed += 1;
                        (0.0, 0.0)
                    } else {
                        if wet_up != wet_down && (if wet_up { p_down } else { p_up }) != 0.0 {
                            ghosts += 1;
                        }
                        if wet_up || wet_down { (u - (p_up - p_down) / f64::from(H), 1.0) } else { (0.0, 0.0) }
                    }
                };
                // Ghost pressures reach 25 × 3, a step of 300 over h.
                close(face.velocity[a], velocity, 400.0, &format!("velocity {p:?}/{a}"));
                close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
            }
        }
        // Native velocity projection uses 1e-6, unlike the matrix's 1e-9.
        // Check that projection operator with the same error bound.
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
                    // A closed face contributes no pressure flux after the
                    // producer-side validity clear.
                    let mut face = q;
                    face[a] = q[a].max(r[a]);
                    if open[pad_index(face)].weight[a] <= 0.0 {
                        continue;
                    }
                    let j = cell_index(r);
                    let theta = (f64::from(phi[j]).max(0.0) / (centre + 1e-6)).clamp(-25.0, 25.0);
                    let p_j = if water[j] > 0.5 { f64::from(pressure[j]) } else { theta * f64::from(pressure[c]) };
                    lp += (p_j - f64::from(pressure[c])) / (h * h);
                }
            }
            let left = div(&got, q);
            // Walls and inner closed faces carry no flux after subtraction;
            // this explicitly includes the producer-side closed-face clear.
            let mut walled = faces.clone();
            for (i, face) in walled.iter_mut().enumerate() {
                let p = pad_coords(i);
                for a in 0..3 {
                    let both_air = if face_exists(p, a) && p[a] > 0 && p[a] < N[a] {
                        let mut below = p;
                        below[a] -= 1;
                        water[cell_index(p)] <= 0.5 && water[cell_index(below)] <= 0.5
                    } else {
                        false
                    };
                    if p[a] == 0 || p[a] == N[a] || open[i].weight[a] <= 0.0 || both_air {
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

/// The particle distance pass's own reading: the min over the 125 bins around
/// each cell, from the sort's ranges, keeping a particle only when the cell
/// is inside its box; 3h when no particle contributes.
fn cpu_gather_distance(sorted: &[FluidParticle], ranges: &[CellRange]) -> Vec<f64> {
    let h = f64::from(H);
    let phi = (0..cell_len())
        .map(|c| {
            let p = cell_coords(c);
            let centre: [f64; 3] = std::array::from_fn(|a| f64::from(MIN[a]) + (p[a] as f64 + 0.5) * h);
            let mut phi = 3.0 * h;
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

fn distance_fixtures() -> Vec<(&'static str, Vec<FluidParticle>)> {
    let particle = |q: [f32; 3]| FluidParticle { position_radius: [q[0], q[1], q[2], 0.08], velocity: [0.0; 3], id: 1 };
    let centre = |p: [usize; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + (p[a] as f32 + 0.5) * H) };
    let at = |cells: [f32; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + cells[a] * H) };
    let corner = centre([2, 1, 1]).map(|v| v + 0.499 * H);
    vec![
        ("random", random_particles(0xd157, 70)),
        ("corner", vec![particle(corner)]),
        ("boxed", vec![particle(at([2.001, 0.001, 0.001])), particle(at([1.1, 1.5, 1.5]))]),
        ("ring_two_only", vec![particle(at([2.05, 2.5, 1.5]))]),
    ]
}

/// CPU gather must reproduce the native scatter in every cell, including
/// air cells with only ring-two contributors beside a wet cell. Those air
/// distances set the ghost-fluid pressure ratio.
#[test]
fn particle_distance_gather_matches_native_scatter() {
    for (name, particles) in distance_fixtures() {
        let (sorted, ranges) = cpu_sort(&particles);
        let gather = cpu_gather_distance(&sorted, &ranges);
        let engine = cpu_scatter_distance(&particles);
        for (c, (a, b)) in gather.iter().zip(&engine).enumerate() {
            assert!((a - b).abs() < 1e-12, "{name} cell {:?}: gather {a} vs scatter {b}", cell_coords(c));
        }
        if name == "ring_two_only" {
            let h = f64::from(H);
            assert!((engine[cell_index([0, 2, 1])] - (1.55 * h - sdf_radius())).abs() < 1e-6);
            assert!(engine[cell_index([1, 2, 1])] < 0.0, "adjacent cell must be wet");
        }
    }
}

/// GPU distances match the native scatter oracle in every cell. Fixtures
/// exercise zero snapping, exact scatter-box support, and sparse ring two.
#[test]
fn gpu_flip_particle_distance_is_the_engines_level_set() {
    let h = f64::from(H);
    for (name, particles) in distance_fixtures() {
        let (sorted, ranges) = cpu_sort(&particles);
        let want = cpu_scatter_distance(&particles);
        let step = StepParams { capacity: sorted.len() as u32, ..lattice() };
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let got: Vec<f32> = pass.bind(1, &ranges).bind(2, &sorted).run("particle_distance", &step, 5, cell_len(), threads);
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

#[test]
fn gpu_flip_extend_faces_fills_one_layer() {
    let faces = random_faces(0xe7e, true);
    let got: Vec<FaceSample> = Pass::new().bind(3, &faces).run("extend_faces", &lattice(), 4, face_len(), face_len());
    let want = super::gpu_flip_extension_tests::cpu_extend(&faces, N);
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

/// A wall's held zero must not claim the gap before the fluid front arrives.
#[test]
fn gpu_flip_extend_faces_waits_for_fluid_beside_walls() {
    use super::gpu_flip_extension_tests::{cpu_extend, wall_gap};
    for axis in 0..3 {
        for high in [false, true] {
            let (mut faces, target) = wall_gap(N, axis, high);
            for layer in 0..2 {
                let want = cpu_extend(&faces, N);
                let got: Vec<FaceSample> = Pass::new().bind(3, &faces).run("extend_faces", &lattice(), 4, face_len(), face_len());
                for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                    for a in 0..3 {
                        close(g.velocity[a], f64::from(w.velocity[a]), 1.0, &format!("layer {layer} face {:?}/{a}", pad_coords(i)));
                        assert_eq!(g.weight[a], w.weight[a]);
                    }
                }
                faces = got;
            }
            assert!(faces[target].velocity[axis] > 0.0, "axis {axis}, high {high}: fluid reaches the wall gap");
        }
    }
}

/// The engine holds every border sample of a component's lattice done: its
/// value counts in a neighbour's mean, it is never extended or a seed. Beside
/// a transverse wall row and in a two-row corner, two layers.
#[test]
fn gpu_flip_extend_faces_holds_border_rows_as_native() {
    use super::gpu_flip_extension_tests::{cpu_extend, transverse_wall_fixture};
    for corner in [false, true] {
        let mut faces = transverse_wall_fixture(N, corner);
        let mut averaged = 0;
        for layer in 0..2 {
            let want = cpu_extend(&faces, N);
            let got: Vec<FaceSample> = Pass::new().bind(3, &faces).run("extend_faces", &lattice(), 4, face_len(), face_len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                for a in 0..3 {
                    close(g.velocity[a], f64::from(w.velocity[a]), 1.0, &format!("corner {corner} layer {layer} face {:?}/{a}", pad_coords(i)));
                    assert_eq!(g.weight[a], w.weight[a], "corner {corner} layer {layer} weight {:?}/{a}", pad_coords(i));
                    averaged += usize::from(w.weight[a] > 0.0 && w.velocity[a] > 0.0 && w.velocity[a] < 2.0);
                }
            }
            faces = got;
        }
        assert!(averaged > 0, "corner {corner}: the fixture averages a held border zero");
    }
}

#[test]
fn gpu_flip_pressure_clear_precedes_face_extension() {
    let target = [2, 2, 2];
    let target_index = pad_index(target);
    let mut faces = vec![FaceSample::default(); face_len()];
    faces[target_index].velocity[0] = 9.0;
    faces[pad_index([4, 2, 2])].velocity[0] = 13.0;
    let open = vec![FaceSample { weight: [1.0; 4], ..FaceSample::default() }; face_len()];
    // An inner closed high-velocity face and open faces between air cells must
    // both be cleared by subtraction, so extension has no invalid seed.
    let mut open = open;
    open[target_index].weight[0] = 0.0;
    let water = vec![0.0; cell_len()];
    let pressure = vec![0.0; cell_len()];
    let phi = vec![0.0; cell_len()];
    let projected: Vec<FaceSample> = Pass::new()
        .bind(20, &faces)
        .bind(10, &open)
        .bind(6, &water)
        .bind(8, &pressure)
        .bind(7, &phi)
        .run("subtract_pressure", &lattice(), 20, face_len(), face_len());
    assert_eq!(projected[target_index].velocity[0], 0.0, "closed inner velocity is cleared before extension");
    assert_eq!(projected[target_index].weight[0], 0.0, "closed inner validity is cleared before extension");
    assert_eq!(projected[pad_index([4, 2, 2])].velocity[0], 0.0, "open air-only velocity is cleared before extension");
    let extended: Vec<FaceSample> = Pass::new()
        .bind(3, &projected)
        .run("extend_faces", &lattice(), 4, face_len(), face_len());
    for x in [target[0] - 1, target[0], target[0] + 1] {
        let index = pad_index([x, target[1], target[2]]);
        assert_eq!(extended[index].velocity[0], 0.0, "closed air face cannot seed x-neighbour velocity at x={x}");
        assert_eq!(extended[index].weight[0], 0.0, "closed air face cannot seed x-neighbour validity at x={x}");
    }
}

fn density_support_sources() -> [String; 2] {
    const CULL: &str = "                    if finite(q) && any(abs(centre - q) >= vec3<f32>(1.0)) {\n                        continue;\n                    }\n";
    let source = include_str!("shaders/gpu_flip_step.wgsl");
    assert_eq!(source.matches(CULL).count(), 1, "density support predicate occurs once");
    let original = source.replacen(CULL, "", 1);
    [original.as_str(), source].map(|shader| {
        with_stats_layout(&format!("{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{LIQUID_FIELD}\n{shader}"))
    })
}

#[test]
fn gpu_flip_density_support_shader_validates() {
    for source in density_support_sources() {
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("density support shader validates");
    }
}

#[test]
fn gpu_flip_density_support_matches_original() {
    let mut pass = Pass::new();
    let pipelines = density_support_sources().map(|source| {
        pass.device.create_compute_pipeline(&source, "density_source", "density-support-proof")
    });
    let mut particles = Vec::new();
    let mut rng = Stream::new(0xde8517);
    // Near rest rather than clamp-saturated: eight slightly jittered sites
    // per cell. Leave one corner empty to exercise the face-neighbour air rule.
    for c in 0..cell_len() - 1 {
        let p = cell_coords(c);
        for site in 0..8 {
            let q: [f32; 3] = std::array::from_fn(|a| p[a] as f32 + 0.25 + 0.5 * ((site >> a) & 1) as f32 + rng.signed(0.004));
            let x: [f32; 3] = std::array::from_fn(|a| MIN[a] + H * q[a]);
            particles.push(FluidParticle { position_radius: [x[0], x[1], x[2], 0.08], ..FluidParticle::default() });
        }
    }
    let centre = [2.5_f32; 3];
    let x = centre.map(|q| q * H);
    particles.push(FluidParticle { position_radius: [MIN[0] + x[0], MIN[1] + x[1], MIN[2] + x[2], 0.08], ..FluidParticle::default() });
    let mut support = [0; 2];
    for a in 0..3 {
        for side in [-1.0_f32, 1.0] {
            let boundary = MIN[a] + H * (centre[a] + side);
            for direction in [-1, 0, 1] {
                let mut x: [f32; 3] = std::array::from_fn(|b| MIN[b] + H * centre[b]);
                x[a] = boundary;
                // Also include world-coordinate neighbours; several ULPs
                // survive the world-to-lattice rounding at every box offset.
                for _ in 0..4 {
                    x[a] = match direction { -1 => x[a].next_down(), 1 => x[a].next_up(), _ => x[a] };
                }
                let q: [f32; 3] = std::array::from_fn(|b| (x[b] - MIN[b]) / H);
                let outside = (0..3).any(|b| (centre[b] - q[b]).abs() >= 1.0);
                support[usize::from(outside)] += 1;
                particles.push(FluidParticle { position_radius: [x[0], x[1], x[2], 0.08], ..FluidParticle::default() });
            }
        }
    }
    assert!(support.iter().all(|&count| count > 0), "positive and culled zero-support probes: {support:?}");
    let mut water = vec![1.0_f32; cell_len()];
    water[0] = 0.0;
    let mask: Vec<u32> = (0..cell_len()).map(|c| u32::from(c % 4 != 0)).collect();
    for nonfinite in [false, true] {
        let mut input = particles.clone();
        if nonfinite {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut p = input[0];
                p.position_radius[0] = value;
                input.push(p);
            }
        }
        let (sorted, ranges) = cpu_sort(&input);
        for (body_count, distance) in [(0, 4.0 * H), (1, 4.0 * H), (1, -4.0 * H)] {
            for narrow in [false, true] {
                pass.bound.clear();
                let threads = pass.every_tile();
                pass.bind(1, &ranges).bind(2, &sorted).bind(6, &water)
                    .bind(9, &vec![distance; face_len()]).bind(45, &mask)
                    .bind(46, &[0_u32; 12]).bind(5, &vec![f32::NAN; cell_len()]);
                let step = StepParams {
                    capacity: sorted.len() as u32, body_count, rate: 4.0,
                    narrow_band: if narrow { 2 } else { 0 }, ..lattice()
                };
                let outputs = pipelines.each_ref().map(|pipeline| {
                    let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&step) }];
                    bindings.extend(pass.bound.iter().map(|(binding, buffer)| GpuBinding::Buffer { binding: *binding, buffer, offset: 0 }));
                    let mut enc = pass.device.create_encoder("density support parity");
                    enc.dispatch_compute(pipeline, &bindings, [threads.div_ceil(256) as u32, 1, 1], "density-support-proof");
                    enc.commit_and_wait_completed();
                    pass.bound::<f32>(5, cell_len())
                });
                for (c, (&want, &got)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
                    let context = format!("cell{c} body{body_count} sdf{distance} narrow{narrow} nonfinite{nonfinite}");
                    if want.is_finite() {
                        assert_eq!(got.to_bits(), want.to_bits(), "{context}: finite output bits");
                    } else if want.is_nan() {
                        assert!(got.is_nan(), "{context}: NaN classification");
                    } else {
                        assert!(got.is_infinite() && got.is_sign_negative() == want.is_sign_negative(), "{context}: infinity classification");
                    }
                    if c == 0 || (narrow && mask[c] == 0) {
                        assert_eq!(got, 0.0, "{context}: air/inactive mask");
                    }
                }
                if !nonfinite {
                    assert!(outputs[0].iter().all(|v| v.is_finite()), "finite fixture yields finite output");
                    if body_count == 0 {
                        let value = outputs[0][cell_index([2, 2, 2])];
                        assert!(value < 0.0 && value.abs() < 2.0, "positive marker contribution is visible below the density clamp: {value}");
                        assert!(outputs[0].iter().any(|v| v.abs() > 0.0 && v.abs() < 2.0), "nonzero unsaturated near-rest outputs");
                    }
                }
            }
        }
    }
}

/// Native MACVelocityField's ordinary per-component trilinear sample, in f64.
/// q is in cells from the lattice minimum. Missing corners contribute zero;
/// face validity does not renormalise the interpolation.
fn cpu_sample(q: [f64; 3], field: &[FaceSample]) -> [f64; 3] {
    cpu_sample_on(q, field, N)
}

pub(super) fn cpu_sample_on(q: [f64; 3], field: &[FaceSample], n: [usize; 3]) -> [f64; 3] {
    if q.iter().any(|value| !value.is_finite() || *value < 0.0)
        || q.iter().zip(n.iter()).any(|(value, &extent)| *value >= extent as f64)
    {
        return [0.0; 3];
    }
    std::array::from_fn(|a| {
        let top: [i64; 3] = std::array::from_fn(|b| if b == a { n[b] as i64 } else { n[b] as i64 - 1 });
        let s: [f64; 3] = std::array::from_fn(|b| q[b] - if b == a { 0.0 } else { 0.5 });
        let base: [i64; 3] = std::array::from_fn(|b| s[b].floor() as i64);
        let t: [f64; 3] = std::array::from_fn(|b| s[b] - base[b] as f64);
        let mut sum = 0.0;
        for corner in 0..8 {
            let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let c: [i64; 3] = std::array::from_fn(|b| base[b] + bit[b] as i64);
            if (0..3).all(|b| c[b] >= 0 && c[b] <= top[b]) {
                let c: [usize; 3] = c.map(|value| value as usize);
                let face = field[c[0] + (n[0] + 1) * (c[1] + (n[1] + 1) * c[2])];
                let w: f64 = (0..3).map(|b| if bit[b] == 1 { t[b] } else { 1.0 - t[b] }).product();
                sum += w * f64::from(face.velocity[a]);
            }
        }
        sum
    })
}

#[test]
fn cpu_sample_matches_native_mac_interpolation_and_bounds() {
    // Constant stored values include invalid faces deliberately: native
    // interpolation reads stored values and only fades missing lattice
    // corners, independent of the validity weights.
    let constant = vec![FaceSample { velocity: [2.0, 3.0, 5.0, 0.0], weight: [0.0; 4] }; face_len()];
    let check = |q: [f64; 3], want: [f64; 3], label: &str| {
        let got = cpu_sample(q, &constant);
        for a in 0..3 {
            assert!((got[a] - want[a]).abs() < 1e-12, "{label} component {a}: {} vs {}", got[a], want[a]);
        }
    };
    check([2.0, 2.0, 2.0], [2.0, 3.0, 5.0], "constant interior");
    check([0.25, 0.25, 0.25], [1.125, 1.6875, 2.8125], "lower corner fade");
    check([5.75, 4.75, 3.75], [1.125, 1.6875, 2.8125], "upper corner fade");
    check([0.0, 0.0, 0.0], [0.5, 0.75, 1.25], "lower inclusive corner");
    check([-f64::EPSILON, 1.0, 1.0], [0.0; 3], "below grid");
    check([N[0] as f64, 1.0, 1.0], [0.0; 3], "exclusive high edge");

    // An affine field is reproduced exactly at an interior point by each
    // component's face lattice, including its half-cell transverse offsets.
    let mut affine = vec![FaceSample::default(); face_len()];
    for (i, face) in affine.iter_mut().enumerate() {
        let c = pad_coords(i).map(|value| value as f32);
        for a in 0..3 {
            face.velocity[a] = 10.0 * (a as f32 + 1.0) + c[0] + 2.0 * c[1] + 3.0 * c[2];
        }
    }
    let q = [2.25, 2.75, 1.5];
    let got = cpu_sample(q, &affine);
    for (a, got) in got.into_iter().enumerate() {
        let expected = 10.0 * (a as f64 + 1.0)
            + (q[0] - f64::from((a != 0) as u8) * 0.5)
            + 2.0 * (q[1] - f64::from((a != 1) as u8) * 0.5)
            + 3.0 * (q[2] - f64::from((a != 2) as u8) * 0.5);
        assert!((got - expected).abs() < 1e-5, "affine component {a}: {got} vs {expected}");
    }
}

#[test]
fn gpu_flip_faces_to_particles_reads_native_stored_face_values() {
    // Exact binary positions and zero origin exercise all three exclusive
    // high edges without a world-coordinate rounding ambiguity.
    let points = [[2.0, 2.0, 2.0], [0.25, 0.25, 0.25], [5.75, 4.75, 3.75],
        [0.0, 0.0, 0.0], [-0.125, 1.0, 1.0], [6.0, 1.0, 1.0],
        [1.0, 5.0, 1.0], [1.0, 1.0, 4.0], [2.25, 2.75, 1.5]];
    let particles: Vec<_> = points.iter().enumerate().map(|(i, q)| FluidParticle {
        position_radius: [q[0] as f32 * H, q[1] as f32 * H, q[2] as f32 * H, 0.08],
        velocity: [0.0; 3], id: i as u32 + 1,
    }).collect();
    // Include nonzero data in non-existent packed component lanes. Correct
    // component bounds must discard those lanes, regardless of their weight.
    let stored = vec![FaceSample { velocity: [2.0, 3.0, 5.0, 0.0], weight: [0.0; 4] }; face_len()];
    let mut affine = stored.clone();
    for (i, face) in affine.iter_mut().enumerate() {
        let c = pad_coords(i);
        for a in 0..3 {
            face.velocity[a] = (10 * (a + 1) + c[0] + 2 * c[1] + 3 * c[2]) as f32;
        }
    }
    let zero = vec![FaceSample::default(); face_len()];
    let solid = wall_solid(N, H);
    let run = |flip: f32, faces: &[FaceSample], old: &[FaceSample]| {
        Pass::new()
            .bind(2, &particles)
            .bind(3, faces)
            .bind(9, &solid)
            .bind(15, &[LiquidShape::default()])
            .bind(16, &[0_u32; 4])
            .bind(17, old)
            .bind(18, faces)
            .bind(22, &vec![0_u32; 2 * particles.len()])
            .bind(36, &[LiquidBody::default()])
            .run::<FluidParticle>("faces_to_particles", &StepParams {
                step_dt: 0.0, flip, particles: particles.len() as u32, box_min: [0.0; 3], ..lattice()
            }, 19, particles.len(), particles.len())
    };
    for field in [&stored, &affine] {
        let fresh = run(0.0, field, &zero);
        let old_grid = run(1.0, &zero, field);
        for (i, q) in points.into_iter().enumerate() {
            let want = cpu_sample(q, field);
            for (a, want) in want.into_iter().enumerate() {
                close(fresh[i].velocity[a], want, 10.0, "new-grid stored sample");
                close(old_grid[i].velocity[a], -want, 10.0, "old-grid stored sample");
            }
            assert_eq!(fresh[i].id, particles[i].id);
        }
        assert_eq!(fresh[0].position_radius, particles[0].position_radius, "dt=0 keeps the interior position");
        assert_eq!(old_grid[0].position_radius, particles[0].position_radius, "dt=0 keeps the old-grid interior position");
    }
}

#[test]
fn gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3() {
    let faces = random_faces(0xf1a5, true);
    let old = random_faces(0x01d5, false);
    let mut particles = random_particles(0x2b3, 300);
    // Keep the random draw away from wall corners so the f64 wall oracle
    // compares the same one-axis lattice gradient as the shader.
    for particle in &mut particles {
        for a in 0..3 {
            let q = ((particle.position_radius[a] - MIN[a]) / H).clamp(1.0, N[a] as f32 - 1.0);
            particle.position_radius[a] = MIN[a] + q * H;
        }
    }
    // Two particles against the walls, so the native march is exercised.
    particles[0].position_radius[..3].copy_from_slice(&[MIN[0] + 0.15 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H]);
    particles[1].position_radius[..3].copy_from_slice(&[MIN[0] + 2.5 * H, MIN[1] + (N[1] as f32 - 0.15) * H, MIN[2] + 2.5 * H]);
    // Two broken particles: the move must leave them non-finite, never clamp
    // or zero them, so the tick's stats see them.
    particles[2].position_radius[0] = f32::NAN;
    particles[3].velocity[1] = f32::INFINITY;
    let (dt, flip) = (0.07f32, 0.9f32);
    let step = StepParams { step_dt: dt, flip, particles: particles.len() as u32, ..lattice() };
    let solid = wall_solid(N, H);
    let mut pass = Pass::new();
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles)
        .bind(3, &faces)
        // Metal requires the conditional solid/region resources even with zero counts.
        .bind(9, &solid)
        .bind(15, &[LiquidShape::default()])
        .bind(16, &[0_u32; 4])
        .bind(36, &[LiquidBody::default()])
        .bind(17, &old)
        // Spread equal to the new faces: no density move.
        .bind(18, &faces)
        .bind(22, &vec![7u32; 2 * particles.len()])
        .run("faces_to_particles", &step, 19, particles.len(), particles.len());
    // Step 0 of the tick starts the counts over the stale 7s.
    let capped: Vec<u32> = pass.bound(22, 2 * particles.len());
    let per_cell = f64::from(dt) / f64::from(H);
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
        let k1 = after;
        let k2 = cpu_sample(std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]), &faces);
        let k3 = cpu_sample(std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]), &faces);
        let before = cpu_sample(q0, &old);
        assert_eq!(capped[2 * i], 0, "particle {i}: native RK3 does not cap stage velocities");
        assert_eq!(capped[2 * i + 1], 0, "particle {i}: no bodies, no refused push");
        let reached: [f64; 3] = std::array::from_fn(|a| q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0);
        let q1 = native_wall_move(q0, reached, N, f64::from(H));
        for a in 0..3 {
            let position = f64::from(MIN[a]) + q1[a] * f64::from(H);
            let velocity =
                f64::from(flip) * (f64::from(p.velocity[a]) + after[a] - before[a]) + (1.0 - f64::from(flip)) * after[a];
            assert!((f64::from(g.position_radius[a]) - position).abs() < 2e-5, "particle {i} position {a}: {} vs {position}", g.position_radius[a]);
            assert!((f64::from(g.velocity[a]) - velocity).abs() < 1e-4, "particle {i} velocity {a}: {} vs {velocity}", g.velocity[a]);
        }
        assert_eq!((g.position_radius[3], g.id), (p.position_radius[3], p.id), "particle {i} keeps radius and id");
    }
}

/// A low pre-step marker speed does not bound the post-force grid. Exercise
/// both a uniformly high field and a field whose later RK3 stages accelerate.
#[test]
fn gpu_flip_rk3_stage_velocity_is_not_truncated() {
    let n = [96usize, 8, 8];
    let m = n.map(|v| v + 1);
    let len = m.iter().product();
    let q0 = [32.0, 4.0, 4.0];
    let dt = 0.01f32;
    let per_cell = f64::from(dt) / f64::from(H);
    // The former default Top Speed of 20 m/s rounded this dt/h to one cell.
    let old_limit = 1.0 / per_cell;
    for (name, initial, slope) in [("uniform", 600.0, 0.0), ("later stages", 20.0, 20.0)] {
        let faces: Vec<_> = (0..len).map(|i| FaceSample {
            velocity: [initial + slope * (i % m[0]) as f32 - slope * q0[0] as f32, 0.0, 0.0, 0.0],
            weight: [1.0; 4],
        }).collect();
        let particle = FluidParticle {
            position_radius: [MIN[0] + q0[0] as f32 * H, MIN[1] + q0[1] as f32 * H, MIN[2] + q0[2] as f32 * H, 0.08],
            velocity: [0.0; 3], id: 17,
        };
        let k1 = cpu_sample_on(q0, &faces, n);
        let k2 = cpu_sample_on(std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]), &faces, n);
        let k3 = cpu_sample_on(std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]), &faces, n);
        assert!(k3[0] > old_limit, "{name} exceeds the former stage limiter");
        if slope != 0.0 { assert!(k1[0] < old_limit && k2[0] > old_limit); }
        let want: [f64; 3] = std::array::from_fn(|a| q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0);
        assert!(want[0] - q0[0] > 1.0, "{name} distinguishes the former one-cell stage cap");
        assert!(want[0] < n[0] as f64 - 2.0, "the proof remains clear of collisions");
        for live in [false, true] {
            let mut clock = [0u32; 12];
            clock[0] = dt.to_bits();
            clock[3] = 0.0f32.to_bits(); // Pre-step marker speed before the high grid field.
            clock[11] = u32::from(live);
            let mut pass = Pass::new();
            let got: Vec<FluidParticle> = pass.bind(2, &[particle]).bind(3, &faces)
                .bind(9, &wall_solid(n, H)).bind(15, &[LiquidShape::default()])
                .bind(16, &[0_u32; 4]).bind(17, &faces).bind(18, &faces)
                .bind(22, &[91u32; 2]).bind(36, &[LiquidBody::default()]).bind(46, &clock)
                .run("faces_to_particles", &StepParams { n: n.map(|v| v as u32), step_dt: dt, particles: 1, ..lattice() }, 19, 1, 1);
            for a in 0..3 {
                let actual = (f64::from(got[0].position_radius[a]) - f64::from(MIN[a])) / f64::from(H);
                assert!((actual - want[a]).abs() < 2e-5, "{name}, live={live}, axis {a}: {actual} != {want:?}");
            }
            assert_eq!(pass.bound::<u32>(22, 2), [0, 0], "{name}: no stage cap or refused push");
            assert_eq!((got[0].id, got[0].position_radius[3]), (particle.id, particle.position_radius[3]));
        }
    }
}

#[test]
fn gpu_flip_marker_motion_matches_native_wall_sequence_without_bodies() {
    let y = 2.5_f64;
    let cases = [
        ("stationary", [0.15, y, y], [0.0, 0.0, 0.0], 0.01_f32, false),
        ("approach", [0.15, y, y], [-1.0, 0.0, 0.0], 0.01, false),
        ("crossing", [0.15, y, y], [-10.0, 0.0, 0.0], 0.04, false),
        ("tangent", [0.15, y, y], [0.0, 1.0, 0.0], 0.10, false),
        ("upper", [N[0] as f64 - 0.15, y, y], [1.0, 0.0, 0.0], 0.01, false),
        ("grid escape", [2.0, y, y], [-20.0, 0.0, 0.0], 0.10, false),
        ("upper grid escape", [N[0] as f64 - 2.0, y, y], [20.0, 0.0, 0.0], 0.10, false),
        ("world epsilon", [0.05, y, y], [0.00005, 0.0, 0.0], 0.01, false),
        ("fallback clamp", [0.05, y, y], [0.0, 1.0, 0.0], 0.025, true),
        ("nonfinite", [f64::NAN, y, y], [0.0; 3], 0.01, false),
    ];
    for entry in ["faces_to_particles", "narrow_move"] {
    for (name, q0, velocity, dt, flat) in cases {
        let solid = if flat { vec![H; face_len()] } else { wall_solid(N, H) };
        let particles = [FluidParticle {
            position_radius: [
                MIN[0] + q0[0] as f32 * H,
                MIN[1] + q0[1] as f32 * H,
                MIN[2] + q0[2] as f32 * H,
                0.08,
            ],
            velocity: [0.0; 3],
            id: 1,
        }];
        // Both RK3 and the optional RK4 marker mover must read stored solid
        // velocities even when their validity flag is zero. This does not
        // validate the separate Narrow Band grid-backtrace sampler.
        let faces = vec![FaceSample { velocity: [velocity[0] as f32, velocity[1] as f32, velocity[2] as f32, 0.0], weight: [0.0; 4] }; face_len()];
        let step = StepParams { step_dt: dt, particles: 1, ..lattice() };
        let got: Vec<FluidParticle> = Pass::new()
            .bind(2, &particles)
            .bind(3, &faces)
            .bind(9, &solid)
            .bind(15, &[LiquidShape::default()])
            .bind(16, &[0_u32; 4])
            .bind(17, &faces)
            .bind(18, &faces)
            .bind(22, &[0_u32; 2])
            .bind(36, &[LiquidBody::default()])
            .run(entry, &step, 19, 1, 1);
        if name == "nonfinite" {
            assert!(!got[0].position_radius[0].is_finite(), "{entry}: invalid position remains visible");
            assert_eq!((got[0].position_radius[3], got[0].id), (particles[0].position_radius[3], particles[0].id));
            continue;
        }
        // Sampling changes near the current grid edges, so even a constant
        // stored field is not a constant particle velocity there. Compute the
        // actual integrator stages before applying the independent collision
        // oracle; these fixtures keep every stage inside the travel guard.
        let per_cell = f64::from(dt) / f64::from(H);
        let at = |fraction: f64, v: [f64; 3]| cpu_sample(std::array::from_fn(|a| q0[a] + fraction * per_cell * v[a]), &faces);
        let k1 = cpu_sample(q0, &faces);
        let k2 = at(0.5, k1);
        let reached: [f64; 3] = if entry == "faces_to_particles" {
            let k3 = at(0.75, k2);
            std::array::from_fn(|a| q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0)
        } else {
            let k3 = at(0.5, k2);
            let k4 = at(1.0, k3);
            std::array::from_fn(|a| q0[a] + per_cell * (k1[a] + 2.0 * k2[a] + 2.0 * k3[a] + k4[a]) / 6.0)
        };
        if name.contains("grid escape") {
            assert!(reached[0] < 0.0 || reached[0] >= N[0] as f64,
                "{entry}/{name}: fixture reaches outside the grid: {reached:?}");
        }
        let want = if flat {
            native_collision_move(q0, reached, N, f64::from(H), |_| 1.0, |_| [0.0; 3])
        } else {
            native_wall_move(q0, reached, N, f64::from(H))
        };
        let actual: [f64; 3] = std::array::from_fn(|a| (f64::from(got[0].position_radius[a]) - f64::from(MIN[a])) / f64::from(H));
        for a in 0..3 {
            assert!((actual[a] - want[a]).abs() < 2e-5, "{entry}/{name} axis {a}: {actual:?} vs {want:?}");
        }
        assert!(got[0].position_radius[3] > 0.0, "{name} remains live");
    }
    }
}

/// D19: a particle under a body is never removed; only an open face's band
/// removes water. The first sits at the centre of a solid block, where the
/// distance has no gradient to climb, so it stays and counts as refused.
#[test]
fn gpu_flip_collision_keeps_negative_body_phi_and_removes_open_band_particles() {
    let mut solid = wall_solid(N, H);
    let m = N.map(|n| n + 1);
    for z in 2..=3 {
        for y in 2..=3 {
            for x in 2..=3 {
                solid[x + m[0] * (y + m[1] * z)] = -H;
            }
        }
    }
    let particles = [
        FluidParticle { position_radius: [MIN[0] + 2.5 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08], id: 1, ..FluidParticle::default() },
        FluidParticle { position_radius: [MIN[0] + 0.5 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08], id: 2, ..FluidParticle::default() },
    ];
    let faces = vec![FaceSample::default(); face_len()];
    for entry in ["faces_to_particles", "narrow_move"] {
    for closed_faces in [62, 63] {
    let step = StepParams { body_count: 1, rows: 1, region_count: 0, closed_faces, particles: 2, ..lattice() };
    let mut pass = Pass::new();
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles)
        .bind(3, &faces)
        .bind(9, &solid)
        .bind(15, &[LiquidShape::default()])
        .bind(16, &[0_u32; 4])
        .bind(17, &faces)
        .bind(18, &faces)
        .bind(22, &[0_u32; 4])
        .bind(36, &[LiquidBody::default()])
        .run(entry, &step, 19, particles.len(), particles.len());
    assert_eq!(got[0].position_radius, particles[0].position_radius, "{entry}: the particle under the body stays, live");
    assert_eq!(pass.bound::<u32>(22, 2)[1], 1, "{entry}: its push-out is counted as refused");
    assert_eq!(got[1].position_radius[3], if closed_faces == 62 { 0.0 } else { 0.08 }, "only the open low-X band removes the second particle");
    }
    }
}

#[test]
fn gpu_flip_collision_refuses_excessive_push_and_keeps_the_particle() {
    // A plane solid whose exit is beyond the native five-cell push limit.
    // The last point also lies outside the safety AABB; its attempted clamp
    // stays inside this solid, so the final native fallback is the old point.
    // D19: the particle is kept there, live, and counted as refused.
    let solid: Vec<f32> = (0..face_len()).map(|i| (pad_coords(i)[0] as f32 - 10.0) * H).collect();
    let particle = FluidParticle { position_radius: [MIN[0] + 0.05 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08], id: 1, ..FluidParticle::default() };
    let faces = vec![FaceSample { velocity: [0.0, 1.0, 0.0, 0.0], weight: [1.0; 4] }; face_len()];
    let step = StepParams { body_count: 1, rows: 1, step_dt: 0.025, particles: 1, ..lattice() };
    for entry in ["faces_to_particles", "narrow_move"] {
        let mut pass = Pass::new();
        let got: Vec<FluidParticle> = pass
            .bind(2, &[particle]).bind(3, &faces).bind(9, &solid)
            .bind(15, &[LiquidShape::default()]).bind(16, &[0_u32; 4])
            .bind(17, &faces).bind(18, &faces).bind(22, &[0_u32; 2])
            .bind(36, &[LiquidBody::default()])
            .run(entry, &step, 19, 1, 1);
        assert_eq!(got[0].position_radius[..3], particle.position_radius[..3], "{entry}: last-position fallback");
        assert_eq!(got[0].position_radius[3], particle.position_radius[3], "{entry}: water is never deleted at a solid");
        assert_eq!(pass.bound::<u32>(22, 2)[1], 1, "{entry}: refused push is counted");
    }
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
        assert!((g.position_radius[3] - 0.31017524 * H).abs() < 1e-6);
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

/// A body at `SOLID_TICK` as pose_bodies poses it: position, rotation,
/// linear and angular velocity, and its packed mobility on the supports it
/// stays on.
struct Posed {
    position: [f32; 3],
    rotation: [f32; 4],
    linear: [f32; 3],
    angular: [f32; 3],
    mobility: Mobility,
}

/// Box3D's substep over a `SOLID_TICK` tick: the law's h.
fn solid_h() -> f32 {
    manifold_physics::coupled_motion::coupled_substep(manifold_physics::Seconds(f64::from(SOLID_TICK)))
}

/// Each body of `solids` at `SOLID_TICK`: a dynamic one by the coupled motion
/// law (`manifold_physics::coupled_motion`, the CPU twin) under its reaction
/// so far (8 floats a body) and its support points (one set a body); any
/// other at its own velocity (`body_pose_at`).
fn posed(solids: &Solids, reaction: &[f32], contacts: &[BodySupports]) -> Vec<Posed> {
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    solids
        .bodies
        .iter()
        .enumerate()
        .map(|(row, body)| {
            let start = coupled_start(body);
            let mut points = [SupportPoint::default(); MAX_SUPPORT_POINTS];
            let kept = unpack_supports(&contacts[row], &mut points);
            let points = &points[..kept];
            if body.position_inv_mass[3] > 0.0 {
                let push = &reaction[8 * row..8 * row + 8];
                let s = coupled_state_at(
                    &start,
                    points,
                    [push[0], push[1], push[2]],
                    [push[4], push[5], push[6]],
                    SOLID_TICK,
                    solid_h(),
                );
                let mobility = constrained_mobility(&start, points, s.held);
                Posed { position: s.position, rotation: s.rotation, linear: s.linear_velocity, angular: s.angular_velocity, mobility }
            } else {
                let (position, rotation) = body_pose_at(body, SOLID_TICK);
                let mobility = constrained_mobility(&start, points, Held::default());
                Posed { position, rotation, linear: xyz(body.linear_velocity), angular: xyz(body.angular_velocity), mobility }
            }
        })
        .collect()
}

/// The closest enabled body at x posed as `poses` holds them (its row and
/// signed distance), as liquid_shape_distance samples a lattice, past it adds
/// the gap and scales along the trilinear slope; none when the walls (inset
/// 0) are nearer. The margin is the gap
/// to the runner-up, body or walls, so the fixture can show no f32 rounding
/// decides either.
fn closest(solids: &Solids, poses: &[Posed], x: [f64; 3]) -> (Option<usize>, f64) {
    let mut best: Option<(usize, f64)> = None;
    let mut gaps = Vec::new();
    for (row, body) in solids.bodies.iter().enumerate() {
        let shape_index = body.accel_shape[3];
        if shape_index < 0.0 {
            continue;
        }
        let shape = solids.shapes[shape_index as usize];
        let (position, q) = (poses[row].position, poses[row].rotation);
        let inverse = [-f64::from(q[0]), -f64::from(q[1]), -f64::from(q[2]), f64::from(q[3])];
        let local = rotate(inverse, std::array::from_fn(|i| x[i] - f64::from(position[i])));
        let dims = [shape.dims_x, shape.dims_y, shape.dims_z].map(|d| d as usize);
        let g: [f64; 3] = std::array::from_fn(|i| {
            (local[i] / f64::from(shape.scale_min[i]) - f64::from(shape.origin_spacing[i])) / f64::from(shape.origin_spacing[3])
        });
        let c: [f64; 3] = std::array::from_fn(|i| g[i].clamp(0.0, (dims[i] - 1) as f64));
        let beyond = (0..3).map(|i| (g[i] - c[i]).powi(2)).sum::<f64>().sqrt() * f64::from(shape.origin_spacing[3]);
        let base: [usize; 3] = std::array::from_fn(|i| (c[i].floor() as usize).min(dims[i] - 2));
        let f: [f64; 3] = std::array::from_fn(|i| c[i] - base[i] as f64);
        let (mut d, mut slope) = (0.0, [0.0f64; 3]);
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let w: [f64; 3] = std::array::from_fn(|i| if o[i] == 1 { f[i] } else { 1.0 - f[i] });
            let at = shape.atlas_offset as usize + (base[0] + o[0]) + dims[0] * ((base[1] + o[1]) + dims[1] * (base[2] + o[2]));
            let v = f64::from(solids.distances[at]);
            d += w[0] * w[1] * w[2] * v;
            for i in 0..3 {
                let sign = if o[i] == 1 { 1.0 } else { -1.0 };
                slope[i] += sign * w[(i + 1) % 3] * w[(i + 2) % 3] * v;
            }
        }
        let steep = slope.iter().map(|v| v * v).sum::<f64>().sqrt();
        let across = (0..3).map(|i| (slope[i] / f64::from(shape.scale_min[i])).powi(2)).sum::<f64>().sqrt();
        let stretch = if steep > 1e-3 * f64::from(shape.origin_spacing[3]) { steep / across } else { f64::from(shape.scale_min[3]) };
        d = (d + beyond) * stretch;
        gaps.push(d);
        if best.is_none_or(|(_, nearest)| d < nearest) {
            best = Some((row, d));
        }
    }
    let walls = (0..3)
        .map(|i| {
            let low = f64::from(SOLID_MIN[i]);
            (x[i] - low).min(low + SOLID_N[i] as f64 * f64::from(SOLID_H) - x[i])
        })
        .fold(f64::INFINITY, f64::min);
    gaps.push(walls);
    gaps.sort_by(f64::total_cmp);
    let gap = if gaps.len() > 1 { gaps[1] - gaps[0] } else { f64::INFINITY };
    (best.filter(|&(_, d)| d <= walls).map(|(row, _)| row), gap)
}

/// The step's solid face velocity in f64: the closest body's rigid velocity
/// at each cut inner face's centre, the mean friction at its four corners,
/// and the owner code Σ (body + 1)·256^axis in velocity w. Also the smallest
/// margin any query had (see `closest`) and how many faces a body moved.
fn cpu_solid_face_velocity(open: &[f64], solids: &Solids, poses: &[Posed]) -> (Vec<f64>, f64, usize) {
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
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                continue;
            }
            // Only a face a solid covers is sampled; the extrapolation
            // carries it out.
            if open[i * FACE_FLOATS + 4 + a] >= 1.0 {
                continue;
            }
            out[i * FACE_FLOATS + 7] += f64::from(1u32 << a);
            let mut centre: [f64; 3] = std::array::from_fn(|b| min[b] + (p[b] as f64 + 0.5) * h);
            centre[a] = min[a] + p[a] as f64 * h;
            let (row, clear) = closest(solids, poses, centre);
            margin = margin.min(clear);
            if let Some(row) = row {
                let pose = &poses[row];
                let r: [f64; 3] = std::array::from_fn(|b| centre[b] - f64::from(pose.position[b]));
                let (v, w) = (pose.linear.map(f64::from), pose.angular.map(f64::from));
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
                let (row, clear) = closest(solids, poses, std::array::from_fn(|d| min[d] + q[d] as f64 * h));
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

/// The bodies of `solids` as a second tick's rows behind a first tick's that
/// would pose every body differently, a reaction so far, and support points
/// for both ticks' rows (the second tick's dynamic body on a floor's four
/// corners and against a slanted wall; the first tick's under a ceiling):
/// rows, reaction, supports, and the step's params.
fn two_tick_bodies(solids: &Solids) -> (Vec<LiquidBody>, Vec<f32>, Vec<BodySupports>, StepParams) {
    let count = solids.bodies.len();
    let reaction: Vec<f32> = random_values(8 * count, 0x5fb).iter().map(|v| v * 0.4).collect();
    let mut rows = solids.bodies.clone();
    for body in &mut rows {
        body.linear_velocity[0] += 5.0;
        body.position_inv_mass[1] -= 0.2;
    }
    rows.extend(solids.bodies.iter().copied());
    let point = |lever, normal, patch, patch_lever| SupportPoint { lever, normal, friction: 0.6, patch, patch_lever, ..SupportPoint::default() };
    let mut contacts = vec![[[0.0f32; 4]; SUPPORT_VEC4S]; rows.len()];
    let ceiling = [0.0, 0.15, 0.0];
    contacts[1] = pack_supports(&[point(ceiling, [0.0, -1.0, 0.0], 0, ceiling)]);
    let floor = [[0.1, -0.15, 0.1], [-0.1, -0.15, 0.1], [-0.1, -0.15, -0.1], [0.1, -0.15, -0.1]];
    let mut held: Vec<SupportPoint> = floor.iter().map(|&lever| point(lever, [0.0, 1.0, 0.0], 0, [0.0, -0.15, 0.0])).collect();
    let wall = [0.09, 0.0, -0.12];
    held.push(point(wall, [-0.6, 0.0, 0.8], 1, wall));
    contacts[count + 1] = pack_supports(&held);
    let step = StepParams {
        body_count: count as i32,
        rows: rows.len() as i32,
        tick_seconds: SOLID_TICK,
        shapes_len: solids.shapes.len() as u32,
        coupled_h: solid_h(),
        ..solid_lattice()
    };
    (rows, reaction, contacts, step)
}

/// I18 (LIQUID_SOLVER_SEAM_DESIGN.md D15): pose_bodies poses each body of the
/// tick as the CPU law does: the dynamic body by `coupled_state_at` with a
/// floor and a slanted wall cutting its predicted fall, the prescribed ones
/// at their own velocity, every other field kept; and each body's mobility
/// as `constrained_mobility` gives it on the supports the law leaves it on.
#[test]
fn gpu_flip_pose_bodies_matches_coupled_motion() {
    let solids = solids();
    let (rows, reaction, contacts, step) = two_tick_bodies(&solids);
    let count = solids.bodies.len();
    let mut pass = Pass::new();
    pass.bind(14, &rows).bind(21, &reaction).bind(49, &contacts).bind(50, &vec![[0.0f32; 4]; 6 * count]);
    let got: Vec<LiquidBody> = pass.run("pose_bodies", &step, 48, count, count);
    let mobility: Vec<[f32; 4]> = pass.bound(50, 6 * count);
    let want = posed(&solids, &reaction, &contacts[count..]);
    let unprojected = posed(&solids, &reaction, &vec![[[0.0; 4]; SUPPORT_VEC4S]; count]);
    assert!(
        (want[1].linear[1] - unprojected[1].linear[1]).abs() > 0.1,
        "the floor cuts the dynamic body's predicted fall: {} against {}",
        want[1].linear[1],
        unprojected[1].linear[1]
    );
    for (b, (g, w)) in got.iter().zip(&want).enumerate() {
        let near = |a: &[f32], e: &[f32], what: &str| {
            for (x, y) in a.iter().zip(e) {
                assert!((x - y).abs() <= 2e-6 * (1.0 + y.abs()), "body {b} {what}: {a:?} against {e:?}");
            }
        };
        near(&g.position_inv_mass[..3], &w.position, "position");
        near(&g.rotation, &w.rotation, "rotation");
        near(&g.linear_velocity[..3], &w.linear, "linear velocity");
        near(&g.angular_velocity[..3], &w.angular, "angular velocity");
        let scale = w.mobility.iter().fold(1.0f32, |a, b| a.max(b.abs()));
        let packed: Vec<f32> = mobility[6 * b..6 * b + 6].iter().flatten().copied().collect();
        for (k, (x, y)) in packed.iter().zip(&w.mobility).enumerate() {
            assert!((x - y).abs() <= 1e-5 * scale, "body {b} mobility {k}: {x} against {y}");
        }
        let body = &solids.bodies[b];
        assert_eq!(
            (g.position_inv_mass[3], g.linear_velocity[3], g.angular_velocity[3], g.inv_inertia_x, g.inv_inertia_y, g.inv_inertia_z, g.accel_shape),
            (body.position_inv_mass[3], body.linear_velocity[3], body.angular_velocity[3], body.inv_inertia_x, body.inv_inertia_y, body.inv_inertia_z, body.accel_shape),
            "body {b} keeps its other fields"
        );
    }
}

/// The step's solid face velocity against the rigid velocity, corner
/// friction and owner codes computed here, on random cut faces under two
/// overlapping turning bodies, one dynamic with a reaction so far and two
/// contact normals, posed by pose_bodies. The bodies are a second tick's
/// rows, behind a first tick's that would move every face differently.
#[test]
fn gpu_flip_solid_face_velocity_matches_cpu() {
    let solids = solids();
    let open = random_open_faces(SOLID_N, 0x5fa, 0);
    let count = solids.bodies.len();
    let (rows, reaction, contacts, step) = two_tick_bodies(&solids);
    let mut pass = Pass::new();
    pass.bind(10, &open).bind(14, &rows).bind(15, &solids.shapes).bind(16, &solids.atlas).bind(21, &reaction).bind(49, &contacts);
    pass.bind(50, &vec![[0.0f32; 4]; 6 * count]);
    let _: Vec<LiquidBody> = pass.run("pose_bodies", &step, 48, count, count);
    let got: Vec<f32> = pass.run("solid_face_velocity", &step, 4, face_grid_len(SOLID_N), solid_records());
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let poses = posed(&solids, &reaction, &contacts[count..]);
    let (want, margin, moved) = cpu_solid_face_velocity(&open64, &solids, &poses);
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

/// One layer of the solid velocity's extrapolation in f64, as
/// GridUtils::extrapolateGridWithObserver runs it on each axis's face
/// lattice: border faces are done from the start but never seed; an unknown
/// face beside a known one takes the mean of its done neighbours.
fn cpu_solid_extrapolate(records: &[f32], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out: Vec<f64> = records.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        let mut known = records[i * FACE_FLOATS + 7] as u32;
        let mut code = records[i * FACE_FLOATS + 3] as u32;
        for a in 0..3 {
            let mut top = n.map(|v| v - 1);
            top[a] = n[a];
            if (0..3).any(|b| p[b] > top[b]) {
                continue;
            }
            let border = |q: [usize; 3]| (0..3).any(|b| q[b] == 0 || q[b] == top[b]);
            let is_known = |r: usize| (records[r * FACE_FLOATS + 7] as u32 >> a) & 1 == 1;
            if border(p) || is_known(i) {
                continue;
            }
            let (mut sum, mut count, mut owner, mut seeded) = (0.0, 0.0, 0u32, false);
            for b in 0..3 {
                for d in [-1i64, 1] {
                    let mut q = p;
                    let x = p[b] as i64 + d;
                    if x < 0 || x > top[b] as i64 {
                        continue;
                    }
                    q[b] = x as usize;
                    let r = q[0] + m[0] * (q[1] + m[1] * q[2]);
                    if border(q) || is_known(r) {
                        sum += f64::from(records[r * FACE_FLOATS + a]);
                        count += 1.0;
                    }
                    if !border(q) && is_known(r) {
                        seeded = true;
                        if owner == 0 {
                            owner = (records[r * FACE_FLOATS + 3] as u32 >> (8 * a)) & 255;
                        }
                    }
                }
            }
            if seeded {
                out[i * FACE_FLOATS + a] = sum / count;
                known |= 1 << a;
                code |= owner << (8 * a);
            }
        }
        out[i * FACE_FLOATS + 3] = f64::from(code);
        out[i * FACE_FLOATS + 7] = f64::from(known);
    }
    out
}

/// One layer of the solid extrapolation against the engine's rule in f64,
/// on random velocities, a sparse random known set and random owners.
#[test]
fn gpu_flip_solid_extrapolate_matches_cpu() {
    let n = SOLID_N;
    let mut seed = 0x5e7u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let records: Vec<f32> = random_values(face_grid_len(n), 0x5e6)
        .chunks(FACE_FLOATS)
        .flat_map(|r| {
            let known = (0..3).filter(|_| next() % 5 == 0).fold(0u32, |k, a| k | (1 << a));
            let code = (0..3).filter(|a| (known >> a) & 1 == 1).fold(0u32, |c, a| c | ((1 + next() % 3) << (8 * a)));
            [r[0], r[1], r[2], code as f32, 0.0, 0.0, 0.0, known as f32]
        })
        .collect();
    let got: Vec<f32> = Pass::new()
        .bind(3, &records)
        .run("solid_extrapolate", &solid_lattice(), 4, face_grid_len(n), solid_records());
    let want = cpu_solid_extrapolate(&records, n);
    let grown = got
        .chunks(FACE_FLOATS)
        .zip(records.chunks(FACE_FLOATS))
        .filter(|(g, r)| g[7] as u32 != r[7] as u32)
        .count();
    assert!(grown > 5,"the layer reaches unknown faces: {grown}");
    for (record, (g, w)) in got.chunks(FACE_FLOATS).zip(want.chunks(FACE_FLOATS)).enumerate() {
        assert_eq!(f64::from(g[3]), w[3], "owner code of record {record}");
        assert_eq!(f64::from(g[7]), w[7], "known mask of record {record}");
    }
    assert_close(&got, &want, "solid extrapolation");
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
/// unreached region of more than one cell is isolated. Returns the isolated
/// cells, then every sealed (wet, unreached) cell.
fn cpu_isolated(water: &[f32], open: &[FaceSample], mask: u32) -> (Vec<bool>, Vec<bool>) {
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
    let isolated = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            wet(c) && !reach[i] && neighbours(c).iter().any(|&q| wet(q))
        })
        .collect();
    let sealed = (0..cell_len()).map(|i| wet(cell_coords(i)) && !reach[i]).collect();
    (isolated, sealed)
}

/// The sealed water's pockets on CPU: each cell of `sealed` labelled by the
/// lowest cell index it links to through sealed cells; other cells None.
fn cpu_pocket_labels(sealed: &[bool], open: &[FaceSample]) -> Vec<Option<usize>> {
    let mut label = vec![None; cell_len()];
    for start in 0..cell_len() {
        if !sealed[start] || label[start].is_some() {
            continue;
        }
        // Ascending start, so the first cell reached is the lowest index.
        label[start] = Some(start);
        let mut queue = vec![start];
        while let Some(i) = queue.pop() {
            let c = cell_coords(i);
            for a in 0..3 {
                for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])].into_iter().flatten() {
                    let mut q = c;
                    q[a] = d;
                    let mut f = c;
                    f[a] = c[a].max(d);
                    let j = cell_index(q);
                    if sealed[j] && label[j].is_none() && open[pad_index(f)].weight[a] >= 1e-6 {
                        label[j] = Some(start);
                        queue.push(j);
                    }
                }
            }
        }
    }
    label
}

/// Runs the pocket passes over `water`, `open` and the solid velocity
/// `moving`, rounds until one changes nothing; returns the conditioned
/// velocity, the rounds that changed a cell, then `rhs` with each sealed
/// pocket's mean removed and the solver words after it.
fn gpu_pockets(water: &[f32], open: &[FaceSample], moving: &[FaceSample], mask: u32, rhs: &[f32]) -> (Vec<FaceSample>, usize, Vec<f32>, Vec<u32>) {
    let params = StepParams { closed_faces: mask, ..lattice() };
    let lines = (N[1] * N[2]).max(N[0] * N[2]).max(N[0] * N[1]);
    let mut pass = Pass::new();
    pass.bind(6, water)
        .bind(10, open)
        .bind(23, &vec![0u32; cell_len()])
        .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
        .bind(4, moving)
        .bind(25, &vec![0u32; cell_len()])
        .bind(5, rhs)
        .bind(26, &vec![7u32; 3 * cell_len() + 2])
        .bind(22, &[9u32; 6]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    let mut rounds = 0;
    loop {
        pass.run::<u32>("pocket_round", &params, 24, POCKET_GATE_WORDS as usize, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            pass.run::<u32>(sweep, &params, 24, POCKET_GATE_WORDS as usize, lines);
        }
        if pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[9] == 0 {
            break;
        }
        rounds += 1;
        assert!(rounds <= cell_len(), "the spread never settled");
    }
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 0, "a settled spread leaves no sealed cell linked to air");
    let got = pass.run::<FaceSample>("pocket_condition", &params, 4, face_len(), face_len());
    pass.run::<u32>("pocket_clear", &params, 26, 3 * cell_len() + 2, 3 * cell_len() + 2);
    pass.run::<u32>("pocket_accumulate", &params, 26, 3 * cell_len() + 2, cell_len());
    let removed = pass.run::<f32>("pocket_remove", &params, 5, cell_len(), cell_len());
    let words = pass.run::<u32>("pocket_flux_pressure", &StepParams { step_in_tick: 0, ..params }, 22, 6, 1);
    (got, rounds, removed, words)
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
        let (isolated, sealed) = cpu_isolated(&water, &open, mask);
        let rhs: Vec<f32> = (0..cell_len()).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let (got, rounds, removed, words) = gpu_pockets(&water, &open, &moving, mask, &rhs);
        // Each sealed pocket loses its mean, so it sums to 0; every other
        // cell keeps its value. The GPU rounds each value to 1/65536 first.
        let labels = cpu_pocket_labels(&sealed, &open);
        let mut sums = std::collections::BTreeMap::<usize, (f64, usize)>::new();
        for (i, label) in labels.iter().enumerate() {
            if let Some(l) = label {
                let entry = sums.entry(*l).or_default();
                entry.0 += f64::from(rhs[i]);
                entry.1 += 1;
            }
        }
        let mut total = 0.0;
        for (i, label) in labels.iter().enumerate() {
            let want = match label {
                Some(l) => {
                    let (sum, count) = sums[l];
                    if i == *l {
                        total += sum.abs();
                    }
                    f64::from(rhs[i]) - sum / count as f64
                }
                None => f64::from(rhs[i]),
            };
            let tolerance = if label.is_some() { 1e-4 } else { 0.0 };
            assert!((f64::from(removed[i]) - want).abs() <= tolerance, "seed {seed:#x} mask {mask}: cell {i} rhs {} want {want}", removed[i]);
        }
        for l in sums.keys() {
            let residual: f64 = (0..cell_len()).filter(|&i| labels[i] == Some(*l)).map(|i| f64::from(removed[i])).sum();
            assert!(residual.abs() < 1e-3, "seed {seed:#x} mask {mask}: pocket {l} sums to {residual} after its mean is removed");
        }
        let h = f64::from(lattice().cell_size);
        let flux = f64::from(f32::from_bits(words[4]));
        assert!((flux - total * h * h * h).abs() <= 1e-4 * (total * h * h * h).max(1e-9), "seed {seed:#x} mask {mask}: removed flux {flux}, want {}", total * h * h * h);
        assert_eq!(words[5], 0, "the tick's first step clears the density word");
        assert_eq!(&words[..4], &[9, 9, 9, 9], "the other solver words stay");
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
        println!("GPU FLIP pockets seed {seed:#x} mask {mask}: {} isolated cells, {} pockets, {rounds} rounds", isolated.iter().filter(|&&s| s).count(), sums.len());
    }
    assert!(zeroed > 0, "some pocket was sealed");
}

/// Pocket sweeps' original triples and appended replay range agree through
/// changed, quiet and inactive rounds, even with a stale active range.
#[test]
fn gpu_flip_pocket_round_initializes_replay_gate() {
    fn write(pass: &Pass, binding: u32, values: &[u32]) {
        let buffer = &pass.bound.iter().find(|(b, _)| *b == binding).expect("bound").1;
        assert_eq!(buffer.size, (values.len() * 4) as u64);
        // SAFETY: each Pass::run retired its command buffer before this write.
        unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    }
    let params = lattice();
    let words = POCKET_GATE_WORDS as usize;
    assert_eq!(words, 17, "eleven original words, pad, two replay range words and setup triple");
    let mut clock = [0u32; 12];
    clock[0] = (1.0f32 / 60.0).to_bits();
    clock[11] = 1;
    let mut pass = Pass::new();
    pass.bind(24, &vec![73u32; words]).bind(46, &clock);
    let mut expected = vec![73u32; words];
    expected[9] = 1;
    expected[10] = 0;
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14..17].copy_from_slice(&[0, 1, 1]);
    assert_eq!(pass.run::<u32>("pocket_start", &params, 24, words, 1), expected, "active start clears the range and preserves original triple words");
    let lines = [N[1] * N[2], N[2] * N[0], N[0] * N[1]];
    for (axis, count) in lines.into_iter().enumerate() {
        expected[3 * axis..3 * axis + 3].copy_from_slice(&[count.div_ceil(256) as u32, 1, 1]);
    }
    expected[9] = 0;
    expected[12..14].copy_from_slice(&[0, 4]);
    expected[14] = 1;
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), expected, "changed round enables three sweeps and next setup");
    for axis in 0..3 { expected[3 * axis] = 0; }
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14] = 0;
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), expected, "quiet round enables no sweep");

    // An inactive slot always starts before its rounds. Start clears the
    // preceding active slot's range; each inactive round keeps it cleared.
    let mut stale = vec![73u32; words];
    stale[9] = 1;
    stale[12..14].copy_from_slice(&[0, 4]);
    stale[14..17].copy_from_slice(&[1, 1, 1]);
    write(&pass, 24, &stale);
    clock[0] = 0;
    write(&pass, 46, &clock);
    for axis in 0..3 { stale[3 * axis] = 0; }
    stale[9] = 0;
    stale[10] = 0;
    stale[12..14].copy_from_slice(&[0, 0]);
    stale[14] = 0;
    assert_eq!(pass.run::<u32>("pocket_start", &params, 24, words, 1), stale, "inactive start clears stale execution and retains original word semantics");
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), stale, "inactive round preserves the cleared execution range");
}

#[test]
fn gpu_flip_pocket_spread_reports_an_unfinished_cap() {
    // Every cell water and every link open, air only past the open -X face:
    // with no round run, sealed cells still link to air.
    let params = StepParams { closed_faces: 63 & !1, ..lattice() };
    let water = vec![1.0f32; cell_len()];
    let open = solid_faces(0x5e9, false);
    let mut pass = Pass::new();
    pass.bind(6, &water).bind(10, &open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; POCKET_GATE_WORDS as usize]).bind(25, &vec![0u32; cell_len()]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 1, "an unfinished spread is flagged");
    // The step's dry, sealed and air counts follow in words 7-9.
    let pocket = pass.bound::<u32>(23, cell_len());
    let counts: Vec<u32> = (0..3).map(|k| pocket.iter().filter(|&&s| s == k).count() as u32).collect();
    assert_eq!(counts, [0, (cell_len() - N[1] * N[2]) as u32, (N[1] * N[2]) as u32], "every cell water, the -X layer touching air");
    pass.bind(1, &vec![0u32; 2 * cell_len()]).bind(7, &vec![0.0f32; cell_len()]);
    pass.bind(22, &[5u32, 6, 7, 9, 0, 0, 0, 77, 77, 77, 77, 77, 77, 77, 77, 77, 77]);
    let first: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 0, ..params }, 22, 17, 1);
    assert_eq!(first[..4], [5, 6, 7, 1], "the tick's first step sets the word");
    assert_eq!(first[7..10], counts[..], "the step's dry, sealed and air cells");
    assert_eq!(first[10..14], [0, u32::MAX, 0, 0], "the lowest seed is cell 0, through the open -X box face");
    assert_eq!(first[16], 0, "every cell water: no dry floor hole");
    let second: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 1, ..params }, 22, 17, 1);
    assert_eq!(second[..4], [5, 6, 7, 2], "later steps add to it");
    assert_eq!(super::gpu_flip_step::pocket_rounds([6, 5, 4]), 6, "the cap is the longest side");
}

/// One region row over a box of whole cells, `lo` to `lo + cells`, its
/// lattice holding −1 everywhere, so a point is inside exactly when it is in
/// the box (sites sit at quarter cells, never on a face); its shape and atlas.
fn region_box(code: f32, lo: [usize; 3], cells: [u32; 3], velocity: [f32; 3]) -> (LiquidBody, LiquidShape, Vec<u32>) {
    let dims = cells.map(|c| c + 1);
    let mut atlas = Vec::new();
    pack_distance_atlas(&vec![-1.0; dims.iter().product::<u32>() as usize], &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [0.0, 0.0, 0.0, H],
        dims_x: dims[0],
        dims_y: dims[1],
        dims_z: dims[2],
        atlas_offset: 0,
        scale_min: [1.0; 4],
    };
    let p: [f32; 3] = std::array::from_fn(|a| MIN[a] + lo[a] as f32 * H);
    let row = LiquidBody {
        position_inv_mass: [p[0], p[1], p[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        angular_velocity: [0.0, 0.0, 0.0, code],
        inv_inertia_x: [velocity[0], velocity[1], velocity[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, 0.0],
        ..LiquidBody::default()
    };
    (row, shape, atlas)
}

fn in_box(x: [f64; 3], lo: [usize; 3], cells: [u32; 3]) -> bool {
    (0..3).all(|a| {
        let q = (x[a] - f64::from(MIN[a])) / f64::from(H);
        q >= lo[a] as f64 && q <= (lo[a] + cells[a] as usize) as f64
    })
}

fn region_params() -> StepParams {
    StepParams { region_count: 1, region_rows: 1, shapes_len: 1, ..lattice() }
}

/// An inflow fills (fluidsimulation.cpp 8566-8603, 8771-8811): every empty
/// half-cell site inside it and outside the solid takes one particle at rest
/// in the inflow's velocity, written after the live prefix in site order; an
/// occupied site and a site past the pool's slots take none.
#[test]
fn gpu_flip_inflow_emits_at_empty_sites_into_free_slots() {
    let (lo, cells, velocity) = ([1, 2, 1], [3, 2, 2], [0.4, -1.5, 0.2]);
    let (row, shape, atlas) = region_box(2.0, lo, cells, velocity);
    let site = |j: [usize; 3]| -> [f64; 3] { std::array::from_fn(|a| f64::from(MIN[a]) + (0.25 + 0.5 * j[a] as f64) * f64::from(H)) };
    let sites: [usize; 3] = N.map(|n| 2 * n);
    let site_count: usize = sites.iter().product();
    let mut live: Vec<FluidParticle> =
        random_particles(0xe31, 120).into_iter().filter(|p| p.position_radius[3] > 0.0).collect();
    // One particle on an inflow site, so the proof sees a taken site skipped.
    let held = site([2 * lo[0] + 1, 2 * lo[1], 2 * lo[2] + 1]);
    live.push(FluidParticle { position_radius: [held[0] as f32, held[1] as f32, held[2] as f32, 0.08], velocity: [0.0; 3], id: 999 });
    let (sorted, mut ranges) = cpu_sort(&live);
    // The sorter gives an empty cell the running start too (write_ranges), so
    // the last cell's end is the live count even when that cell is empty.
    let mut end = 0;
    for r in &mut ranges {
        r.start = end;
        end += r.count;
    }
    let taken = |j: [usize; 3]| {
        sorted.iter().any(|p| (0..3).all(|a| ((2.0 * (p.position_radius[a] - MIN[a]) / H).floor() as i64) == j[a] as i64))
    };
    let mut want = Vec::new();
    for idx in 0..site_count {
        let j = [idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])];
        want.push(u32::from(in_box(site(j), lo, cells) && !taken(j)));
    }
    let flagged = want.iter().filter(|&&f| f == 1).count();
    let inside = (0..site_count).filter(|&idx| {
        in_box(site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]), lo, cells)
    });
    let inside = inside.count();
    assert!(inside > flagged && flagged > 0, "the fixture holds taken and empty inflow sites: {flagged} of {inside} empty");
    let corners = vec![1.0f32; face_len()];
    let params = region_params();
    let got: Vec<u32> = Pass::new()
        .bind(1, &ranges)
        .bind(2, &sorted)
        .bind(9, &corners)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .run("emit_flags", &params, 37, site_count, site_count);
    assert_eq!(got, want, "the flags are the empty inflow sites");

    // Two short of room: a full pool emits fewer, never errors.
    let capacity = sorted.len() + flagged - 2;
    let scan: Vec<u32> = want.iter().scan(0, |sum, &f| { *sum += f; Some(*sum) }).collect();
    let mut pool = sorted.clone();
    pool.resize(capacity + 4, FluidParticle::default());
    let params = StepParams { capacity: capacity as u32, ..params };
    let written: Vec<FluidParticle> = Pass::new()
        .bind(1, &ranges)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .bind(37, &scan)
        .bind(38, &pool)
        .bind(47, &[0u32, 0, 1000, 0])
        .run("emit_write", &params, 38, pool.len(), site_count);
    assert_eq!(written[..sorted.len()], sorted[..], "the live prefix is untouched");
    assert!(written[capacity..].iter().all(|p| *p == FluidParticle::default()), "nothing past the pool's slots");
    let emitted: Vec<usize> = (0..site_count).filter(|&idx| want[idx] == 1).collect();
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]);
        let p = written[slot];
        for a in 0..3 {
            close(p.position_radius[a], x[a], 1.0, "emitted position");
            close(p.velocity[a], f64::from(velocity[a]), 1.0, "emitted velocity");
        }
        close(p.position_radius[3], 0.31017524 * f64::from(H), 1.0, "emitted radius");
        assert_eq!(p.id, 1000 + (slot - sorted.len()) as u32);
    }
    let live_after = written.iter().filter(|p| p.position_radius[3] > 0.0).count();
    assert_eq!(live_after, capacity, "the count is the live prefix plus the emitted, up to the pool");

    // Jitter factor 1, the region a full cell deep everywhere: each emitted
    // particle moves uniformly up to a quarter cell each way, keyed by site
    // and substep (_jitterMarkerParticlePosition).
    // first_tick 3 too: the one region row is the tick's.
    let params = StepParams { emit_jitter: 0.25, tick_index: 3, first_tick: 3, step_in_tick: 1, ..params };
    let jittered: Vec<FluidParticle> = Pass::new()
        .bind(1, &ranges)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .bind(37, &scan)
        .bind(38, &pool)
        .bind(47, &[0u32, 0, 1000, 0])
        .run("emit_write", &params, 38, pool.len(), site_count);
    let substep = 3u32 * 64 + 1;
    let mut spread = 0.0f64;
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]);
        let key = (idx as u32).wrapping_mul(3).wrapping_add(substep.wrapping_mul(2_654_435_761));
        for (a, &site_a) in x.iter().enumerate() {
            let unit = f64::from(cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) / 16_777_216.0;
            let want = site_a + f64::from(H) * 0.25 * (2.0 * unit - 1.0);
            close(jittered[slot].position_radius[a], want, 1.0, "jittered position");
            spread = spread.max((want - site_a).abs() / f64::from(H));
        }
    }
    println!("emit proof: {inside} inflow sites, {flagged} empty, {} written to a pool {} short, jitter spread {spread:.4} cells", capacity - sorted.len(), flagged + sorted.len() - capacity);
    assert!(spread > 0.2 && spread <= 0.25, "the draw reaches near a quarter cell and never past it: {spread}");
}

/// An outflow empties (fluidsimulation.cpp 9011-9031): a particle whose new
/// position its distance holds below zero dies; every other keeps its radius.
#[test]
fn gpu_flip_outflow_kills_the_particles_it_holds() {
    let (lo, cells) = ([2, 1, 0], [3, 3, 2]);
    let (row, shape, atlas) = region_box(3.0, lo, cells, [0.0; 3]);
    let particles: Vec<FluidParticle> = random_particles(0xd7a, 300)
        .into_iter()
        .map(|mut p| {
            p.velocity = [0.0; 3];
            p
        })
        .collect();
    // Still faces: nothing moves, so the new position is the old one.
    let faces = vec![FaceSample::default(); face_len()];
    let step = StepParams { step_dt: 0.02, flip: 1.0, particles: particles.len() as u32, ..region_params() };
    let got: Vec<FluidParticle> = Pass::new()
        .bind(2, &particles)
        .bind(3, &faces)
        .bind(9, &wall_solid(N, H))
        .bind(17, &faces)
        .bind(18, &faces)
        .bind(22, &vec![0u32; 2 * particles.len()])
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .run("faces_to_particles", &step, 19, particles.len(), particles.len());
    let mut drained = 0;
    for (i, (g, p)) in got.iter().zip(&particles).enumerate() {
        if p.position_radius[3] <= 0.0 {
            assert_eq!(g.position_radius[3], p.position_radius[3], "unused slot {i} stays unused");
            continue;
        }
        let x: [f64; 3] = std::array::from_fn(|a| f64::from(g.position_radius[a]));
        let dies = in_box(x, lo, cells);
        drained += usize::from(dies);
        let want = if dies { 0.0 } else { p.position_radius[3] };
        assert_eq!(g.position_radius[3], want, "particle {i} at {x:?}");
    }
    let alive = got.iter().filter(|p| p.position_radius[3] > 0.0).count();
    let before = particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
    println!("drain proof: {drained} drained of {} live", particles.iter().filter(|p| p.position_radius[3] > 0.0).count());
    assert!(drained > 10, "the drain holds a share of the draw: {drained}");
    assert_eq!(alive, before - drained, "the live count drops by exactly the drained");
}

/// The fine pockets' states and labels after a settled spread over `water`
/// and `open`, every tank face closed.
fn fine_pockets(water: &[f32], open: &[FaceSample]) -> (Vec<u32>, Vec<u32>) {
    let params = lattice();
    let lines = (N[1] * N[2]).max(N[0] * N[2]).max(N[0] * N[1]);
    let mut pass = Pass::new();
    pass.bind(6, water).bind(10, open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; POCKET_GATE_WORDS as usize]).bind(25, &vec![0u32; cell_len()]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    for _ in 0..=cell_len() {
        pass.run::<u32>("pocket_round", &params, 24, POCKET_GATE_WORDS as usize, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            pass.run::<u32>(sweep, &params, 24, POCKET_GATE_WORDS as usize, lines);
        }
        if pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[9] == 0 {
            break;
        }
    }
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 0, "a settled spread leaves no sealed cell linked to air");
    (pass.bound(23, cell_len()), pass.bound(25, cell_len()))
}

/// The pockets at Solve Level 1 as the CPU coarsens them: a coarse cell is
/// sealed when every in-lattice child that is not solid is sealed under one
/// fine label (a solid child is skipped, as the solver coarsens it), and
/// takes the lowest coarse cell of that fine label as its own; (state,
/// label) per coarse cell, the label of a dry cell unspecified (None).
fn cpu_coarse_pockets(labels: &[Option<usize>], solid: &[bool], c: [usize; 3]) -> Vec<Option<usize>> {
    let coarse_len = c.iter().product::<usize>();
    let fine_label: Vec<Option<usize>> = (0..coarse_len)
        .map(|i| {
            let at = [i % c[0], (i / c[0]) % c[1], i / (c[0] * c[1])];
            let mut shared = None;
            for child in 0..8 {
                let q = [2 * at[0] + (child & 1), 2 * at[1] + (child >> 1 & 1), 2 * at[2] + (child >> 2)];
                if (0..3).any(|a| q[a] >= N[a]) || solid[cell_index(q)] {
                    continue;
                }
                match (labels[cell_index(q)], shared) {
                    (None, _) => return None,
                    (Some(l), None) => shared = Some(l),
                    (Some(l), Some(s)) if l != s => return None,
                    _ => {}
                }
            }
            shared
        })
        .collect();
    (0..coarse_len)
        .map(|i| fine_label[i].map(|l| (0..coarse_len).find(|&j| fine_label[j] == Some(l)).expect("itself at the latest")))
        .collect()
}

/// At Solve Level 1 the pockets are coarsened with the water
/// (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)): a
/// level cell is sealed when all its fine children are sealed under one
/// label, its label the lowest level cell of that pocket, and the mean
/// comes off the coarse right-hand side pocket by pocket so each sums to
/// zero again; what it removes is h³ at the coarse cell size. A tank of
/// water, whole; split by a closed plane on a coarse boundary (two coarse
/// pockets); split off it (the straddling coarse cells stay dry); with one
/// solid cell (not water, every face closed) inside the first coarse cell,
/// which stays sealed with its seven water children.
#[test]
fn gpu_flip_pocket_mean_at_solve_level_one_sums_to_zero() {
    let coarse = N.map(|n| n.div_ceil(2));
    assert_eq!(coarse, [3, 3, 2]);
    let coarse_len = coarse.iter().product::<usize>();
    let mut sealed_coarse_total = 0;
    for (case, split_x, solid_cell) in [
        ("whole", None, None),
        ("split on the coarse boundary", Some(4), None),
        ("split through a coarse cell", Some(3), None),
        ("a solid cell inside a coarse cell", None, Some([1, 1, 1])),
    ] {
        let mut water = vec![1.0f32; cell_len()];
        let mut open = solid_faces(0x5ea, false);
        if let Some(x) = split_x {
            for (i, face) in open.iter_mut().enumerate() {
                if pad_coords(i)[0] == x {
                    face.weight[0] = 0.0;
                }
            }
        }
        if let Some(c) = solid_cell {
            water[cell_index(c)] = 0.0;
            for (i, face) in open.iter_mut().enumerate() {
                let p = pad_coords(i);
                for a in 0..3 {
                    if (0..3).all(|b| b == a || p[b] == c[b]) && (p[a] == c[a] || p[a] == c[a] + 1) {
                        face.weight[a] = 0.0;
                    }
                }
            }
        }
        let solid: Vec<bool> = (0..cell_len()).map(|i| solid_cell == Some(cell_coords(i))).collect();
        let (state, label) = fine_pockets(&water, &open);
        let (_, sealed) = cpu_isolated(&water, &open, 63);
        assert!((0..cell_len()).all(|i| sealed[i] || solid[i]), "{case}: every water cell is sealed");
        let fine_labels = cpu_pocket_labels(&sealed, &open);
        for i in 0..cell_len() {
            if solid[i] {
                assert_ne!(state[i], 1, "{case}: the solid cell {i} is no pocket");
                continue;
            }
            assert_eq!(state[i], 1, "{case}: fine cell {i} sealed");
            assert_eq!(Some(label[i] as usize), fine_labels[i], "{case}: fine cell {i} label");
        }
        let params = StepParams { solve_level: 1, ..lattice() };
        let mut pass = Pass::new();
        pass.bind(6, &water).bind(10, &open).bind(23, &state).bind(25, &label).bind(39, &vec![7u32; coarse_len]).bind(40, &vec![7u32; coarse_len]).bind(41, &vec![0u32; cell_len()]);
        pass.run::<u32>("pocket_leader_clear", &params, 41, cell_len(), cell_len());
        pass.run::<u32>("pocket_coarsen", &params, 39, coarse_len, coarse_len);
        let got_label = pass.run::<u32>("pocket_relabel", &params, 40, coarse_len, coarse_len);
        let got_state = pass.bound::<u32>(39, coarse_len);
        let want = cpu_coarse_pockets(&fine_labels, &solid, coarse);
        if solid_cell.is_some() {
            assert!(want[0].is_some(), "{case}: the coarse cell over the solid cell is sealed");
        }
        for i in 0..coarse_len {
            assert_eq!(got_state[i] == 1, want[i].is_some(), "{case}: coarse cell {i} sealed");
            if let Some(l) = want[i] {
                assert_eq!(got_label[i] as usize, l, "{case}: coarse cell {i} label");
            }
        }
        let sealed_count = want.iter().flatten().count();
        sealed_coarse_total += sealed_count;
        let pockets: std::collections::BTreeSet<usize> = want.iter().flatten().copied().collect();
        println!("coarse pockets {case}: {sealed_count} of {coarse_len} coarse cells sealed in {} pockets {pockets:?}", pockets.len());
        // The mean off the coarse right-hand side, on the coarse lattice at
        // its cell size, the flux into the pressure word of a later substep.
        let mut rng = Stream::new(0x5eb);
        let rhs: Vec<f32> = (0..coarse_len).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let coarse_params = StepParams { n: coarse.map(|n| n as u32), cell_size: 2.0 * H, step_in_tick: 1, ..params };
        let mut mean = Pass::new();
        mean.bind(23, &got_state).bind(25, &got_label).bind(5, &rhs).bind(26, &vec![7u32; 3 * coarse_len + 2]).bind(22, &[9u32; 6]);
        mean.run::<u32>("pocket_clear", &coarse_params, 26, 3 * coarse_len + 2, 3 * coarse_len + 2);
        mean.run::<u32>("pocket_accumulate", &coarse_params, 26, 3 * coarse_len + 2, coarse_len);
        let removed = mean.run::<f32>("pocket_remove", &coarse_params, 5, coarse_len, coarse_len);
        let words = mean.run::<u32>("pocket_flux_pressure", &coarse_params, 22, 6, 1);
        let mut total = 0.0;
        for &l in &pockets {
            let members: Vec<usize> = (0..coarse_len).filter(|&i| want[i] == Some(l)).collect();
            let sum: f64 = members.iter().map(|&i| f64::from(rhs[i])).sum();
            total += sum.abs();
            for &i in &members {
                let want_value = f64::from(rhs[i]) - sum / members.len() as f64;
                assert!((f64::from(removed[i]) - want_value).abs() <= 1e-4, "{case}: coarse cell {i} rhs {} want {want_value}", removed[i]);
            }
            let left: f64 = members.iter().map(|&i| f64::from(removed[i])).sum();
            assert!(left.abs() < 1e-3, "{case}: coarse pocket {l} sums to {left} after its mean is removed");
        }
        for i in (0..coarse_len).filter(|&i| want[i].is_none()) {
            assert_eq!(removed[i], rhs[i], "{case}: dry coarse cell {i} keeps its value");
        }
        let h = 2.0 * f64::from(H);
        let flux = f64::from(f32::from_bits(words[4])) - f64::from(f32::from_bits(9));
        let want_flux = total * h * h * h;
        assert!((flux - want_flux).abs() <= 1e-4 * want_flux.max(1e-9), "{case}: removed flux {flux}, want {want_flux} at the coarse cell size");
    }
    assert!(sealed_coarse_total > 0);
}

#[test]
fn gpu_flip_step_order_extend_constraint_value_proof() {
    use super::gpu_flip_extension_tests::{cpu_extend, moving_wall_fixture};
    let (initial, wall) = moving_wall_fixture(N);
    assert_eq!(initial.len(), face_len());
    for cfl in [1, 3, 5, 8] {
        let mut want = initial.clone();
        let mut got = initial.clone();
        for _ in 0..super::gpu_flip_step::band_layers(cfl) {
            want = cpu_extend(&want, N);
            got = Pass::new().bind(3, &got)
                .run("extend_faces", &lattice(), 4, face_len(), face_len());
        }
        let mut open = vec![FaceSample { weight: [1.0; 4], ..FaceSample::default() }; face_len()];
        let mut moving = vec![FaceSample::default(); face_len()];
        open[wall].weight[0] = 0.5;
        moving[wall].weight[0] = 0.5;
        moving[wall].velocity[0] = 7.0;
        let open64: Vec<f64> = bytemuck::cast_slice::<_, f32>(&open).iter().map(|&x| f64::from(x)).collect();
        let moving64: Vec<f64> = bytemuck::cast_slice::<_, f32>(&moving).iter().map(|&x| f64::from(x)).collect();
        let expected = cpu_constrain(bytemuck::cast_slice(&want), &open64, &moving64, N);
        let constrained: Vec<FaceSample> = Pass::new().bind(20, &got).bind(10, &open).bind(11, &moving)
            .run("constrain_solid_faces", &lattice(), 20, face_len(), face_len());
        assert_close(bytemuck::cast_slice(&constrained), &expected, "extend then constrain moving wall");
        // The wall face's z + 1 neighbour is this lattice's held border row:
        // the extension averages its 0 with the seed's 2.
        assert_eq!(constrained[wall].velocity[0], 4.0);
    }
}

#[test]
fn gpu_flip_step_order_cell_cap_compacts_preserving_ids() {
    use super::sort_particles_into_cells::{ParticleSorter, SortJob, LIQUID_PARTICLE_READ, SortLabels};
    use super::prefix_scan::ScanLabels;
    let particles: Vec<_> = (0..300).map(|i| FluidParticle {
        position_radius: [MIN[0] + 2.25 * H, MIN[1] + 2.25 * H, MIN[2] + 2.25 * H, 0.05],
        velocity: [1.0, 0.0, 0.0], id: 1000 + i,
    }).collect();
    let (mut sorted, ranges) = cpu_sort(&particles);
    // The clock removed an extreme marker, but its pre-removal cell rank
    // must still consume a quota place, exactly as the native loop does.
    sorted[0].position_radius[3] = 0.0;
    assert_eq!(ranges.len(), cell_len());
    assert_eq!(ranges.iter().map(|r| r.count).sum::<u32>(), 300);
    let mut pass = Pass::new();
    let removed: Vec<FluidParticle> = pass.bind(1, &ranges).bind(38, &sorted)
        .run("remove_crowded_markers", &lattice(), 38, 300, cell_len());
    let mut speeds = [1.0; 300];
    speeds[0] = 1000.0;
    let want = super::gpu_flip_extension_tests::native_cell_survivors(&speeds, 6.0);
    for (i, p) in removed.iter().enumerate() {
        assert_eq!(p.position_radius[3] > 0.0, want.contains(&i));
        assert_eq!(p.id, sorted[i].id);
    }
    let input = &pass.bound.iter().find(|(b, _)| *b == 38).unwrap().1;
    let output = pass.device.create_buffer_shared(300 * 32);
    let mut sorter = ParticleSorter::default();
    sorter.prepare(&pass.device);
    sorter.reserve_ranges(&pass.device, N.map(|n| n as u32)).unwrap();
    let mut enc = pass.device.create_encoder("step order compact proof");
    let labels = SortLabels { clear: "clear", count: "count",
        scan: ScanLabels { blocks: "blocks", add: "add" }, ranges: "ranges",
        tail: "tail", scatter: "scatter", stabilise: "stable" };
    sorter.encode(&pass.device, &mut enc, &SortJob {
        particles: input, read: LIQUID_PARTICLE_READ, capacity: 300, count: 300,
        bin_min: MIN, inv_cell: 1.0 / H, bins: N.map(|n| n as u32),
        sorted: Some(&output), order: None, gate: None,
    }, &labels).unwrap();
    enc.commit_and_wait_completed();
    let compacted: Vec<FluidParticle> = read(&output, 300);
    assert_eq!(&compacted[..249], &particles[1..250]);
    assert!(compacted[249..].iter().all(|p| p.position_radius[3] == 0.0));
}

#[test]
fn gpu_flip_step_order_inflow_waits_until_next_step() {
    use super::gpu_flip_step::GpuFlipStep;
    use crate::node_graph::primitive::Primitive;
    let capacity = 512;
    // Extent arithmetic before device creation: 120 cell ranges, 210 face
    // records, 960 emit sites, 512 particle records, two counters per slot.
    assert_eq!((cell_len(), face_len()), (120, 210));
    assert_eq!(super::gpu_flip_step::emit_sites(N.map(|n| n as u32)), 960);
    let mut h = Harness::new();
    let mut node = GpuFlipStep::new();
    node.prepare_pipelines(&h.device);
    let particles = h.array::<FluidParticle>(&[], capacity);
    let capped = h.array::<u32>(&[], 2 * capacity + super::liquid_stats::SOLVER_WORDS as usize);
    // The step solves on the native grid: three more cells, its origin 1.5
    // cells outside the box.
    let solver = N.map(|n| n + 3);
    let solver_min: [f32; 3] = std::array::from_fn(|a| MIN[a] - 1.5 * H);
    let solver_faces = solver.map(|n| n + 1).iter().product::<usize>();
    let faces = h.array::<FaceSample>(&[], solver_faces);
    let (row, shape, atlas) = region_box(2.0, [2, 2, 1], [1, 1, 1], [0.25, 0.0, 0.0]);
    let regions = h.array(&[row], 1);
    let shapes = h.array(&[shape], 1);
    let atlas = h.array(&atlas, atlas.len());
    let bodies = h.array::<LiquidBody>(&[], 1);
    let pad = PADDING_NODES as f32;
    let mut p = params(&[
        ("nodes_x", N[0] as f32 + 1.0 + 2.0 * pad),
        ("nodes_y", N[1] as f32 + 1.0 + 2.0 * pad),
        ("nodes_z", N[2] as f32 + 1.0 + 2.0 * pad),
        ("lattice_min_x", MIN[0] - pad * H), ("lattice_min_y", MIN[1] - pad * H),
        ("lattice_min_z", MIN[2] - pad * H), ("cell_size", H),
        ("gravity_y", 0.0), ("interval_duration", 0.125), ("volume_projection", 0.0),
        ("region_count", 1.0), ("iterations", 0.0), ("flip", 1.0),
    ]);
    // liquid_state's birth identity as its reset seeds it: next id, epoch,
    // reserved base, full-reset request.
    let identity = h.array::<u32>(&[1, 1, 0, 0], 4);
    let inputs = [("particles", particles.0), ("regions", regions.0),
        ("shapes", shapes.0), ("atlas", atlas.0), ("bodies", bodies.0), ("identity", identity.0)];
    let outputs = [("out", particles.0), ("faces", faces.0), ("capped", capped.0)];
    let (_, errors) = h.run(&mut node, &inputs, &outputs, &p);
    assert!(errors.is_empty(), "{errors:?}");
    let first: Vec<FluidParticle> = read(&particles.1, capacity);
    let alive: Vec<_> = first.iter().filter(|p| p.position_radius[3] > 0.0).copied().collect();
    assert_eq!(alive.len(), 8);
    for marker in &alive {
        assert_eq!(marker.velocity, [0.25, 0.0, 0.0]);
        let q = (marker.position_radius[0] - MIN[0]) / H;
        assert!(q == 2.25 || q == 2.75, "fresh inflow moved: {q}");
    }
    p.insert("tick_index".into(), ParamValue::Float(1.0));
    p.insert("first_tick".into(), ParamValue::Float(1.0));
    let (_, errors) = h.run(&mut node, &inputs, &outputs, &p);
    assert!(errors.is_empty(), "{errors:?}");
    let second: Vec<FluidParticle> = read(&particles.1, capacity);
    let field: Vec<FaceSample> = read(&h.buffer(faces.0), solver_faces);
    assert!(alive.iter().any(|before| second.iter().any(|after|
        after.id == before.id && after.position_radius[0] > before.position_radius[0] + 1e-6)),
        "the first inflow must move on the second step");
    // Native RK3 on the second step's published field, with no density move.
    for before in &alive {
        let after = second.iter().find(|p| p.id == before.id && p.position_radius[3] > 0.0).unwrap();
        let q = std::array::from_fn(|a| f64::from((before.position_radius[a] - solver_min[a]) / H));
        let v1 = cpu_sample_on(q, &field, solver);
        let v2 = cpu_sample_on(std::array::from_fn(|a| q[a] + 0.5 * 0.125 / f64::from(H) * v1[a]), &field, solver);
        let v3 = cpu_sample_on(std::array::from_fn(|a| q[a] + 0.75 * 0.125 / f64::from(H) * v2[a]), &field, solver);
        for a in 0..3 {
            let expected = f64::from(before.position_radius[a]) + 0.125 *
                (2.0 / 9.0 * v1[a] + 3.0 / 9.0 * v2[a] + 4.0 / 9.0 * v3[a]);
            close(after.position_radius[a], expected, 1.0, "second-step inflow RK3");
        }
    }
}
