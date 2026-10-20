//! The coarse lattice a scene force field is laid onto for the GPU. Water,
//! matter and swarms sample the same field this way; the GPU read is the
//! trilinear include beside each solver's kernels, and [`ForceLattice::sample`]
//! is its CPU twin.

use crate::clock::{ClockFrame, SimulationClock};
use crate::tick_samples::TickSamples;
use crate::{FieldValue, VectorField};

/// Solver cells per force-lattice cell along each axis
/// (GPU_MPM_SOLVER_DESIGN.md D13).
pub const FORCE_LATTICE_STRIDE: u32 = 4;

/// Force lattices a buffer keeps room for up front: more than a live frame's
/// ticks (`clock::MAX_LIVE_INTERVALS`). Offline catch-up grows it; not a time
/// or quality cap.
pub const FORCE_LATTICE_RESERVE: u32 = 3;

/// A field value is not finite at some lattice node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotFinite;

/// Nodes on a regular grid from `min`, four floats per node (xyz, pad), x
/// fastest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ForceLattice {
    min: [f32; 3],
    spacing: f32,
    nodes: [u32; 3],
}

impl ForceLattice {
    /// The lattice over a solver lattice of `solver_nodes` nodes at
    /// `cell_size`: it starts at the solver's min corner and covers its last
    /// node, so a solver node always sits inside it.
    pub fn covering(min: [f32; 3], cell_size: f32, solver_nodes: [u32; 3]) -> Self {
        Self {
            min,
            spacing: cell_size * FORCE_LATTICE_STRIDE as f32,
            nodes: solver_nodes.map(|n| n.saturating_sub(1).div_ceil(FORCE_LATTICE_STRIDE).max(1) + 1),
        }
    }

    /// The lattice over a box from `min` of `size` with `detail` nodes along
    /// its longest side, reaching at least the far corner on every axis. No
    /// axis takes more than `detail` nodes, so any box holds at most
    /// `detail³` whatever its size.
    pub fn over_box(min: [f32; 3], size: [f32; 3], detail: u32) -> Self {
        let detail = detail.max(2);
        let spacing = size.into_iter().fold(0.0_f32, f32::max) / (detail - 1) as f32;
        Self { min, spacing, nodes: size.map(|s| ((s / spacing).ceil().max(1.0) as u32 + 1).min(detail)) }
    }

    pub fn min(&self) -> [f32; 3] {
        self.min
    }

    pub fn nodes(&self) -> [u32; 3] {
        self.nodes
    }

    pub fn spacing(&self) -> f32 {
        self.spacing
    }

    pub fn node_count(&self) -> usize {
        self.nodes.iter().map(|&n| n as usize).product()
    }

    /// Bytes of one lattice: four floats per node.
    pub fn bytes(&self) -> u64 {
        self.node_count() as u64 * 16
    }

    fn position(&self, index: usize) -> [f32; 3] {
        let [nx, ny, _] = self.nodes.map(|n| n as usize);
        let coord = [index % nx, (index / nx) % ny, index / (nx * ny)];
        std::array::from_fn(|d| self.min[d] + coord[d] as f32 * self.spacing)
    }

    /// Sample `field` at every node into `values` (one lattice long).
    pub fn fill(&self, values: &mut [[f32; 4]], field: impl Fn([f32; 3]) -> [f32; 3]) -> Result<(), NotFinite> {
        for (index, value) in values.iter_mut().enumerate() {
            let v = field(self.position(index));
            if v.iter().any(|c| !c.is_finite()) {
                return Err(NotFinite);
            }
            *value = [v[0], v[1], v[2], 0.0];
        }
        Ok(())
    }

    /// The trilinear read of `values` at `x`, clamped to the lattice: the CPU
    /// twin of the GPU include's eight-corner sum.
    pub fn sample(&self, values: &[[f32; 4]], x: [f32; 3]) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        for k in 0..8u32 {
            let mut index = 0usize;
            let mut stride = 1usize;
            let mut weight = 1.0f32;
            for (d, ((&x, &min), &nodes)) in x.iter().zip(&self.min).zip(&self.nodes).enumerate() {
                let g = (x - min) / self.spacing;
                let base = g.floor().max(0.0).min((nodes - 2) as f32);
                let f = (g - base).clamp(0.0, 1.0);
                let o = (k >> d) & 1;
                weight *= if o == 1 { f } else { 1.0 - f };
                index += (base as usize + o as usize) * stride;
                stride *= nodes as usize;
            }
            for (axis, total) in sum.iter_mut().enumerate() {
                *total += values[index][axis] * weight;
            }
        }
        sum
    }
}

/// A scene force field laid onto its lattice at each fixed tick's start,
/// never per display frame, so a tick reads the same forces at any frame
/// rate: one lattice while the frame's ticks agree, one per tick otherwise.
/// Before each frame the host's physics history replay asks
/// [`Self::request_samples`] for the coming ticks' starts and hands each
/// result to [`Self::observe_sample`]; a tick nobody sampled is an error.
#[derive(Default)]
pub struct TickForces {
    lattice: Option<ForceLattice>,
    /// `lattices` lattices back to back.
    forces: Vec<[f32; 4]>,
    lattices: usize,
    /// The field every lattice holds while it holds still.
    source: Option<FieldValue>,
    /// The field at each tick's start, from the oldest tick not yet run.
    samples: TickSamples<FieldValue>,
    dirty: bool,
}

impl TickForces {
    /// Lay each of the frame's ticks' fields onto `lattice`. Call right after
    /// `clock` advanced to `frame`; `field` is this frame's, which belongs to
    /// any tick starting exactly now. `None` turns forces off.
    pub fn prepare(
        &mut self,
        lattice: ForceLattice,
        field: Option<&FieldValue>,
        clock: &SimulationClock,
        frame: &ClockFrame,
    ) -> Result<(), String> {
        let count = lattice.node_count();
        if self.lattice != Some(lattice) {
            self.forces.clear();
            self.lattices = 0;
            self.lattice = Some(lattice);
            self.source = None;
        }
        let first = frame.first_sequence;
        self.samples.settle(clock, frame, field);
        if field.is_none() {
            self.lattices = 0;
            self.source = None;
            return Ok(());
        }
        let ticks = frame.ticks as usize;
        let sampled = self.samples.span(first, ticks).map_err(|tick| {
            format!("Forces: tick {tick} was never sampled; the host must replay physics history before each frame")
        })?;
        let head = sampled.clone().next().map(|(_, at)| at);
        if let Some(head) = head.filter(|head| sampled.clone().all(|(_, at)| at == *head)) {
            if self.lattices != 1 || self.source.as_ref() != Some(head) {
                self.forces.resize(count, [0.0; 4]);
                lattice.fill(&mut self.forces, |x| head.sample(x)).map_err(|_| NOT_FINITE.to_string())?;
                self.lattices = 1;
                self.source = Some(head.clone());
                self.dirty = true;
            }
        } else if ticks > 0 {
            self.forces.resize(ticks * count, [0.0; 4]);
            for (values, at) in self.forces.chunks_exact_mut(count).zip(sampled.map(|(_, at)| at)) {
                lattice.fill(values, |x| at.sample(x)).map_err(|_| NOT_FINITE.to_string())?;
            }
            self.lattices = ticks;
            self.source = None;
            self.dirty = true;
        }
        // Keep only ticks not yet run.
        self.samples.prune_before(first + ticks as u64);
        Ok(())
    }

    /// Each not-yet-run tick's start under `clock` in `(from, until]`.
    pub fn request_samples(&mut self, clock: &SimulationClock, from: f64, until: f64, out: &mut Vec<f64>) {
        self.samples.request(clock, from, until, out);
    }

    /// A history replay sample of the field at transport `now`.
    pub fn observe_sample(&mut self, now: f64, field: Option<&FieldValue>) {
        self.samples.observe(now, field);
    }

    /// Lattices laid this frame: 0 without a field, 1 while it holds still
    /// (every tick reads it), else one per tick from the frame's first tick.
    pub fn lattices(&self) -> usize {
        self.lattices
    }

    /// Every force lattice, back to back.
    pub fn forces(&self) -> &[[f32; 4]] {
        &self.forces[..self.lattices * self.lattice.map_or(0, |lattice| lattice.node_count())]
    }

    /// Whether the lattices changed since the last [`Self::mark_uploaded`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_uploaded(&mut self) {
        self.dirty = false;
    }
}

const NOT_FINITE: &str = "Forces: the acceleration field is not finite on the force lattice";

#[cfg(test)]
mod tests {
    use super::*;

    fn lattice() -> ForceLattice {
        ForceLattice::covering([-1.0, -0.5, -2.0], 0.125, [19, 11, 23])
    }

    #[test]
    fn force_lattice_covers_the_solver_lattice() {
        let lattice = lattice();
        assert_eq!(lattice.nodes(), [6, 4, 7]);
        assert_eq!(lattice.spacing(), 0.5);
        assert_eq!(lattice.bytes(), 6 * 4 * 7 * 16);
        let solver_extent = [18.0 * 0.125, 10.0 * 0.125, 22.0 * 0.125];
        for (nodes, extent) in lattice.nodes().into_iter().zip(solver_extent) {
            assert!((nodes - 1) as f32 * lattice.spacing() >= extent);
        }
    }

    /// Nodes hold the field; the read returns a node at the node, the mean
    /// midway, and the last node past the end.
    #[test]
    fn force_lattice_fill_and_sample_match_the_field() {
        let field = FieldValue::radial([0.2, 0.1, -0.4], 1.5, 1.0)
            .unwrap()
            .sum(&FieldValue::vortex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap())
            .unwrap();
        let lattice = lattice();
        let mut values = vec![[0.0; 4]; lattice.node_count()];
        lattice.fill(&mut values, |x| field.sample(x)).unwrap();
        let [nx, ny, nz] = lattice.nodes().map(|n| n as usize);
        for index in 0..values.len() {
            let position = lattice.position(index);
            let expected = field.sample(position);
            assert_eq!(values[index], [expected[0], expected[1], expected[2], 0.0]);
            let read = lattice.sample(&values, position);
            for axis in 0..3 {
                assert!((read[axis] - expected[axis]).abs() <= 1e-5 * (1.0 + expected[axis].abs()));
            }
        }
        let (a, b) = (values[1 + nx], values[2 + nx]);
        let mid = lattice.sample(&values, [-1.0 + 0.75, 0.0, -2.0]);
        for axis in 0..3 {
            assert!((mid[axis] - 0.5 * (a[axis] + b[axis])).abs() < 1e-5);
        }
        let last = values[nx * ny * nz - 1];
        let beyond = lattice.sample(&values, [10.0, 10.0, 10.0]);
        for axis in 0..3 {
            assert!((beyond[axis] - last[axis]).abs() < 1e-5);
        }
    }

    /// Detail is nodes along the longest side; the others reach the far
    /// corner and never pass the detail, even where the division rounds up.
    #[test]
    fn force_lattice_over_a_box_keeps_its_detail_on_every_axis() {
        for (size, detail) in [([4.0, 4.0, 4.0], 17), ([3.0, 0.7, 1.9], 64), ([0.3, 0.3, 9.1], 2), ([1.0e3, 1.0, 1.0e-3], 64)] {
            let lattice = ForceLattice::over_box([-1.0; 3], size, detail);
            let longest = size.into_iter().fold(0.0_f32, f32::max);
            assert!((lattice.spacing() - longest / (detail - 1) as f32).abs() <= 1e-6 * longest, "{size:?}");
            for (nodes, extent) in lattice.nodes().into_iter().zip(size) {
                assert!(nodes <= detail && nodes >= 2, "{size:?}: {nodes} nodes");
                assert!((nodes - 1) as f32 * lattice.spacing() >= extent * (1.0 - 1e-6), "{size:?} reaches {extent}");
            }
            assert!(lattice.node_count() <= (detail as usize).pow(3));
        }
        assert_eq!(ForceLattice::over_box([0.0; 3], [4.0; 3], 17).nodes(), [17; 3]);
    }

    #[test]
    fn force_lattice_refuses_a_non_finite_field() {
        let lattice = lattice();
        let mut values = vec![[0.0; 4]; lattice.node_count()];
        assert_eq!(lattice.fill(&mut values, |_| [f32::NAN, 0.0, 0.0]), Err(NotFinite));
    }
}
