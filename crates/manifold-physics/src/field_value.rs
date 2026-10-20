//! Owned, bounded vector expressions for graph wires and retained tick inputs.
//!
//! The native adapters still consume `VectorField`; this value supplies owned
//! snapshots without borrowing a renderer graph or allocating while sampling.
use std::sync::Arc;

use crate::interaction::ForceSample;
use crate::{
    FalloffCurve, PhysicsError, RadialField, SampledField, TurbulenceField, UniformField, VectorField,
    VortexField,
};

mod serialization;

const MAX_OPERATIONS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
enum Operation {
    Unused,
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
}

/// A dimensionless spatial vector expression. Strength belongs to the
/// consuming `FieldInput` or an explicit scale operation. Leaf values own
/// their parameters; sampled grids share immutable prepared storage.
///
/// Numeric edits, composition, cloning and sampling require no heap allocation.
/// The 32-operation bound is explicit: overly complex graphs fail preparation
/// rather than silently omitting forces. Native worlds never retain borrowed
/// graph nodes through this value.
///
/// Besides its push a field can carry air ([`Self::air`]): a wind velocity and
/// a drag rate, read through [`VectorField::sample_force`].
#[derive(Clone, Debug, PartialEq)]
pub struct FieldValue {
    operations: [Operation; MAX_OPERATIONS],
    len: usize,
}

impl FieldValue {
    fn leaf(operation: Operation) -> Self {
        let mut operations = std::array::from_fn(|_| Operation::Unused);
        operations[0] = operation;
        Self { operations, len: 1 }
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
        let len = self.len + other.len + 1;
        if len > MAX_OPERATIONS {
            return Err(PhysicsError::InvalidInput(
                "vector field exceeds 32 operations",
            ));
        }
        let mut result = self.clone();
        result.operations[self.len..len - 1].clone_from_slice(&other.operations[..other.len]);
        result.operations[len - 1] = operation;
        result.len = len;
        Ok(result)
    }

    fn push_unary(&self, operation: Operation) -> Result<Self, PhysicsError> {
        if self.len == MAX_OPERATIONS {
            return Err(PhysicsError::InvalidInput(
                "vector field exceeds 32 operations",
            ));
        }
        let mut result = self.clone();
        result.operations[self.len] = operation;
        result.len += 1;
        Ok(result)
    }

    /// Scale the push; the drag rate scales by the strength's size and the
    /// wind keeps its speed (see [`ForceSample::scaled`]).
    pub fn scaled(&self, strength: f32) -> Result<Self, PhysicsError> {
        if !strength.is_finite() {
            return Err(PhysicsError::InvalidInput(
                "vector field scale must be finite",
            ));
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
    pub fn with_air_drag(&self, air_drag: f32) -> Result<Self, PhysicsError> {
        if !air_drag.is_finite() || air_drag < 0.0 {
            return Err(PhysicsError::InvalidInput("air drag must be finite and non-negative"));
        }
        if !self.carries_air() {
            return Ok(self.clone());
        }
        self.push_unary(Operation::AirDrag(air_drag))
    }

    /// Whether any part of this field is air.
    pub fn carries_air(&self) -> bool {
        self.operations[..self.len].iter().any(|operation| matches!(operation, Operation::Air(_)))
    }
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
        let operations = &self.operations[..self.len];
        // Each leaf's operation index and scale, and the first leaf of each
        // stack entry: a postfix program keeps an entry's leaves contiguous.
        let mut leaves = [(0usize, 1.0f32); MAX_OPERATIONS];
        let mut starts = [0usize; MAX_OPERATIONS];
        let (mut count, mut depth) = (0, 0);
        for (index, operation) in operations.iter().enumerate() {
            match operation {
                Operation::Uniform(_)
                | Operation::Radial(_)
                | Operation::Vortex(_)
                | Operation::Sampled(_)
                | Operation::Turbulence(_) => {
                    starts[depth] = count;
                    depth += 1;
                    leaves[count] = (index, 1.0);
                    count += 1;
                }
                Operation::Add | Operation::Multiply => depth -= 1,
                Operation::Scale(strength) => {
                    for leaf in &mut leaves[starts[depth - 1]..count] {
                        leaf.1 *= strength;
                    }
                }
                Operation::Air(_) | Operation::AirDrag(_) => {}
                Operation::Unused => unreachable!("field program is privately constructed"),
            }
        }
        for &(index, scale) in &leaves[..count] {
            let leaf = match &operations[index] {
                Operation::Uniform(field) => FieldLeaf::Uniform(field),
                Operation::Radial(field) => FieldLeaf::Radial(field),
                Operation::Vortex(field) => FieldLeaf::Vortex(field),
                Operation::Sampled(field) => FieldLeaf::Sampled(field),
                Operation::Turbulence(field) => FieldLeaf::Turbulence(field),
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
        let mut stack = [ForceSample::default(); MAX_OPERATIONS];
        let mut depth = 0;
        for operation in &self.operations[..self.len] {
            let leaf = match operation {
                Operation::Uniform(field) => Some(field.sample(position)),
                Operation::Radial(field) => Some(field.sample(position)),
                Operation::Vortex(field) => Some(field.sample(position)),
                Operation::Sampled(field) => Some(field.sample(position)),
                Operation::Turbulence(field) => Some(field.sample(position)),
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
                Operation::Unused => unreachable!("field program is privately constructed"),
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
    fn owned_field_rejects_invalid_values_and_complexity_without_partial_mutation() {
        assert!(FieldValue::uniform([f32::NAN, 0.0, 0.0]).is_err());
        assert!(FieldValue::radial([0.0; 3], 0.0, 1.0).is_err());
        assert!(FieldValue::vortex([0.0; 3], [0.0; 3], 1.0, 1.0).is_err());
        let mut field = FieldValue::uniform([1.0; 3]).unwrap();
        assert!(field.scaled(f32::INFINITY).is_err());
        for _ in 1..MAX_OPERATIONS {
            field = field.scaled(1.0).unwrap();
        }
        assert!(field.scaled(1.0).is_err());
        assert!(field.sum(&FieldValue::uniform([2.0; 3]).unwrap()).is_err());
        assert_eq!(field.sample([0.0; 3]), [1.0; 3]);
    }
}
