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

use manifold_node_engine::testkit::atom::{FACE_FLOATS, assert_close, face_grid_len, random_values};
use super::gpu_flip_step::{POCKET_GATE_WORDS, StepParams, dispatch_pass, tile_total};
use super::liquid_fill::LiquidFill;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::primitives::liquid_stats::{INFLOW_BIRTHS_TAIL, with_stats_layout};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::fluid_particles::{CellRange, FaceSample};
use manifold_water_liquid::bodies::{BodySupports, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape, SUPPORT_VEC4S, body_pose_at, pack_distance_atlas, pack_supports, unpack_supports};
use manifold_water_liquid::coupling::coupled_start;
use manifold_physics::coupled_motion::{Held, MAX_SUPPORT_POINTS, Mobility, SupportPoint, constrained_mobility, coupled_state_at};
mod cells;
mod density;
mod emit;
mod face_gravity;
mod motion;
mod pockets;
mod solids;
mod step_order;
pub(super) use motion::cpu_sample_on;
use manifold_nodes_physics::force_lattice::FORCE_LATTICE;
use manifold_water_liquid::lattice::PADDING_NODES;
use manifold_node_engine::parameters::ParamValue;

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

/// One entry of the step's shader with its buffers bound by number.
struct Pass {
    device: manifold_gpu::testkit::TestDevice,
    bound: Vec<(u32, GpuBuffer)>,
}

impl Pass {
    fn new() -> Self {
        Self { device: manifold_gpu::testkit::test_device(), bound: Vec::new() }
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

