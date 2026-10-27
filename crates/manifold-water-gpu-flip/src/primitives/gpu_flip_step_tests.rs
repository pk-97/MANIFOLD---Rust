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

use super::gpu_flip_step::{StepParams, dispatch_pass, pool_tiles};
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
    BodySupports, LiquidBody, LiquidShape, SUPPORT_VEC4S, body_pose_at, pack_distance_atlas,
    pack_supports, unpack_supports,
};
use manifold_water_liquid::bounds::bounds_distance;
use manifold_water_liquid::coupling::coupled_start;
use manifold_water_liquid::fluid_particles::{CellRange, FaceSample};
use manifold_water_liquid::primitives::liquid_stats::INFLOW_BIRTHS_TAIL;
use manifold_water_liquid::testkit::world_box::{BoxTable, from_pool_under};
use manifold_water_liquid::tile_storage::pool_len;
mod cells;
mod density;
mod emit;
mod face_gravity;
mod free_fall;
mod motion;
mod narrow_band;
mod pockets;
mod slots;
mod solids;
mod step_order;

use emit::*;
use manifold_node_engine::parameters::ParamValue;
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

/// The step's pooled `faces` output as the dense `(n+1)^3` record array of
/// the solver grid whose first world cell is `origin`, read through its
/// `tiles` output; a record of a tile the table does not hold reads as the
/// canonical element.
pub(super) fn step_faces(
    h: &Harness,
    faces: manifold_node_engine::bindings::Slot,
    tiles: manifold_node_engine::bindings::Slot,
    origin: [i32; 3],
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
    let pool_tiles = (pool.len() as u32 - 1) / 512;
    from_pool_under(&pool, &table, pool_tiles, origin, cells.map(|n| n + 1))
}

/// Index i of a box of `dims`, x fastest.
fn coords_in(i: usize, dims: [usize; 3]) -> [usize; 3] {
    [
        i % dims[0],
        (i / dims[0]) % dims[1],
        i / (dims[0] * dims[1]),
    ]
}

/// A lattice with unequal sides, so a swapped axis shows.
const N: [usize; 3] = [6, 5, 4];
const H: f32 = 0.25;
/// The box's low corner: world cell (-3, -2, -2) is its first, so the box
/// sits across the origin and across tiles -1 and 0 on every axis.
const MIN: [f32; 3] = [-0.875, -0.625, -0.625];

/// The authored box the node-level proofs give the step: N cells from
/// NODE_MIN, a whole number of cells from the world origin (world cells
/// (-3, -2, -2) on), so the solver grid 1.5 cells outside it starts on a
/// world cell's low face, as the domain places it.
const NODE_MIN: [f32; 3] = [-0.75, -0.5, -0.5];

/// The step's wires for the box of N cells from NODE_MIN, as the domain
/// publishes them: the cell size, the box's pool, and its Bounds with every
/// face a Wall.
fn node_lattice() -> [(&'static str, f32); 9] {
    let hi: [f32; 3] = std::array::from_fn(|a| NODE_MIN[a] + N[a] as f32 * H);
    [
        ("cell_size", H),
        ("pool_tiles", pool_tiles(N.map(|n| n as u32), 63) as f32),
        ("bounds_lo_x", NODE_MIN[0]),
        ("bounds_lo_y", NODE_MIN[1]),
        ("bounds_lo_z", NODE_MIN[2]),
        ("bounds_hi_x", hi[0]),
        ("bounds_hi_y", hi[1]),
        ("bounds_hi_z", hi[2]),
        ("closed_faces", 63.0),
    ]
}

/// The world cell of box cell 0 for a box from `min` at cell size `h`.
fn box_origin(min: [f32; 3], h: f32) -> [i32; 3] {
    manifold_water_liquid::tile_pool::solver_origin_cell(min, h)
}

/// The native solid push target in cells.
const WALL_MARGIN_CELLS: f64 = 0.2;
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

/// Face a of padded cell p is one of the box's own faces when its other two
/// indices are inside the cells; the solid fixtures cut only those, and only
/// the inner ones (not on the box's edge), as walls are closed faces.
fn face_exists(p: [usize; 3], a: usize) -> bool {
    (0..3).all(|b| b == a || p[b] < N[b])
}

/// The Bounds box's high corner: N cells from MIN.
fn box_hi() -> [f32; 3] {
    std::array::from_fn(|a| MIN[a] + N[a] as f32 * H)
}

/// The step's params over this file's lattice, no impulse tick, the Bounds
/// box N cells from MIN with every face a Wall.
fn lattice() -> StepParams {
    StepParams {
        impulse_tick: -1,
        ..StepParams::default()
    }
    .with_grid_for_test(MIN, H)
    .with_bounds_for_test(MIN, box_hi(), 63, 0)
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

/// Independent f64 port of FluidSimulation::_resolveCollision for the box
/// wall SDF: a wall is a solid like a body, so there is no boundary clamp.
fn native_wall_move(q0: [f64; 3], q1: [f64; 3], n: [usize; 3], h: f64) -> [f64; 3] {
    native_collision_move(
        q0,
        q1,
        h,
        |q| wall_phi(q, n),
        |q| wall_gradient(q, n).map(|v| v * h),
    )
}

// The march and push of the native sequence, without the GPU's conservative
// distance shortcut. phi is in cells; gradient holds the native unscaled
// lattice edge differences.
fn native_collision_move(
    q0: [f64; 3],
    q1: [f64; 3],
    h: f64,
    phi: impl Fn([f64; 3]) -> f64,
    gradient: impl Fn([f64; 3]) -> [f64; 3],
) -> [f64; 3] {
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
        if d < 0.0 {
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
            return kept;
        }
        last = current;
    }
    q1
}

/// How a bound array is stored: a dense array as the CPU references hold it,
/// or a pooled field over the pass's world tiles, which the harness converts
/// from and to the dense array. Cells are `n³`, records `(n + 1)³` (the solid
/// corners among them), both from the box's first world cell, an element one
/// value or a record's floats; every other element of the pool, the
/// canonical one among them, is zero.
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
        3 | 4 | 9 | 10 | 11 | 17 | 18 | 20 | 43 => Layout::Records,
        5..=8 | 23 | 25 | 33..=35 | 41 => Layout::Cells,
        _ => Layout::Dense,
    }
}

/// One entry of the step's shader with its buffers bound by number. Pooled
/// arrays live in the world tiles of the box `cells` from MIN's world cell
/// (`BoxTable` over the record range, slots in tile order); the binding at
/// 51 is the table.
struct Pass {
    device: manifold_gpu::testkit::TestDevice,
    cells: [u32; 3],
    table: BoxTable,
    bound: Vec<(u32, GpuBuffer)>,
    layouts: Vec<(u32, Layout)>,
}

impl Pass {
    fn new() -> Self {
        Self::on(N)
    }

    /// A pass over a lattice other than [`N`], from MIN.
    fn on(cells: [usize; 3]) -> Self {
        Self::at(cells, MIN)
    }

    /// A pass over a box of `cells` from `min` (metres, half a cell below a
    /// world cell's centre).
    fn at(cells: [usize; 3], min: [f32; 3]) -> Self {
        let records = cells.map(|n| n as u32 + 1);
        Self {
            device: manifold_gpu::testkit::test_device(),
            cells: cells.map(|n| n as u32),
            table: BoxTable::new(box_origin(min, H), records),
            bound: Vec::new(),
            layouts: Vec::new(),
        }
    }

    /// Tiles the pool holds.
    fn pool_tiles(&self) -> u32 {
        self.table.pool_tiles
    }

    /// `params` over this pass's pool.
    fn pooled(&self, params: &StepParams) -> StepParams {
        params.with_pool_for_test(self.pool_tiles())
    }

    /// Binds the table at 51 once.
    fn table(&mut self) {
        if !self.bound.iter().any(|(binding, _)| *binding == 51) {
            let words = self.table.words.clone();
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

    /// The box of a layout's dense array.
    fn dims(&self, layout: Layout) -> [usize; 3] {
        let n = self.cells.map(|v| v as usize);
        match layout {
            Layout::Records => n.map(|v| v + 1),
            _ => n,
        }
    }

    /// Values a box element of `dims` holds in an array of `len` values:
    /// one, or a record's floats.
    fn per_element(len: usize, dims: [usize; 3]) -> usize {
        let elements = dims.iter().product::<usize>();
        assert!(
            elements > 0 && len.is_multiple_of(elements),
            "a dense box of {dims:?}: {len} values"
        );
        len / elements
    }

    /// `dense` (a box of `dims` from the box's first cell, each element one
    /// or more values) as a pooled field, every other value `canon`.
    fn to_pool<T: Copy>(&self, dense: &[T], dims: [usize; 3], canon: T) -> Vec<T> {
        let per = Self::per_element(dense.len(), dims);
        let mut pool = vec![canon; per * pool_len(self.pool_tiles()) as usize];
        for (i, element) in dense.chunks(per).enumerate() {
            let at = per * self.table.pool_index(coords_in(i, dims).map(|c| c as u32));
            pool[at..at + per].copy_from_slice(element);
        }
        pool
    }

    /// Binds a buffer holding `values` at `binding`, in `layout`: a pooled
    /// layout converts the dense array to the pool.
    fn bind<T: bytemuck::Pod>(&mut self, binding: u32, values: &[T], layout: Layout) -> &mut Self {
        let pooled: Vec<u8> = match layout {
            Layout::Dense => Vec::new(),
            _ => {
                let pool = self.to_pool(values, self.dims(layout), T::zeroed());
                bytemuck::cast_slice::<T, u8>(&pool).to_vec()
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
    /// the cell's pool index, and the canonical element, the sort's last key,
    /// holding no particle and ending at the live total.
    fn bind_ranges(&mut self, ranges: &[CellRange]) -> &mut Self {
        let live = ranges.iter().map(|range| range.count).sum();
        let mut pooled = self.to_pool(ranges, self.dims(Layout::Cells), CellRange::default());
        let canon = pooled.len() - 1;
        pooled[canon] = CellRange {
            start: live,
            count: 0,
        };
        self.bind(1, &pooled, Layout::Dense)
    }

    /// Binds the slot list at 29 as every slot in order, so a pass over a
    /// list covers the whole box; returns its thread count (512 a slot).
    fn every_tile(&mut self) -> usize {
        let slots: Vec<u32> = (0..self.pool_tiles()).collect();
        self.bind(29, &slots, Layout::Dense);
        slots.len() * 512
    }

    /// Binds every slot as the held list `extend_faces` walks: the list at
    /// 29 and its length in `tile_counts` (30) at `params.ring_max`; returns
    /// its thread count (512 a slot).
    fn allocated_tiles(&mut self, params: &StepParams) -> usize {
        let threads = self.every_tile();
        let mut counts = vec![0u32; params.ring_max as usize + 8];
        counts[params.ring_max as usize] = self.pool_tiles();
        self.bind(30, &counts, Layout::Dense);
        threads
    }

    /// Runs a list pass `entry` over every slot (binding the list at 29 when
    /// it is not bound) and reads `len` records of `out`.
    fn run_list<T: bytemuck::Pod>(
        &mut self,
        entry: &str,
        params: &StepParams,
        out: u32,
        len: usize,
    ) -> Vec<T> {
        let threads = if self.bound.iter().any(|(binding, _)| *binding == 29) {
            self.pool_tiles() as usize * 512
        } else {
            self.every_tile()
        };
        self.run(entry, params, out, len, threads)
    }

    /// Runs `entry` over `threads` threads and reads `len` records of
    /// `out`: the bound buffer for a pass in place, else a zeroed one.
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

    /// A pooled field holding `value(c)` at every cell of every held tile, c
    /// the world cell, and `canon` at the canonical element: unlike a dense
    /// box it holds values beyond the box too.
    fn pooled_everywhere<T: bytemuck::Pod>(
        &self,
        canon: T,
        value: impl Fn([i32; 3]) -> T,
    ) -> Vec<T> {
        let mut pool = vec![canon; pool_len(self.pool_tiles()) as usize];
        for (tile, slot) in self.table.held() {
            for local in 0..512i32 {
                let c = [
                    tile[0] * 8 + (local & 7),
                    tile[1] * 8 + ((local >> 3) & 7),
                    tile[2] * 8 + (local >> 6),
                ];
                pool[slot as usize * 512 + local as usize] = value(c);
            }
        }
        pool
    }

    /// The solid corners of the Bounds box `lo` to `hi` with the Wall faces
    /// `walls`, at every corner of every held tile: the contract's distance,
    /// positive inside and negative beyond a wall, and the canonical corner
    /// clear by three cells.
    fn wall_corners(&self, lo: [f32; 3], hi: [f32; 3], walls: u32) -> Vec<f32> {
        self.pooled_everywhere(3.0 * H, |c| {
            bounds_distance(
                std::array::from_fn(|a| c[a] as f32 * H - 0.5 * H),
                lo,
                hi,
                walls,
            )
        })
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
        let layout = self.layout(binding);
        if layout == Layout::Dense {
            return read(buffer, len);
        }
        let dims = self.dims(layout);
        let per = Self::per_element(len, dims);
        let pool = read::<T>(buffer, per * pool_len(self.pool_tiles()) as usize);
        let mut dense = Vec::with_capacity(len);
        for i in 0..len / per {
            let at = per * self.table.pool_index(coords_in(i, dims).map(|c| c as u32));
            dense.extend_from_slice(&pool[at..at + per]);
        }
        dense
    }
}

fn close(got: f32, want: f64, scale: f64, what: &str) {
    assert!(
        (f64::from(got) - want).abs() <= 1e-5 * scale.max(1.0),
        "{what}: {got} vs {want}"
    );
}

/// Random faces over the padded grid, all three of every record. `valid`
/// draws weights of 0 or 1 (a projected grid) instead of positive particle
/// weights.
fn random_faces(seed: u64, valid: bool) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    (0..face_len())
        .map(|_| {
            let mut face = FaceSample::default();
            for a in 0..3 {
                face.velocity[a] = rng.signed(2.0);
                face.weight[a] = if valid {
                    f32::from(u8::from(rng.unit() < 0.6))
                } else {
                    rng.unit()
                };
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

/// Counting sort by box cell, stable: the contract the step's sort keeps for
/// particles inside the box.
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
