//! The far terms a swarm reads past its force lattice, one set per laid
//! lattice, packed for `node.move_swarm` (`move_swarm_body.wgsl`, `move_swarm_far`).
//!
//! Layout, in `[f32; 4]`: a header of [`FAR_HEADER`], then each lattice's
//! terms back to back, [`FAR_TERM`] each, every lattice padded with zero
//! terms to the same count.
//! - Header: (terms per lattice, 0, 0, 0); the frame's x, y and z axes in
//!   the world (xyz, 0); its origin (xyz, 1); the rows of its inverse
//!   (xyz, 0). The world frame without one.
//! - Term: (kind, curve, radius, falloff), then for a constant (kind 0)
//!   (push xyz, 0), (drag × wind xyz, drag), 0; for a radial (1) or vortex
//!   (2) (centre xyz, push), (axis xyz, drag), (drag × wind, 0, 0, 0).

use super::ForceLattice;
use crate::interaction::ForceSample;
use crate::{FalloffCurve, FarShape, FarTerm, FieldValue, RadialField, VortexField, sample_far};

/// `[f32; 4]` in the header.
pub const FAR_HEADER: usize = 8;
/// `[f32; 4]` per term.
pub const FAR_TERM: usize = 4;

/// Each laid lattice's far terms, packed.
#[derive(Default)]
pub struct FarForces {
    scratch: Vec<FarTerm>,
    terms: Vec<FarTerm>,
    counts: Vec<usize>,
    packed: Vec<[f32; 4]>,
    per_lattice: usize,
}

impl FarForces {
    pub fn clear(&mut self) {
        self.packed.clear();
        self.per_lattice = 0;
    }

    /// One set of terms per field, in lattice order. A field with no far
    /// form clears the whole set, so the swarm reads the lattice's edge
    /// there as before.
    pub fn lay<'a>(&mut self, lattice: &ForceLattice, fields: impl Iterator<Item = &'a FieldValue>) {
        self.terms.clear();
        self.counts.clear();
        for field in fields {
            if !field.far_terms(&mut self.scratch) {
                self.clear();
                return;
            }
            self.counts.push(self.scratch.len());
            self.terms.extend_from_slice(&self.scratch);
        }
        // At least one term, so a field whose far value is nothing still
        // reads nothing past the lattice rather than the edge.
        self.per_lattice = self.counts.iter().copied().max().unwrap_or(0).max(1);
        self.packed.clear();
        self.packed.resize(FAR_HEADER + self.counts.len() * self.per_lattice * FAR_TERM, [0.0; 4]);
        self.packed[0] = [self.per_lattice as f32, 0.0, 0.0, 0.0];
        let (axes, origin, inverse) = lattice.frame.map_or(
            ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], [0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            |frame| (frame.axes, frame.origin, frame.inverse),
        );
        for i in 0..3 {
            self.packed[1 + i] = [axes[i][0], axes[i][1], axes[i][2], 0.0];
            self.packed[5 + i] = [inverse[i][0], inverse[i][1], inverse[i][2], 0.0];
        }
        self.packed[4] = [origin[0], origin[1], origin[2], 1.0];
        let mut at = FAR_HEADER;
        let mut terms = self.terms.iter();
        for &count in &self.counts {
            for (slot, term) in self.packed[at..at + count * FAR_TERM].chunks_exact_mut(FAR_TERM).zip(terms.by_ref().take(count)) {
                slot.copy_from_slice(&pack(term));
            }
            at += self.per_lattice * FAR_TERM;
        }
    }

    /// Terms per lattice; 0 when there is no far form.
    pub fn terms_per_lattice(&self) -> usize {
        self.per_lattice
    }

    /// The header and every lattice's terms; empty when there is no far form.
    pub fn packed(&self) -> &[[f32; 4]] {
        if self.per_lattice == 0 { &[] } else { &self.packed }
    }

    /// Lattice `slot`'s far force at `local`, a point in the lattice's frame,
    /// read from the packed words: the CPU twin of `move_swarm_far`.
    pub fn sample(&self, slot: usize, local: [f32; 3]) -> ForceSample {
        let words = self.packed();
        if words.is_empty() {
            return ForceSample::default();
        }
        let world: [f32; 3] = std::array::from_fn(|d| words[4][d] + (0..3).map(|i| words[1 + i][d] * local[i]).sum::<f32>());
        let start = FAR_HEADER + slot * self.per_lattice * FAR_TERM;
        let terms: Vec<FarTerm> = words[start..start + self.per_lattice * FAR_TERM].chunks_exact(FAR_TERM).map(unpack).collect();
        let mut s = sample_far(&terms, world);
        let to_local = |v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| (0..3).map(|d| words[5 + i][d] * v[d]).sum::<f32>()) };
        s.push = to_local(s.push);
        s.drag_wind = to_local(s.drag_wind);
        s
    }
}

fn pack(term: &FarTerm) -> [[f32; 4]; FAR_TERM] {
    let [p, w] = [term.push, term.drag_wind];
    let shaped = |kind: f32, curve: FalloffCurve, radius: f32, falloff: f32, center: [f32; 3], axis: [f32; 3]| {
        [
            [kind, curve.index() as f32, radius, falloff],
            [center[0], center[1], center[2], p[0]],
            [axis[0], axis[1], axis[2], term.drag],
            [w[0], 0.0, 0.0, 0.0],
        ]
    };
    match term.shape {
        FarShape::Constant => [[0.0; 4], [p[0], p[1], p[2], 0.0], [w[0], w[1], w[2], term.drag], [0.0; 4]],
        FarShape::Radial(f) => shaped(1.0, f.curve(), f.radius(), f.falloff(), f.center(), [0.0; 3]),
        FarShape::Vortex(f) => shaped(2.0, f.curve(), f.radius(), f.falloff(), f.center(), f.axis()),
    }
}

fn unpack(words: &[[f32; 4]]) -> FarTerm {
    let [head, a, b, c] = [words[0], words[1], words[2], words[3]];
    let curve = FalloffCurve::from_index(head[1]);
    let center = [a[0], a[1], a[2]];
    let shape = match head[0].round() as i32 {
        1 => RadialField::new(center, head[2], head[3]).ok().map(|f| FarShape::Radial(f.with_curve(curve))),
        2 => VortexField::new(center, [b[0], b[1], b[2]], head[2], head[3]).ok().map(|f| FarShape::Vortex(f.with_curve(curve))),
        _ => None,
    };
    match shape {
        Some(shape) => FarTerm { shape, push: [a[3], 0.0, 0.0], drag: b[3], drag_wind: [c[0], 0.0, 0.0] },
        None => FarTerm { shape: FarShape::Constant, push: [a[0], a[1], a[2]], drag: b[3], drag_wind: [b[0], b[1], b[2]] },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VectorField;
    use crate::force_lattice::LatticeFrame;

    /// A swarm turned, scaled and moved: past its lattice the packed terms
    /// read the world field at the point's world position, turned back into
    /// the swarm's frame, as the lattice stores its nodes. Two lattices keep
    /// their own terms, the shorter one padded.
    #[test]
    fn packed_far_terms_read_the_field_in_the_swarm_frame() {
        let (c, s) = (0.6f32, 0.8f32);
        let frame = LatticeFrame::new([[2.0 * c, 0.0, -2.0 * s], [0.0, 0.5, 0.0], [s, 0.0, c]], [3.0, 1.0, -2.0]).unwrap();
        let lattice = ForceLattice::over_box([-3.0; 3], [6.0; 3], 9).in_frame(frame);
        let pull = FieldValue::radial_curve([1.0, 2.0, 0.0], 2.5, 1.0, FalloffCurve::Inverse).unwrap().scaled(-20.0).unwrap();
        let swirl = FieldValue::vortex([0.0; 3], [0.0, 1.0, 0.0], 3.0, 1.0).unwrap().scaled(4.0).unwrap();
        let wind = FieldValue::uniform([1.0, 0.0, 0.5]).unwrap().air(2.0).unwrap();
        let first = pull.sum(&swirl).unwrap().sum(&wind).unwrap();
        let second = wind.clone();
        let mut far = FarForces::default();
        far.lay(&lattice, [&first, &second].into_iter());
        assert_eq!(far.terms_per_lattice(), 3);
        assert_eq!(far.packed().len(), FAR_HEADER + 2 * 3 * FAR_TERM);
        let near = |a: f32, b: f32| (a - b).abs() <= 1e-4 * (1.0 + a.abs().max(b.abs()));
        for (slot, field) in [&first, &second].into_iter().enumerate() {
            for local in [[5.0, 0.0, 0.0], [-4.0, 6.0, 1.0], [0.5, -7.0, 9.0]] {
                let world = frame.point(local);
                let mut expected = field.sample_force(world);
                expected.push = frame.vector_to_local(expected.push);
                expected.drag_wind = frame.vector_to_local(expected.drag_wind);
                let read = far.sample(slot, local);
                assert!(near(read.drag, expected.drag), "{slot} {local:?}");
                for axis in 0..3 {
                    assert!(near(read.push[axis], expected.push[axis]), "{slot} {local:?}: {read:?} vs {expected:?}");
                    assert!(near(read.drag_wind[axis], expected.drag_wind[axis]), "{slot} {local:?}");
                }
            }
        }
    }
}
