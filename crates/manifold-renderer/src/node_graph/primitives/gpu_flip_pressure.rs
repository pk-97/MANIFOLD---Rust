//! GPU FLIP's pressure solver: the multigrid-preconditioned conjugate
//! gradient for L p = f on the water (docs/GPU_FLIP_PRESSURE_SOLVE.md section
//! 3 (the solve)), as a module the step node encodes directly. One solver
//! serves every solve on one water lattice: [`PressureSolver::prepare`] builds
//! the coarse levels once, then [`PressureSolver::solve`] runs per right-hand
//! side (the pressure solve and the density solve).
//!
//! The V-cycle depth follows the lattice: each level halves every side,
//! rounding up, until every side is [`COARSEST_SIDE`] or less, and that level
//! is solved exactly by its inverse. An odd side's extra coarse half-cell is
//! solid, so any Resolution runs and the V-cycle stays symmetric
//! (`scripts/mgpcg_reference.py`, the oracle, proves both).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use crate::node_graph::fluid_particles::FaceSample;

const SHADER: &str = include_str!("shaders/gpu_flip_pressure.wgsl");
const INVERSE_SHADER: &str = include_str!("shaders/coarse_inverse.wgsl");

/// The coarsest level's largest side: at most 4³ = 64 cells, the one
/// workgroup the coarse inverse runs in.
pub(crate) const COARSEST_SIDE: u32 = 4;
/// The longest lattice side the solver takes.
pub(crate) const MAX_SIDE: u32 = 1024;
/// Iterations one solve may run: the scalars buffer holds two per iteration.
pub(crate) const MAX_ITERATIONS: u32 = 64;
/// Red-black rounds before and after the coarse correction on every level
/// but the coarsest.
const SMOOTH_ROUNDS: usize = 2;
/// Partial sums per dot product, at most; one per 1024 cells below that.
const MAX_PARTIALS: u32 = 64;
const THREADS: u32 = 256;

/// The V-cycle's lattices, finest first: halve every side, rounding up, while
/// any side is over [`COARSEST_SIDE`].
pub(crate) fn level_lattices(lattice: [u32; 3]) -> Vec<[u32; 3]> {
    let mut levels = vec![lattice];
    let mut n = lattice;
    while n.iter().any(|&side| side > COARSEST_SIDE) {
        n = n.map(|side| side.div_ceil(2));
        levels.push(n);
    }
    levels
}

fn cells(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side)).product()
}

fn face_records(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side) + 1).product()
}

/// Device bytes the solver holds for itself at `lattice`: four lattice
/// vectors, each coarse level's water, faces, right-hand side and correction,
/// the coarse inverse, the partial sums and the scalars.
pub(crate) fn scratch_bytes(lattice: [u32; 3]) -> u64 {
    let levels = level_lattices(lattice);
    let coarse: u64 = levels[1..].iter().map(|&n| 3 * cells(n) * 4 + face_records(n) * FACE_BYTES).sum();
    let last = cells(*levels.last().expect("at least the fine level"));
    4 * cells(lattice) * 4 + coarse + last * last * 4 + u64::from(MAX_PARTIALS) * 4 + u64::from(2 * MAX_ITERATIONS) * 4
}

const FACE_BYTES: u64 = size_of::<FaceSample>() as u64;

/// Dispatches one [`PressureSolver::prepare`] and one
/// [`PressureSolver::solve`] of `iterations` encode at `lattice`.
pub(crate) fn passes(lattice: [u32; 3], iterations: u32) -> (usize, usize) {
    let coarse = level_lattices(lattice).len() - 1;
    let v_cycle = coarse * (4 * SMOOTH_ROUNDS + 3) + 1;
    (2 * coarse + 1, 1 + iterations as usize * (v_cycle + 7))
}

/// Why a lattice is refused, or None.
pub(crate) fn lattice_refusal(lattice: [u32; 3]) -> Option<String> {
    lattice
        .iter()
        .any(|&side| side == 0 || side > MAX_SIDE)
        .then(|| format!("every lattice side must be 1 to {MAX_SIDE}, not {lattice:?}"))
}

/// The water a solve runs on: the cell lattice, water per cell (> 0.5), the
/// padded face grid of open fractions (node.solid_faces' layout, box walls
/// 0), the cell size in metres, and the particles' signed distance per cell
/// for the free surface's ghost rows (docs/GPU_FLIP_PRESSURE_SOLVE.md
/// section 2 (the equation)). With no φ the rows are the plain ones, as the
/// density solve runs; the coarse levels always are.
pub(crate) struct Water<'a> {
    pub lattice: [u32; 3],
    pub cell_size: f32,
    pub water: &'a GpuBuffer,
    pub faces: &'a GpuBuffer,
    pub phi: Option<&'a GpuBuffer>,
}

impl<'a> Water<'a> {
    /// The φ binding and flag a finest-level pass takes. A pass with no φ
    /// binds the water array in its slot and never reads it.
    fn ghost(&self) -> (u32, &'a GpuBuffer) {
        self.phi.map_or((0, self.water), |phi| (1, phi))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    nx: u32,
    ny: u32,
    nz: u32,
    color: u32,
    cx: u32,
    cy: u32,
    cz: u32,
    mode: u32,
    cell_size: f32,
    slot: u32,
    ghost: u32,
    _pad1: u32,
}

impl Params {
    fn at(n: [u32; 3], cell_size: f32) -> Self {
        Self { nx: n[0], ny: n[1], nz: n[2], cell_size, ..Self::default() }
    }

    fn coarse(mut self, c: [u32; 3]) -> Self {
        [self.cx, self.cy, self.cz] = c;
        self
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InverseParams {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    _pad0: u32,
}

struct Pipelines {
    smooth: GpuComputePipeline,
    residual: GpuComputePipeline,
    restrict: GpuComputePipeline,
    prolong: GpuComputePipeline,
    coarsen_water: GpuComputePipeline,
    coarsen_faces: GpuComputePipeline,
    inverse: GpuComputePipeline,
    coarse_solve: GpuComputePipeline,
    init: GpuComputePipeline,
    dot_partial: GpuComputePipeline,
    dot_finalize: GpuComputePipeline,
    direction: GpuComputePipeline,
    update: GpuComputePipeline,
}

impl Pipelines {
    fn new(device: &GpuDevice) -> Self {
        let pipeline = |entry: &str, label: &str| device.create_compute_pipeline(SHADER, entry, label);
        Self {
            smooth: pipeline("smooth_main", "gpu_flip.pressure.smooth"),
            residual: pipeline("residual_main", "gpu_flip.pressure.residual"),
            restrict: pipeline("restrict_main", "gpu_flip.pressure.restrict"),
            prolong: pipeline("prolong_main", "gpu_flip.pressure.prolong"),
            coarsen_water: pipeline("coarsen_water_main", "gpu_flip.pressure.coarsen_water"),
            coarsen_faces: pipeline("coarsen_faces_main", "gpu_flip.pressure.coarsen_faces"),
            inverse: device.create_compute_pipeline(INVERSE_SHADER, "inverse_main", "gpu_flip.pressure.coarse_inverse"),
            coarse_solve: pipeline("coarse_solve_main", "gpu_flip.pressure.coarse_solve"),
            init: pipeline("init_main", "gpu_flip.pressure.init"),
            dot_partial: pipeline("dot_partial_main", "gpu_flip.pressure.dot_partial"),
            dot_finalize: pipeline("dot_finalize_main", "gpu_flip.pressure.dot_finalize"),
            direction: pipeline("direction_main", "gpu_flip.pressure.direction"),
            update: pipeline("update_main", "gpu_flip.pressure.update"),
        }
    }
}

/// A coarse level's own arrays.
struct Level {
    lattice: [u32; 3],
    cell_size: f32,
    water: GpuBuffer,
    faces: GpuBuffer,
    rhs: GpuBuffer,
    e: GpuBuffer,
}

/// Everything sized by the lattice, rebuilt when it changes.
struct Buffers {
    lattice: [u32; 3],
    coarse: Vec<Level>,
    r: GpuBuffer,
    z: GpuBuffer,
    p: GpuBuffer,
    /// A V-cycle level's residual, then the iteration's s = −L p.
    scratch: GpuBuffer,
    inverse: GpuBuffer,
    partials: GpuBuffer,
    scalars: GpuBuffer,
}

impl Buffers {
    fn new(device: &GpuDevice, lattice: [u32; 3]) -> Self {
        let lattices = level_lattices(lattice);
        let vector = |n: [u32; 3]| device.create_buffer(cells(n) * 4);
        let coarse = lattices[1..]
            .iter()
            .map(|&n| Level {
                lattice: n,
                cell_size: 0.0,
                water: vector(n),
                faces: device.create_buffer(face_records(n) * FACE_BYTES),
                rhs: vector(n),
                e: vector(n),
            })
            .collect();
        let last = cells(*lattices.last().expect("at least the fine level"));
        Self {
            lattice,
            coarse,
            r: vector(lattice),
            z: vector(lattice),
            p: vector(lattice),
            scratch: vector(lattice),
            inverse: device.create_buffer(last * last * 4),
            partials: device.create_buffer(u64::from(MAX_PARTIALS) * 4),
            scalars: device.create_buffer(u64::from(2 * MAX_ITERATIONS) * 4),
        }
    }
}

fn bytes(params: &Params) -> GpuBinding<'_> {
    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(params) }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer { binding, buffer, offset: 0 }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(u64::from(THREADS)) as u32, 1, 1]
}

/// One level's view for the V-cycle: the fine level reads the caller's water,
/// faces and φ and works in r and z; coarse levels run zero φ.
struct View<'a> {
    lattice: [u32; 3],
    cell_size: f32,
    water: &'a GpuBuffer,
    faces: &'a GpuBuffer,
    rhs: &'a GpuBuffer,
    e: &'a GpuBuffer,
    ghost: (u32, &'a GpuBuffer),
}

#[derive(Default)]
pub(crate) struct PressureSolver {
    pipelines: Option<Pipelines>,
    buffers: Option<Buffers>,
    /// The lattice and cell size the coarse levels were last built for.
    prepared: Option<([u32; 3], f32)>,
}

impl PressureSolver {
    /// Build the coarse levels and the coarse inverse for `water`. Every
    /// solve until the next prepare runs on this water. Allocates only when
    /// the lattice changes.
    pub(crate) fn prepare(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, water: &Water<'_>) -> Result<(), String> {
        if let Some(reason) = lattice_refusal(water.lattice) {
            return Err(reason);
        }
        let n = water.lattice;
        let phi = water.phi.map_or(u64::MAX, |phi| phi.size);
        if cells(n) * 4 > water.water.size.min(phi) || face_records(n) * FACE_BYTES > water.faces.size {
            return Err(format!("a {n:?} lattice is larger than its water, distance or face arrays"));
        }
        if !(water.cell_size.is_finite() && water.cell_size > 0.0) {
            return Err("the cell size must be positive".into());
        }
        let pipes = self.pipelines.get_or_insert_with(|| Pipelines::new(device));
        if self.buffers.as_ref().is_none_or(|b| b.lattice != n) {
            self.buffers = Some(Buffers::new(device, n));
        }
        let b = self.buffers.as_mut().expect("buffers sized");
        let mut h = water.cell_size;
        for level in &mut b.coarse {
            h *= 2.0;
            level.cell_size = h;
        }
        let mut fine = (n, water.water, water.faces);
        for level in &b.coarse {
            let params = Params::at(fine.0, 0.0).coarse(level.lattice);
            enc.dispatch_compute(
                &pipes.coarsen_water,
                &[bytes(&params), buffer(1, fine.1), buffer(5, &level.water)],
                groups(cells(level.lattice)),
                "gpu_flip.pressure.coarsen_water",
            );
            enc.dispatch_compute(
                &pipes.coarsen_faces,
                &[bytes(&params), buffer(2, fine.2), buffer(9, &level.faces)],
                groups(face_records(level.lattice)),
                "gpu_flip.pressure.coarsen_faces",
            );
            fine = (level.lattice, &level.water, &level.faces);
        }
        let (last, last_water, last_faces) = fine;
        let params = InverseParams { nodes_x: last[0], nodes_y: last[1], nodes_z: last[2], _pad0: 0 };
        enc.dispatch_compute(
            &pipes.inverse,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(1, last_water),
                buffer(2, last_faces),
                buffer(3, &b.inverse),
            ],
            [1, 1, 1],
            "gpu_flip.pressure.coarse_inverse",
        );
        self.prepared = Some((n, water.cell_size));
        Ok(())
    }

    /// Solve L p = `rhs` on the prepared water by `iterations` conjugate
    /// gradient iterations from zero, one V-cycle each; p into `pressure`.
    /// `water` must be what [`Self::prepare`] last saw.
    pub(crate) fn solve(
        &mut self,
        enc: &mut GpuEncoder,
        water: &Water<'_>,
        rhs: &GpuBuffer,
        pressure: &GpuBuffer,
        iterations: u32,
    ) -> Result<(), String> {
        let n = water.lattice;
        if self.prepared != Some((n, water.cell_size)) {
            return Err(format!("the solver was not prepared for a {n:?} lattice at this cell size"));
        }
        if water.phi.is_some_and(|phi| cells(n) * 4 > phi.size) {
            return Err(format!("a {n:?} lattice is larger than its distance array"));
        }
        if !(1..=MAX_ITERATIONS).contains(&iterations) {
            return Err(format!("iterations must be 1 to {MAX_ITERATIONS}, not {iterations}"));
        }
        if cells(n) * 4 > rhs.size.min(pressure.size) {
            return Err(format!("a {n:?} lattice is larger than the right-hand side or the pressure"));
        }
        let (Some(pipes), Some(b)) = (self.pipelines.as_ref(), self.buffers.as_ref()) else {
            return Err("the solver was not prepared".into());
        };
        let fine = Params::at(n, water.cell_size);
        let lattice = groups(cells(n));
        enc.dispatch_compute(
            &pipes.init,
            &[bytes(&fine), buffer(1, water.water), buffer(2, water.faces), buffer(3, rhs), buffer(5, &b.r)],
            lattice,
            "gpu_flip.pressure.init",
        );
        for k in 0..iterations {
            v_cycle(enc, pipes, b, water);
            dot(enc, pipes, b, n, &b.r, &b.z, 2 * k);
            let step = Params { slot: k, ..fine };
            enc.dispatch_compute(
                &pipes.direction,
                &[bytes(&step), buffer(3, &b.z), buffer(5, &b.p), buffer(7, &b.scalars)],
                lattice,
                "gpu_flip.pressure.direction",
            );
            let (ghost, phi) = water.ghost();
            let apply = Params { mode: 1, ghost, ..fine };
            enc.dispatch_compute(
                &pipes.residual,
                &[
                    bytes(&apply),
                    buffer(1, water.water),
                    buffer(2, water.faces),
                    buffer(3, &b.p),
                    buffer(4, &b.p),
                    buffer(5, &b.scratch),
                    buffer(10, phi),
                ],
                lattice,
                "gpu_flip.pressure.apply",
            );
            dot(enc, pipes, b, n, &b.p, &b.scratch, 2 * k + 1);
            enc.dispatch_compute(
                &pipes.update,
                &[
                    bytes(&step),
                    buffer(3, &b.p),
                    buffer(4, &b.scratch),
                    buffer(5, pressure),
                    buffer(6, &b.r),
                    buffer(7, &b.scalars),
                ],
                lattice,
                "gpu_flip.pressure.update",
            );
        }
        Ok(())
    }
}

/// z = V(r): one V-cycle for L e = r from zero.
fn v_cycle<'a>(enc: &mut GpuEncoder, pipes: &Pipelines, b: &'a Buffers, water: &Water<'a>) {
    let view = |level: usize| -> View<'a> {
        match level {
            0 => View {
                lattice: water.lattice,
                cell_size: water.cell_size,
                water: water.water,
                faces: water.faces,
                rhs: &b.r,
                e: &b.z,
                ghost: water.ghost(),
            },
            l => {
                let c = &b.coarse[l - 1];
                View {
                    lattice: c.lattice,
                    cell_size: c.cell_size,
                    water: &c.water,
                    faces: &c.faces,
                    rhs: &c.rhs,
                    e: &c.e,
                    ghost: (0, &c.water),
                }
            }
        }
    };
    let last = b.coarse.len();
    for level in 0..last {
        let v = view(level);
        let coarse = view(level + 1);
        for round in 0..SMOOTH_ROUNDS {
            for color in [0, 1] {
                smooth(enc, pipes, &v, color, round == 0 && color == 0);
            }
        }
        let params = Params::at(v.lattice, v.cell_size).coarse(coarse.lattice);
        enc.dispatch_compute(
            &pipes.residual,
            &[
                bytes(&Params { ghost: v.ghost.0, ..params }),
                buffer(1, v.water),
                buffer(2, v.faces),
                buffer(3, v.rhs),
                buffer(4, v.e),
                buffer(5, &b.scratch),
                buffer(10, v.ghost.1),
            ],
            groups(cells(v.lattice)),
            "gpu_flip.pressure.residual",
        );
        enc.dispatch_compute(
            &pipes.restrict,
            &[bytes(&params), buffer(3, &b.scratch), buffer(5, coarse.rhs), buffer(8, coarse.water)],
            groups(cells(coarse.lattice)),
            "gpu_flip.pressure.restrict",
        );
    }
    let v = view(last);
    let params = Params::at(v.lattice, v.cell_size);
    enc.dispatch_compute(
        &pipes.coarse_solve,
        &[bytes(&params), buffer(3, &b.inverse), buffer(4, v.rhs), buffer(5, v.e)],
        [1, 1, 1],
        "gpu_flip.pressure.coarse_solve",
    );
    for level in (0..last).rev() {
        let v = view(level);
        let coarse = view(level + 1);
        let params = Params::at(v.lattice, v.cell_size).coarse(coarse.lattice);
        enc.dispatch_compute(
            &pipes.prolong,
            &[bytes(&params), buffer(1, v.water), buffer(3, coarse.e), buffer(5, v.e)],
            groups(cells(v.lattice)),
            "gpu_flip.pressure.prolong",
        );
        for _ in 0..SMOOTH_ROUNDS {
            for color in [1, 0] {
                smooth(enc, pipes, &v, color, false);
            }
        }
    }
}

/// One red-black sweep of `color` at a level, from zero when `from_zero`.
fn smooth(enc: &mut GpuEncoder, pipes: &Pipelines, v: &View<'_>, color: u32, from_zero: bool) {
    let params = Params { color, mode: u32::from(from_zero), ghost: v.ghost.0, ..Params::at(v.lattice, v.cell_size) };
    enc.dispatch_compute(
        &pipes.smooth,
        &[bytes(&params), buffer(1, v.water), buffer(2, v.faces), buffer(3, v.rhs), buffer(5, v.e), buffer(10, v.ghost.1)],
        groups(cells(v.lattice)),
        "gpu_flip.pressure.smooth",
    );
}

/// a · b over the fine lattice into scalars[slot], in a fixed order.
fn dot(enc: &mut GpuEncoder, pipes: &Pipelines, b: &Buffers, n: [u32; 3], x: &GpuBuffer, y: &GpuBuffer, slot: u32) {
    let partials = (cells(n).div_ceil(1024) as u32).clamp(1, MAX_PARTIALS);
    let params = Params { color: partials, slot, ..Params::at(n, 0.0) };
    enc.dispatch_compute(
        &pipes.dot_partial,
        &[bytes(&params), buffer(3, x), buffer(4, y), buffer(6, &b.partials)],
        [partials, 1, 1],
        "gpu_flip.pressure.dot_partial",
    );
    enc.dispatch_compute(
        &pipes.dot_finalize,
        &[bytes(&params), buffer(6, &b.partials), buffer(7, &b.scalars)],
        [1, 1, 1],
        "gpu_flip.pressure.dot_finalize",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The liquid conformance suite checks codegen bodies for atomics; these
    /// hand shaders have none, so they are checked here.
    #[test]
    fn pressure_solver_uses_no_atomics() {
        assert!(!SHADER.contains("atomic"));
        assert!(!INVERSE_SHADER.contains("atomic"));
    }

    /// The level rule, odd sides included, as the reference's --symmetry run
    /// prints it; the coarsest level always fits the coarse inverse.
    #[test]
    fn levels_halve_rounding_up_to_four() {
        let sides = |n: u32| level_lattices([n; 3]).iter().map(|l| l[0]).collect::<Vec<_>>();
        assert_eq!(sides(24), [24, 12, 6, 3]);
        assert_eq!(sides(25), [25, 13, 7, 4]);
        assert_eq!(sides(37), [37, 19, 10, 5, 3]);
        assert_eq!(sides(40), [40, 20, 10, 5, 3]);
        assert_eq!(sides(64), [64, 32, 16, 8, 4]);
        assert_eq!(sides(100), [100, 50, 25, 13, 7, 4]);
        assert_eq!(sides(128), [128, 64, 32, 16, 8, 4]);
        assert_eq!(sides(3), [3]);
        assert_eq!(level_lattices([64, 9, 1]), [[64, 9, 1], [32, 5, 1], [16, 3, 1], [8, 2, 1], [4, 1, 1]]);
        for n in 1..=MAX_SIDE {
            let last = *level_lattices([n, n.div_ceil(3), 1]).last().unwrap();
            assert!(last.iter().all(|&side| side <= COARSEST_SIDE), "{n}");
            assert!(cells(last) <= crate::node_graph::primitives::coarse_inverse::MAX_COARSE_CELLS);
        }
    }

    #[test]
    fn refusals_name_the_lattice_and_count() {
        assert!(lattice_refusal([64; 3]).is_none());
        assert!(lattice_refusal([0, 64, 64]).unwrap().contains("1 to 1024"));
        assert!(lattice_refusal([2048, 64, 64]).is_some());
    }

    /// Every shader index stays inside the arrays the solver sizes, walked
    /// per pass on the CPU at odd and even lattices: a transfer's fine reads
    /// and a coarsening's children, both through the round-up rule.
    #[test]
    fn transfers_stay_inside_their_lattices() {
        for n in [1, 2, 3, 4, 5, 7, 9, 24, 25, 37, 40] {
            let levels = level_lattices([n, n + 1, n + 2]);
            for pair in levels.windows(2) {
                let (fine, coarse) = (pair[0], pair[1]);
                for a in 0..3 {
                    for c in 0..coarse[a] as i64 {
                        // Restriction gathers fine 2c − 1 .. 2c + 2 inside the
                        // fine lattice; prolongation's parent f / 2 of every
                        // fine cell is a coarse cell.
                        assert!(2 * c < i64::from(fine[a]), "{fine:?} → {coarse:?}");
                    }
                    for f in 0..fine[a] {
                        assert!(f / 2 < coarse[a]);
                    }
                    // A coarse face 1 .. c − 1 reads fine face 2c, inside the
                    // fine face grid's n + 1.
                    assert!(2 * (coarse[a] - 1) <= fine[a]);
                }
            }
        }
    }

    #[test]
    fn pass_counts_follow_the_levels() {
        assert_eq!(passes([64; 3], 8), (9, 1 + 8 * (4 * 11 + 1 + 7)));
        assert_eq!(passes([3; 3], 3), (1, 1 + 3 * 8));
        assert!(scratch_bytes([128; 3]) > 4 * 128 * 128 * 128 * 4);
    }
}
