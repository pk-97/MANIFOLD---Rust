//! A field read past the lattice it is laid on: radial and vortex leaves
//! from their formulas, as [`crate::interaction`] samples them, and every
//! uniform push and air at its constant value. Turbulence and sampled grids
//! drop out there: a gust's mean is still air, and a grid says nothing
//! outside itself.

use smallvec::SmallVec;

use super::{FieldValue, INLINE_OPERATIONS, Operation};
use crate::interaction::ForceSample;
use crate::{RadialField, VectorField, VortexField};

/// What one far term reads at a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FarShape {
    /// The same everywhere: the term's push, drag and drag × wind as they are.
    Constant,
    Radial(RadialField),
    Vortex(VortexField),
}

/// One term of a field past its lattice. A shaped term reads its leaf's
/// vector `f` at a point and adds `push · f` to the push and `drag_wind · f`
/// to the drag × wind; a constant term adds its vectors as they are. Either
/// adds `drag` to the drag rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarTerm {
    pub shape: FarShape,
    pub push: [f32; 3],
    pub drag: f32,
    pub drag_wind: [f32; 3],
}

impl FarTerm {
    fn shaped(shape: FarShape) -> Self {
        Self {
            shape,
            push: [1.0, 0.0, 0.0],
            drag: 0.0,
            drag_wind: [0.0; 3],
        }
    }

    fn constant(push: [f32; 3], drag: f32) -> Self {
        Self {
            shape: FarShape::Constant,
            push,
            drag,
            drag_wind: [0.0; 3],
        }
    }

    /// The term at world `position`. A shaped term's coefficients are the
    /// first component of `push` and `drag_wind`.
    pub fn sample_force(&self, position: [f32; 3]) -> ForceSample {
        let f = match self.shape {
            FarShape::Constant => {
                return ForceSample {
                    push: self.push,
                    drag: self.drag,
                    drag_wind: self.drag_wind,
                };
            }
            FarShape::Radial(field) => field.sample(position),
            FarShape::Vortex(field) => field.sample(position),
        };
        ForceSample {
            push: f.map(|c| c * self.push[0]),
            drag: self.drag,
            drag_wind: f.map(|c| c * self.drag_wind[0]),
        }
    }

    fn scale(&mut self, strength: f32) {
        self.push = self.push.map(|c| c * strength);
        self.drag *= strength.abs();
        self.drag_wind = self.drag_wind.map(|c| c * strength);
    }

    /// The push becomes wind felt at `rate`; the rate itself is the entry's,
    /// added once by its own constant term.
    fn become_air(&mut self, rate: f32) {
        for axis in 0..3 {
            self.drag_wind[axis] += rate * self.push[axis];
        }
        self.push = [0.0; 3];
    }

    fn air_drag(&mut self, weight: f32) {
        self.drag *= weight;
        self.drag_wind = self.drag_wind.map(|c| c * weight);
    }
}

impl FieldValue {
    /// This field's far terms into `out`, every constant summed into the
    /// first: what [`FarTerm::sample_force`] summed gives past the lattice.
    /// `false`, with `out` empty, for a field that multiplies two fields,
    /// which has no far form.
    pub fn far_terms(&self, out: &mut Vec<FarTerm>) -> bool {
        out.clear();
        // Where each stack entry's terms start: a postfix program keeps an
        // entry's terms together at the end.
        let mut starts = SmallVec::<[usize; INLINE_OPERATIONS]>::new();
        for operation in &self.operations {
            match operation {
                Operation::Uniform(field) => {
                    starts.push(out.len());
                    out.push(FarTerm::constant(field.vector(), 0.0));
                }
                Operation::Radial(field) => {
                    starts.push(out.len());
                    out.push(FarTerm::shaped(FarShape::Radial(*field)));
                }
                Operation::Vortex(field) => {
                    starts.push(out.len());
                    out.push(FarTerm::shaped(FarShape::Vortex(*field)));
                }
                Operation::Sampled(_) | Operation::Turbulence(_) | Operation::Zero { .. } => {
                    starts.push(out.len())
                }
                Operation::Add => {
                    starts.pop();
                }
                Operation::Multiply => {
                    out.clear();
                    return false;
                }
                Operation::Scale(strength) => out
                    [*starts.last().expect("a scale follows its entry")..]
                    .iter_mut()
                    .for_each(|term| term.scale(*strength)),
                Operation::Air(rate) => {
                    out[*starts.last().expect("air follows its entry")..]
                        .iter_mut()
                        .for_each(|term| term.become_air(*rate));
                    out.push(FarTerm::constant([0.0; 3], *rate));
                }
                Operation::AirDrag(weight) => out
                    [*starts.last().expect("air drag follows its entry")..]
                    .iter_mut()
                    .for_each(|term| term.air_drag(*weight)),
            }
        }
        // One constant term first, then the shaped ones in order.
        let mut constant = FarTerm::constant([0.0; 3], 0.0);
        let mut any = false;
        out.retain(|term| {
            if term.shape != FarShape::Constant {
                return true;
            }
            any = true;
            for axis in 0..3 {
                constant.push[axis] += term.push[axis];
                constant.drag_wind[axis] += term.drag_wind[axis];
            }
            constant.drag += term.drag;
            false
        });
        if any {
            out.insert(0, constant);
        }
        true
    }
}

/// The sum of `terms` at world `position`.
pub fn sample_far(terms: &[FarTerm], position: [f32; 3]) -> ForceSample {
    terms.iter().fold(ForceSample::default(), |sum, term| {
        sum + term.sample_force(position)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FalloffCurve;

    fn close(a: ForceSample, b: ForceSample) -> bool {
        let near = |x: f32, y: f32| (x - y).abs() <= 1e-5 * (1.0 + x.abs().max(y.abs()));
        (0..3).all(|axis| {
            near(a.push[axis], b.push[axis]) && near(a.drag_wind[axis], b.drag_wind[axis])
        }) && near(a.drag, b.drag)
    }

    /// A swarm's stock forces and the World's air: two radial forces of
    /// opposite sign and curve, a vortex, gravity, a wind whose gusts drop
    /// out far away, still air and the swarm's Air Drag. Past the lattice the
    /// terms read the same as the field with its gusts held still.
    #[test]
    fn far_terms_read_the_field_with_its_gusts_dropped() {
        let pull = FieldValue::radial_curve([0.0, 2.0, 0.0], 2.5, 1.0, FalloffCurve::Inverse)
            .unwrap()
            .scaled(-20.0)
            .unwrap();
        let core = FieldValue::radial([0.0, 2.0, 0.0], 1.5, 1.0)
            .unwrap()
            .scaled(20.0)
            .unwrap();
        let swirl = FieldValue::vortex_curve(
            [1.0, 0.0, -1.0],
            [0.3, 1.0, 0.1],
            4.0,
            2.0,
            FalloffCurve::Inverse,
        )
        .unwrap()
        .scaled(3.0)
        .unwrap();
        let gravity = FieldValue::uniform([0.0, -9.8, 0.0]).unwrap();
        let breeze = FieldValue::uniform([0.5, 0.0, 0.0]).unwrap();
        let gusts = FieldValue::turbulence(0.8, [0.0; 3], 7)
            .unwrap()
            .scaled(5.0)
            .unwrap();
        let wind = breeze.sum(&gusts).unwrap().air(3.0).unwrap();
        let calm = breeze.air(3.0).unwrap();
        let scene = |wind: &FieldValue| {
            pull.sum(&core)
                .unwrap()
                .sum(&swirl)
                .unwrap()
                .sum(wind)
                .unwrap()
        };
        let felt = |field: FieldValue| {
            FieldValue::felt_in_still_air(Some(&field), 0.4, 1.5)
                .unwrap()
                .unwrap()
                .sum(&gravity)
                .unwrap()
        };
        let (field, still) = (felt(scene(&wind)), felt(scene(&calm)));
        let mut terms = Vec::new();
        assert!(field.far_terms(&mut terms));
        // Gravity, the wind, still air and their drag fold into one constant.
        assert_eq!(terms.len(), 4);
        assert_eq!(terms[0].shape, FarShape::Constant);
        for position in [
            [9.0, 2.0, 0.5],
            [-6.0, -3.0, 4.0],
            [0.2, 2.1, 0.0],
            [30.0, 0.0, -30.0],
            [0.0, 2.0, 0.0],
        ] {
            let far = sample_far(&terms, position);
            assert!(
                close(far, still.sample_force(position)),
                "{position:?}: {far:?} vs {:?}",
                still.sample_force(position)
            );
        }
    }

    /// A field that multiplies two fields has no far form.
    #[test]
    fn a_product_has_no_far_terms() {
        let mask = FieldValue::uniform([1.0; 3]).unwrap();
        let field = FieldValue::radial([0.0; 3], 1.0, 1.0)
            .unwrap()
            .multiply(&mask)
            .unwrap();
        let mut terms = vec![FarTerm::constant([1.0; 3], 0.0)];
        assert!(!field.far_terms(&mut terms));
        assert!(terms.is_empty());
    }
}
