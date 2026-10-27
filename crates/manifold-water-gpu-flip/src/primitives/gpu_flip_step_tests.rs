//! GPU value proofs for node.gpu_flip_step's particle, face and solid passes
//! Each entry of the
//! step's shader run on its own against a CPU f64 reference.
//!
//! The CPU references port FLIP Fluids rules (MIT, Copyright (C) 2026 Ryan L.
//! Guy & Dennis Fassbaender; see THIRD_PARTY_NOTICES.md): velocityadvector.cpp
//! (the Wyvill sum), particlelevelset.cpp (the particle distance),
//! levelsetutils.cpp and meshlevelset.cpp (open fractions),
//! fluidsimulation.cpp (solid face velocity, the constraint),
//! pressuresolver.cpp (the pressure subtraction).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuDevice};

use super::gpu_flip_step::{POCKET_GATE_WORDS, StepParams, dispatch_pass};
use super::liquid_fill::LiquidFill;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::atom::{
    FACE_FLOATS, assert_close, face_grid_len, random_values,
};
use manifold_physics::coupled_motion::{
    Held, MAX_SUPPORT_POINTS, Mobility, SupportPoint, constrained_mobility, coupled_state_at,
};
use manifold_water_liquid::bodies::{
    BodySupports, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape, SUPPORT_VEC4S,
    body_pose_at, pack_distance_atlas, pack_supports, unpack_supports,
};
use manifold_water_liquid::coupling::coupled_start;
use manifold_water_liquid::fluid_particles::{CellRange, FaceSample};
use manifold_water_liquid::primitives::liquid_stats::{INFLOW_BIRTHS_TAIL, with_stats_layout};
use manifold_water_liquid::tile_storage::testkit::{
    from_pool, identity_table, records_from_pool, records_from_pool_with, records_to_pool, to_pool,
};
use manifold_water_liquid::tile_storage::{pool_len, tile_total};
mod cells;
mod density;
mod emit;
mod face_gravity;
mod motion;
mod pockets;
mod slots;
mod solids;
mod step_order;

use emit::*;
use manifold_node_engine::parameters::ParamValue;
use manifold_nodes_physics::force_lattice::FORCE_LATTICE;
use manifold_water_liquid::lattice::PADDING_NODES;
pub(super) use motion::*;
use solids::*;

/// `count` elements of a buffer the step allocated (private storage), through
/// a staging copy.
pub(super) fn read_staged<T: bytemuck::Pod>(
    device: &GpuDevice,
    buffer: &GpuBuffer,
    count: usize,
) -> Vec<T> {
    let staged = device.create_buffer_shared(buffer.size);
    let mut enc = device.create_encoder("step proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait_completed();
    read(&staged, count)
}

/// The step's pooled `faces` output as the dense `(n+1)^3` record array,
/// read through its `tiles` output.
pub(super) fn step_faces(
    h: &Harness,
    faces: manifold_node_engine::bindings::Slot,
    tiles: manifold_node_engine::bindings::Slot,
    cells: [u32; 3],
) -> Vec<FaceSample> {
    let pool = h.buffer(faces);
    let table = h.buffer(tiles);
    let pool: Vec<FaceSample> = read_staged(
        &h.device,
        &pool,
        (pool.size / std::mem::size_of::<FaceSample>() as u64) as usize,
    );
    let table: Vec<u32> = read_staged(&h.device, &table, (table.size / 4) as usize);
    records_from_pool_with(&pool, &table, cells)
}

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
    StepParams {
        n: N.map(|n| n as u32),
        box_min: MIN,
        cell_size: H,
        impulse_tick: -1,
        closed_faces: 63,
        ..StepParams::default()
    }
}

fn wall_solid(n: [usize; 3], h: f32) -> Vec<f32> {
    let m = n.map(|v| v + 1);
    (0..m.iter().product())
        .map(|i| {
            let q = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
            let distance = (0..3)
                .map(|a| (q[a] as f32).min(n[a] as f32 - q[a] as f32))
                .fold(f32::INFINITY, f32::min);
            distance * h
        })
        .collect()
}

fn wall_phi(q: [f64; 3], n: [usize; 3]) -> f64 {
    (0..3)
        .map(|a| q[a].min(n[a] as f64 - q[a]))
        .fold(f64::INFINITY, f64::min)
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
    native_collision_move(
        q0,
        q1,
        n,
        h,
        |q| wall_phi(q, n),
        |q| wall_gradient(q, n).map(|v| v * h),
    )
}

// Source sequence without the GPU's conservative distance shortcut. phi is
// in cells; gradient holds the native unscaled lattice edge differences.
fn native_collision_move(
    q0: [f64; 3],
    mut q1: [f64; 3],
    n: [usize; 3],
    h: f64,
    phi: impl Fn([f64; 3]) -> f64,
    gradient: impl Fn([f64; 3]) -> [f64; 3],
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
                let pushed =
                    std::array::from_fn(|a| current[a] - (d - WALL_MARGIN_CELLS) * g[a] / g_len);
                let push_distance = (0..3)
                    .map(|a| (pushed[a] - current[a]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                if push_distance <= SOLID_PUSH_CELLS && phi(pushed) >= 0.0 {
                    kept = pushed;
                }
            }
            if !inside_boundary_cpu(kept, n, edge) {
                let original = kept;
                kept = clamp_boundary_cpu(kept, n, edge, h);
                let clamp_distance = (0..3)
                    .map(|a| (kept[a] - original[a]).powi(2))
                    .sum::<f64>()
                    .sqrt();
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

/// How a bound array is stored: a dense array as the CPU references hold it,
/// or a pooled field of the identity-allocated tile pool, which the harness
/// converts from and to the dense array. Cells are `n³`, records `(n + 1)³`
/// with the records past the lattice answered by the canonical block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Dense,
    Cells,
    Records,
}

/// The layout of an output the harness creates itself, by the step shader's
/// binding numbers.
fn output_layout(binding: u32) -> Layout {
    match binding {
        3 | 4 | 10 | 11 | 17 | 18 | 20 => Layout::Records,
        5..=8 | 23 | 25 | 33..=35 | 41 => Layout::Cells,
        _ => Layout::Dense,
    }
}

/// Bit a of the canonical mask of record `p` past the lattice `n`: face a
/// exists and sits on a box wall.
fn wall_mask(p: [usize; 3], n: [usize; 3]) -> usize {
    (0..3)
        .filter(|&a| {
            let mut other = p;
            other[a] = 0;
            (0..3).all(|b| other[b] < n[b]) && (p[a] == 0 || p[a] == n[a])
        })
        .fold(0, |mask, a| mask | (1 << a))
}

/// The 8 canonical records of a record field, read off the dense array's
/// records past the lattice. The pool stores none of them, so every one must
/// hold the record of its wall mask (only masks 0, 1, 2 and 4 occur there).
fn canonical_records(dense: &[FaceSample], cells: [u32; 3], binding: u32) -> [FaceSample; 8] {
    let n = cells.map(|v| v as usize);
    let at = |p: [usize; 3]| dense[p[0] + (n[0] + 1) * (p[1] + (n[1] + 1) * p[2])];
    let mut canon = [at(n); 8];
    canon[1] = at([n[0], 0, 0]);
    canon[2] = at([0, n[1], 0]);
    canon[4] = at([0, 0, n[2]]);
    for (i, record) in dense.iter().enumerate() {
        let p = [
            i % (n[0] + 1),
            (i / (n[0] + 1)) % (n[1] + 1),
            i / ((n[0] + 1) * (n[1] + 1)),
        ];
        if (0..3).all(|a| p[a] < n[a]) {
            continue;
        }
        let mask = wall_mask(p, n);
        assert_eq!(
            bytemuck::bytes_of(record),
            bytemuck::bytes_of(&canon[mask]),
            "binding {binding}: record {p:?} past the lattice differs from its canonical record \
             (mask {mask}); a pooled field stores no record there"
        );
    }
    canon
}

/// The records of a gathered face grid with nothing gathered: every face
/// absent but the box walls, closed (velocity 0, weight 1).
fn canonical_faces_on(n: [usize; 3]) -> Vec<FaceSample> {
    let m = n.map(|v| v + 1);
    (0..m.iter().product())
        .map(|i| {
            let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
            let mut face = FaceSample::default();
            for a in 0..3 {
                let mut other = p;
                other[a] = 0;
                if (0..3).all(|b| other[b] < n[b]) && (p[a] == 0 || p[a] == n[a]) {
                    face.weight[a] = 1.0;
                }
            }
            face
        })
        .collect()
}

fn canonical_faces() -> Vec<FaceSample> {
    canonical_faces_on(N)
}

/// `records` (32-byte records, one per padded cell of the lattice `n`) with
/// every record past the lattice zeroed: the pool stores one record per wall
/// mask there, so a fixture that wants the CPU reference and the pool to read
/// the same value gives them zero.
fn zero_past_lattice<T: bytemuck::Pod>(mut records: Vec<T>, n: [usize; 3]) -> Vec<T> {
    let m = n.map(|v| v + 1);
    let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut records);
    assert_eq!(
        bytes.len(),
        32 * m.iter().product::<usize>(),
        "one 32-byte record per padded cell"
    );
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        if (0..3).any(|a| p[a] >= n[a]) {
            bytes[32 * i..32 * (i + 1)].fill(0);
        }
    }
    records
}

/// One entry of the step's shader with its buffers bound by number. Pooled
/// arrays live in the identity-allocated tile pool (`P = T`, slot = tile) of
/// the lattice `cells`; the binding at 51 is the slot table.
struct Pass {
    device: manifold_gpu::testkit::TestDevice,
    cells: [u32; 3],
    bound: Vec<(u32, GpuBuffer)>,
    layouts: Vec<(u32, Layout)>,
}

impl Pass {
    fn new() -> Self {
        Self::on(N)
    }

    /// A pass over a lattice other than [`N`].
    fn on(cells: [usize; 3]) -> Self {
        Self {
            device: manifold_gpu::testkit::test_device(),
            cells: cells.map(|n| n as u32),
            bound: Vec::new(),
            layouts: Vec::new(),
        }
    }

    /// Tiles the pool holds.
    fn pool_tiles(&self) -> u32 {
        identity_table(self.cells).1
    }

    /// `params` over this pass's pool.
    fn pooled(&self, params: &StepParams) -> StepParams {
        StepParams {
            pool_tiles: self.pool_tiles(),
            ..*params
        }
    }

    /// Binds the identity slot table at 51 once.
    fn table(&mut self) {
        if !self.bound.iter().any(|(binding, _)| *binding == 51) {
            let (words, _) = identity_table(self.cells);
            self.bind(51, &words, Layout::Dense);
        }
    }

    fn layout(&self, binding: u32) -> Layout {
        self.layouts
            .iter()
            .find(|(b, _)| *b == binding)
            .expect("bound")
            .1
    }

    /// Binds a buffer holding `values` at `binding`, in `layout`: a pooled
    /// layout converts the dense array to the pool (records past the lattice
    /// must already hold their canonical records).
    fn bind<T: bytemuck::Pod>(&mut self, binding: u32, values: &[T], layout: Layout) -> &mut Self {
        let pooled: Vec<u8> = match layout {
            Layout::Dense => Vec::new(),
            Layout::Cells => {
                let pool = to_pool(values, self.cells, &[T::zeroed()]);
                bytemuck::cast_slice::<T, u8>(&pool).to_vec()
            }
            Layout::Records => {
                let records: Vec<FaceSample> = bytemuck::pod_collect_to_vec(values);
                let canon = canonical_records(&records, self.cells, binding);
                let pool = records_to_pool(&records, self.cells, &canon);
                bytemuck::cast_slice::<FaceSample, u8>(&pool).to_vec()
            }
        };
        let bytes: &[u8] = if layout == Layout::Dense {
            bytemuck::cast_slice::<T, u8>(values)
        } else {
            &pooled
        };
        let buffer = self
            .device
            .create_buffer_shared((bytes.len() as u64).max(16));
        buffer.zero_fill();
        if !bytes.is_empty() {
            // SAFETY: shared buffer sized for `bytes`; no GPU work in flight.
            unsafe { buffer.write(0, bytes) };
        }
        self.bound.push((binding, buffer));
        self.layouts.push((binding, layout));
        self
    }

    /// Binds the sort's ranges at 1 as the step reads them: one per cell at
    /// the cell's pool index, and the canonical block's last element ending
    /// at the live total (the last key's range ends there).
    fn bind_ranges(&mut self, ranges: &[CellRange]) -> &mut Self {
        let live = ranges.iter().map(|range| range.count).sum();
        let mut canon = [CellRange::default(); 8];
        canon[7] = CellRange {
            start: live,
            count: 0,
        };
        let pooled = to_pool(ranges, self.cells, &canon);
        self.bind(1, &pooled, Layout::Dense)
    }

    /// Binds the tile list at 29 as every tile in order, so a pass over the
    /// cell set C covers the whole lattice; returns its thread count (512 a
    /// tile).
    fn every_tile(&mut self) -> usize {
        let tiles: Vec<u32> = (0..tile_total(self.cells) as u32).collect();
        self.bind(29, &tiles, Layout::Dense);
        tiles.len() * 512
    }

    /// Binds every tile as the allocated list `extend_faces` walks: the list
    /// at 29 and its length in `tile_counts` (30) at `params.ring_max`;
    /// returns its thread count (512 a tile).
    fn allocated_tiles(&mut self, params: &StepParams) -> usize {
        let threads = self.every_tile();
        let mut counts = vec![0u32; params.ring_max as usize + 8];
        counts[params.ring_max as usize] =
            u32::try_from(tile_total(self.cells)).expect("the tile count fits a word");
        self.bind(30, &counts, Layout::Dense);
        threads
    }

    /// Runs `entry` over `threads` threads and reads `len` records of
    /// `out`: the bound buffer for a pass in place, else a zeroed one (zero
    /// records past the lattice).
    fn run<T: bytemuck::Pod>(
        &mut self,
        entry: &str,
        params: &StepParams,
        out: u32,
        len: usize,
        threads: usize,
    ) -> Vec<T> {
        if !self.bound.iter().any(|(binding, _)| *binding == out) {
            self.bind(out, &vec![T::zeroed(); len], output_layout(out));
        }
        self.table();
        let params = self.pooled(params);
        let buffers: Vec<(u32, &GpuBuffer)> = self
            .bound
            .iter()
            .map(|(binding, buffer)| (*binding, buffer))
            .collect();
        dispatch_pass(&self.device, entry, &params, &buffers, threads as u64);
        self.bound(out, len)
    }

    /// `len` records of the buffer bound at `binding`, converted to the dense
    /// array its layout stands for.
    fn bound<T: bytemuck::Pod>(&self, binding: u32, len: usize) -> Vec<T> {
        let buffer = &self
            .bound
            .iter()
            .find(|(b, _)| *b == binding)
            .expect("bound")
            .1;
        let pool = pool_len(self.pool_tiles()) as usize;
        match self.layout(binding) {
            Layout::Dense => read(buffer, len),
            Layout::Cells => {
                let dense = from_pool(&read::<T>(buffer, pool), self.cells);
                assert_eq!(dense.len(), len, "binding {binding}: cells read");
                dense
            }
            Layout::Records => {
                let dense = records_from_pool(&read::<FaceSample>(buffer, pool), self.cells);
                let dense: Vec<T> = bytemuck::pod_collect_to_vec(&dense);
                assert_eq!(dense.len(), len, "binding {binding}: records read");
                dense
            }
        }
    }
}

fn close(got: f32, want: f64, scale: f64, what: &str) {
    assert!(
        (f64::from(got) - want).abs() <= 1e-5 * scale.max(1.0),
        "{what}: {got} vs {want}"
    );
}

/// Random faces over the padded grid; faces that do not exist are zero.
/// `valid` draws weights of 0 or 1 (a projected grid) instead of positive
/// particle weights. The faces on the far box walls are closed (velocity 0,
/// weight 1), the canonical record there, as the pool holds no other.
fn random_faces(seed: u64, valid: bool) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut face = FaceSample::default();
            for a in 0..3 {
                if face_exists(p, a) {
                    face.velocity[a] = rng.signed(2.0);
                    face.weight[a] = if valid {
                        f32::from(u8::from(rng.unit() < 0.6))
                    } else {
                        rng.unit()
                    };
                    if p[a] == N[a] {
                        face.velocity[a] = 0.0;
                        face.weight[a] = 1.0;
                    }
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
                    face.weight[a] = if !solid || (0.25..0.5).contains(&r) {
                        1.0
                    } else if r < 0.25 {
                        0.0
                    } else {
                        0.05 + 0.95 * rng.unit()
                    };
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
    (0..cell_len())
        .map(|_| f32::from(u8::from(rng.unit() < 0.5)))
        .collect()
}

/// Particles scattered through the box, a few unused slots among them.
fn random_particles(seed: u64, count: usize) -> Vec<FluidParticle> {
    let mut rng = Stream::new(seed);
    (0..count)
        .map(|i| {
            let position: [f32; 3] = std::array::from_fn(|a| MIN[a] + rng.unit() * N[a] as f32 * H);
            let velocity = [rng.signed(1.5), rng.signed(1.5), rng.signed(1.5)];
            let radius = if i % 11 == 5 { 0.0 } else { 0.08 };
            FluidParticle {
                position_radius: [position[0], position[1], position[2], radius],
                velocity,
                id: i as u32 + 1,
            }
        })
        .collect()
}

/// Counting sort by cell, stable: the contract the step's sort keeps.
fn cpu_sort(particles: &[FluidParticle]) -> (Vec<FluidParticle>, Vec<CellRange>) {
    let cell_of = |p: &FluidParticle| {
        let c: [usize; 3] = std::array::from_fn(|a| {
            (((p.position_radius[a] - MIN[a]) / H).floor() as i64).clamp(0, N[a] as i64 - 1)
                as usize
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

// ── Solids ─────────────────────────────────────────────────────────────────
