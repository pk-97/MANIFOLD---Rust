//! The coarse lattice a scene force field is laid onto for the GPU. Water,
//! matter and swarms sample the same field this way; the GPU read is the
//! trilinear include beside each solver's kernels, and [`ForceLattice::sample`]
//! is its CPU twin.

/// Solver cells per force-lattice cell along each axis
/// (GPU_MPM_SOLVER_DESIGN.md D13).
pub const FORCE_LATTICE_STRIDE: u32 = 4;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldValue, VectorField};

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

    #[test]
    fn force_lattice_refuses_a_non_finite_field() {
        let lattice = lattice();
        let mut values = vec![[0.0; 4]; lattice.node_count()];
        assert_eq!(lattice.fill(&mut values, |_| [f32::NAN, 0.0, 0.0]), Err(NotFinite));
    }
}
