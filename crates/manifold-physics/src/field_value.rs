//! Owned vector expressions for graph wires and retained tick inputs.
//!
//! The native adapters still consume `VectorField`; this value supplies owned
//! snapshots without borrowing a renderer graph.
use std::sync::Arc;

use smallvec::SmallVec;

use crate::interaction::ForceSample;
use crate::{
    FalloffCurve, PhysicsError, RadialField, SampledField, TurbulenceField, UniformField, VectorField,
    VortexField,
};

mod serialization;

/// Operations held inline: a field this size or smaller composes, clones and
/// samples without allocating. A bigger one (a scene with many forces) spills
/// to the heap and works the same; there is no operation limit.
const INLINE_OPERATIONS: usize = 32;

type Program = SmallVec<[Operation; INLINE_OPERATIONS]>;

#[derive(Clone, Debug, PartialEq)]
enum Operation {
    Uniform(UniformField),
    Radial(RadialField),
    Vortex(VortexField),
    Sampled(Arc<SampledField>),
    Turbulence(TurbulenceField),
    Add,
    Multiply,
    Scale(f32),
    /// The entry's push becomes wind velocity felt at this drag rate (1/s).
    Air(f32),
    /// A consumer's Air Drag: scales the entry's drag rate, the wind kept.
    AirDrag(f32),
    /// Nothing, where a field was scaled to zero or its air felt at zero.
    /// `air` remembers the field carried air, so a rule about air (multiply
    /// refuses it) holds whether a force is switched on or not.
    Zero { air: bool },
}

/// A dimensionless spatial vector expression. Strength belongs to the
/// consuming `FieldInput` or an explicit scale operation. Leaf values own
/// their parameters; sampled grids share immutable prepared storage.
///
/// Numeric edits, composition, cloning and sampling allocate nothing up to
/// [`INLINE_OPERATIONS`]. Native worlds never retain borrowed graph nodes
/// through this value.
///
/// Besides its push a field can carry air ([`Self::air`]): a wind velocity and
/// a drag rate, read through [`VectorField::sample_force`].
#[derive(Clone, Debug, PartialEq)]
pub struct FieldValue {
    /// A postfix program: leaves push, Add and Multiply pop two and push one,
    /// the rest change the top entry.
    operations: Program,
    /// The most entries the program holds at once: the sampling stack.
    depth: usize,
}

impl FieldValue {
    fn leaf(operation: Operation) -> Self {
        let mut operations = Program::new();
        operations.push(operation);
        Self { operations, depth: 1 }
    }

    pub fn uniform(vector: [f32; 3]) -> Result<Self, PhysicsError> {
        UniformField::new(vector).map(|field| Self::leaf(Operation::Uniform(field)))
    }

    pub fn radial(center: [f32; 3], radius: f32, falloff: f32) -> Result<Self, PhysicsError> {
        Self::radial_curve(center, radius, falloff, FalloffCurve::Edge)
    }

    pub fn radial_curve(
        center: [f32; 3],
        radius: f32,
        falloff: f32,
        curve: FalloffCurve,
    ) -> Result<Self, PhysicsError> {
        RadialField::new(center, radius, falloff)
            .map(|field| Self::leaf(Operation::Radial(field.with_curve(curve))))
    }

    pub fn vortex(
        center: [f32; 3],
        axis: [f32; 3],
        radius: f32,
        falloff: f32,
    ) -> Result<Self, PhysicsError> {
        Self::vortex_curve(center, axis, radius, falloff, FalloffCurve::Edge)
    }

    pub fn vortex_curve(
        center: [f32; 3],
        axis: [f32; 3],
        radius: f32,
        falloff: f32,
        curve: FalloffCurve,
    ) -> Result<Self, PhysicsError> {
        VortexField::new(center, axis, radius, falloff)
            .map(|field| Self::leaf(Operation::Vortex(field.with_curve(curve))))
    }

    pub fn sampled(grid: Arc<SampledField>) -> Self {
        Self::leaf(Operation::Sampled(grid))
    }

    pub fn turbulence(size: f32, offset: [f32; 3], seed: u32) -> Result<Self, PhysicsError> {
        TurbulenceField::new(size, offset, seed).map(|field| Self::leaf(Operation::Turbulence(field)))
    }

    pub fn sum(&self, other: &Self) -> Result<Self, PhysicsError> {
        self.combine(other, Operation::Add)
    }

    /// Component-wise product. A grid with equal XYZ components acts as a
    /// scalar spatial mask; arbitrary vector inputs remain composable. A mask
    /// shapes a push: a field already carrying air is refused, so mask the
    /// wind's velocity before it becomes air.
    pub fn multiply(&self, other: &Self) -> Result<Self, PhysicsError> {
        if self.carries_air() || other.carries_air() {
            return Err(PhysicsError::InvalidInput(
                "a field carrying air cannot be multiplied; multiply the wind before it becomes air",
            ));
        }
        self.combine(other, Operation::Multiply)
    }

    fn combine(&self, other: &Self, operation: Operation) -> Result<Self, PhysicsError> {
        let mut result = self.clone();
        result.operations.extend(other.operations.iter().cloned());
        result.operations.push(operation);
        result.depth = self.depth.max(1 + other.depth);
        Ok(result)
    }

    fn push_unary(&self, operation: Operation) -> Result<Self, PhysicsError> {
        let mut result = self.clone();
        result.operations.push(operation);
        Ok(result)
    }

    /// Scale the push; the drag rate scales by the strength's size and the
    /// wind keeps its speed (see [`ForceSample::scaled`]). A zero strength
    /// drops the field for a zero leaf: it samples the same, and a field that
    /// moves over time (drifting gusts) held at zero, or a disabled force, no
    /// longer changes from tick to tick, so its lattice is laid once.
    pub fn scaled(&self, strength: f32) -> Result<Self, PhysicsError> {
        if !strength.is_finite() {
            return Err(PhysicsError::InvalidInput(
                "vector field scale must be finite",
            ));
        }
        if strength == 0.0 {
            return Ok(Self::leaf(Operation::Zero { air: self.carries_air() }));
        }
        self.push_unary(Operation::Scale(strength))
    }

    /// Turn this field's push, read as air velocity in m/s, into wind felt at
    /// `drag_rate` (1/s): a body moving at v is pushed by drag_rate · (wind − v)
    /// (Houdini's POP Wind air resistance). Air the field already carries is
    /// kept.
    pub fn air(&self, drag_rate: f32) -> Result<Self, PhysicsError> {
        if !drag_rate.is_finite() || drag_rate < 0.0 {
            return Err(PhysicsError::InvalidInput(
                "air drag rate must be finite and non-negative",
            ));
        }
        self.push_unary(Operation::Air(drag_rate))
    }

    /// The field as one consumer feels it: its drag rate times `air_drag`
    /// (Blender's per-object field weights). 0 ignores the air. A field with
    /// no air is returned as is.
    /// At Air Drag 0 the air drops out as a zero scale does: only the push
    /// is left, so drifting gusts the consumer ignores never refill its
    /// lattice.
    pub fn with_air_drag(&self, air_drag: f32) -> Result<Self, PhysicsError> {
        if !air_drag.is_finite() || air_drag < 0.0 {
            return Err(PhysicsError::InvalidInput("air drag must be finite and non-negative"));
        }
        if !self.carries_air() {
            return Ok(self.clone());
        }
        if air_drag == 0.0 {
            return Ok(self.without_air());
        }
        self.push_unary(Operation::AirDrag(air_drag))
    }

    /// The field one consumer feels, the same rule for every consumer
    /// (D-114): the scene's `forces` plus the Physics World's still air, the
    /// air felt at the consumer's `air_drag`. Still air is air at rest felt
    /// at `air_resistance` (1/s), so with no wind a body under gravity
    /// reaches a terminal speed (Houdini's POP Drag). In wind the two mix by
    /// their rates, as several winds do. At `air_resistance` 0 the forces are
    /// felt exactly as without still air.
    pub fn felt_in_still_air(
        forces: Option<&Self>,
        air_resistance: f32,
        air_drag: f32,
    ) -> Result<Option<Self>, PhysicsError> {
        if !air_resistance.is_finite() || air_resistance < 0.0 {
            return Err(PhysicsError::InvalidInput("air resistance must be finite and non-negative"));
        }
        let field = if air_resistance == 0.0 {
            forces.cloned()
        } else {
            let still = Self::uniform([0.0; 3])?.air(air_resistance)?;
            Some(match forces {
                Some(forces) => forces.sum(&still)?,
                None => still,
            })
        };
        field.map(|field| field.with_air_drag(air_drag)).transpose()
    }

    /// The field with every entry that became air replaced by nothing. Air
    /// zeroes its entry's push, and nothing after it turns drag back into
    /// push, so what is left samples as this field felt at Air Drag 0.
    fn without_air(&self) -> Self {
        let mut operations = Program::new();
        // Where each stack entry starts in `operations`.
        let mut starts = SmallVec::<[usize; INLINE_OPERATIONS]>::new();
        for operation in &self.operations {
            match operation {
                Operation::Air(_) => {
                    operations.truncate(*starts.last().expect("air follows its entry"));
                    operations.push(Operation::Zero { air: true });
                    continue;
                }
                Operation::Add | Operation::Multiply => {
                    starts.pop();
                }
                Operation::Scale(_) | Operation::AirDrag(_) => {}
                _ => starts.push(operations.len()),
            }
            operations.push(operation.clone());
        }
        let depth = stack_depth(&operations);
        Self { operations, depth }
    }

    /// Whether any part of this field is air, switched on or not.
    pub fn carries_air(&self) -> bool {
        self.operations
            .iter()
            .any(|operation| matches!(operation, Operation::Air(_) | Operation::Zero { air: true }))
    }
}

/// The most entries a postfix program holds at once.
fn stack_depth(operations: &[Operation]) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    for operation in operations {
        match operation {
            Operation::Add | Operation::Multiply => depth = depth.saturating_sub(1),
            Operation::Scale(_) | Operation::Air(_) | Operation::AirDrag(_) => {}
            _ => {
                depth += 1;
                deepest = deepest.max(depth);
            }
        }
    }
    deepest
}

/// One leaf of a [`FieldValue`], for editor overlays.
#[derive(Clone, Copy, Debug)]
pub enum FieldLeaf<'a> {
    Uniform(&'a UniformField),
    Radial(&'a RadialField),
    Vortex(&'a VortexField),
    Sampled(&'a SampledField),
    Turbulence(&'a TurbulenceField),
}

impl FieldValue {
    /// Visit every leaf with the product of the scale operations applied
    /// to it, so a negative strength reads as a reversed leaf. A product with
    /// another field is not folded into either side's scale; air operations
    /// leave the scale as it is.
    pub fn for_each_leaf(&self, mut visit: impl FnMut(FieldLeaf<'_>, f32)) {
        let operations = &self.operations;
        // Each leaf's operation index and scale, and the first leaf of each
        // stack entry: a postfix program keeps an entry's leaves contiguous.
        let mut leaves = SmallVec::<[(usize, f32); INLINE_OPERATIONS]>::new();
        let mut starts = SmallVec::<[usize; INLINE_OPERATIONS]>::new();
        for (index, operation) in operations.iter().enumerate() {
            match operation {
                Operation::Uniform(_)
                | Operation::Radial(_)
                | Operation::Vortex(_)
                | Operation::Sampled(_)
                | Operation::Turbulence(_)
                | Operation::Zero { .. } => {
                    starts.push(leaves.len());
                    leaves.push((index, 1.0));
                }
                Operation::Add | Operation::Multiply => {
                    starts.pop();
                }
                Operation::Scale(strength) => {
                    let start = *starts.last().expect("a scale follows its entry");
                    for leaf in &mut leaves[start..] {
                        leaf.1 *= strength;
                    }
                }
                Operation::Air(_) | Operation::AirDrag(_) => {}
            }
        }
        for &(index, scale) in &leaves {
            let leaf = match &operations[index] {
                Operation::Uniform(field) => FieldLeaf::Uniform(field),
                Operation::Radial(field) => FieldLeaf::Radial(field),
                Operation::Vortex(field) => FieldLeaf::Vortex(field),
                Operation::Sampled(field) => FieldLeaf::Sampled(field),
                Operation::Turbulence(field) => FieldLeaf::Turbulence(field),
                // Nothing acts there, so nothing is drawn.
                Operation::Zero { .. } => continue,
                _ => unreachable!("only leaves are recorded"),
            };
            visit(leaf, scale);
        }
    }
}

impl VectorField for FieldValue {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        self.sample_force(position).push
    }

    fn sample_force(&self, position: [f32; 3]) -> ForceSample {
        let mut stack = SmallVec::<[ForceSample; INLINE_OPERATIONS]>::new();
        stack.resize(self.depth, ForceSample::default());
        let mut depth = 0;
        for operation in &self.operations {
            let leaf = match operation {
                Operation::Uniform(field) => Some(field.sample(position)),
                Operation::Radial(field) => Some(field.sample(position)),
                Operation::Vortex(field) => Some(field.sample(position)),
                Operation::Sampled(field) => Some(field.sample(position)),
                Operation::Turbulence(field) => Some(field.sample(position)),
                Operation::Zero { .. } => Some([0.0; 3]),
                Operation::Add => {
                    depth -= 1;
                    stack[depth - 1] = stack[depth - 1] + stack[depth];
                    None
                }
                Operation::Multiply => {
                    // Construction refuses air on either side.
                    depth -= 1;
                    let rhs = stack[depth].push;
                    let lhs = &mut stack[depth - 1].push;
                    for axis in 0..3 {
                        lhs[axis] *= rhs[axis];
                    }
                    None
                }
                Operation::Scale(strength) => {
                    stack[depth - 1] = stack[depth - 1].scaled(*strength);
                    None
                }
                Operation::Air(rate) => {
                    let top = &mut stack[depth - 1];
                    top.drag += rate;
                    for axis in 0..3 {
                        top.drag_wind[axis] += rate * top.push[axis];
                    }
                    top.push = [0.0; 3];
                    None
                }
                Operation::AirDrag(weight) => {
                    let top = &mut stack[depth - 1];
                    top.drag *= weight;
                    top.drag_wind = top.drag_wind.map(|c| c * weight);
                    None
                }
            };
            if let Some(value) = leaf {
                stack[depth] = ForceSample::push(value);
                depth += 1;
            }
        }
        debug_assert_eq!(depth, 1);
        stack[0]
    }
}

#[cfg(test)]
mod force_set_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_field_composition_samples_all_contributors() {
        let uniform = FieldValue::uniform([2.0, -1.0, 0.0]).unwrap();
        let radial = FieldValue::radial([0.0; 3], 4.0, 1.0).unwrap();
        let vortex = FieldValue::vortex([0.0; 3], [0.0, 1.0, 0.0], 4.0, 1.0).unwrap();
        let combined = uniform
            .sum(&radial)
            .unwrap()
            .sum(&vortex)
            .unwrap()
            .scaled(2.0)
            .unwrap();
        assert_eq!(combined.sample([2.0, 0.0, 0.0]), [5.0, -2.0, -1.0]);
        assert_eq!(combined.sample([5.0, 0.0, 0.0]), [4.0, -2.0, 0.0]);
        assert_eq!(uniform.sample([2.0, 0.0, 0.0]), [2.0, -1.0, 0.0]);
    }

    #[test]
    fn field_leaves_carry_the_scales_applied_to_them() {
        let wind = FieldValue::uniform([1.0, 0.0, 0.0]).unwrap().scaled(-2.0).unwrap();
        let burst = FieldValue::radial([0.0, 1.0, 0.0], 0.5, 1.0).unwrap();
        let field = wind.sum(&burst).unwrap().scaled(3.0).unwrap();
        let mut seen = Vec::new();
        field.for_each_leaf(|leaf, scale| seen.push((matches!(leaf, FieldLeaf::Uniform(_)), scale)));
        assert_eq!(seen, [(true, -6.0), (false, 3.0)]);
    }

    #[test]
    fn owned_field_grid_mask_shares_storage_and_preserves_world_coordinates() {
        let grid =
            Arc::new(SampledField::new([10.0, 0.0, 0.0], 1.0, [2; 3], vec![[0.25; 3]; 8]).unwrap());
        let mask = FieldValue::sampled(grid.clone());
        let field = FieldValue::uniform([4.0, 8.0, -4.0])
            .unwrap()
            .multiply(&mask)
            .unwrap();
        let copy = field.clone();
        assert_eq!(Arc::strong_count(&grid), 4);
        assert_eq!(copy.sample([10.5, 0.5, 0.5]), [1.0, 2.0, -1.0]);
        assert_eq!(copy.sample([0.5; 3]), [0.0; 3]);
        assert_eq!(field, copy);
    }

    #[test]
    fn owned_field_rejects_invalid_values_and_grows_without_a_limit() {
        assert!(FieldValue::uniform([f32::NAN, 0.0, 0.0]).is_err());
        assert!(FieldValue::radial([0.0; 3], 0.0, 1.0).is_err());
        assert!(FieldValue::vortex([0.0; 3], [0.0; 3], 1.0, 1.0).is_err());
        let mut field = FieldValue::uniform([1.0; 3]).unwrap();
        assert!(field.scaled(f32::INFINITY).is_err());
        assert_eq!(field.sample([0.0; 3]), [1.0; 3], "a refused edit leaves the field as it was");
        // Many forces summed (ruling 23: no fixed count caps forces).
        for _ in 0..40 {
            field = field.sum(&FieldValue::uniform([1.0; 3]).unwrap().scaled(0.5).unwrap()).unwrap();
        }
        assert_eq!(field.sample([0.0; 3]), [21.0; 3]);
    }
}
