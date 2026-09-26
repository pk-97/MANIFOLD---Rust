//! Owned, bounded vector expressions for graph wires and retained tick inputs.
//!
//! The native adapters still consume `VectorField`; this value supplies owned
//! snapshots without borrowing a renderer graph or allocating while sampling.
use std::sync::Arc;

use crate::{PhysicsError, RadialField, SampledField, UniformField, VectorField, VortexField};

const MAX_OPERATIONS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
enum Operation {
    Unused,
    Uniform(UniformField),
    Radial(RadialField),
    Vortex(VortexField),
    Sampled(Arc<SampledField>),
    Add,
    Multiply,
    Scale(f32),
}

/// A dimensionless spatial vector expression. Strength belongs to the
/// consuming `FieldInput` or an explicit scale operation. Leaf values own
/// their parameters; sampled grids share immutable prepared storage.
///
/// Numeric edits, composition, cloning and sampling require no heap allocation.
/// The 32-operation bound is explicit: overly complex graphs fail preparation
/// rather than silently omitting forces. Native worlds never retain borrowed
/// graph nodes through this value.
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
        RadialField::new(center, radius, falloff).map(|field| Self::leaf(Operation::Radial(field)))
    }

    pub fn vortex(
        center: [f32; 3],
        axis: [f32; 3],
        radius: f32,
        falloff: f32,
    ) -> Result<Self, PhysicsError> {
        VortexField::new(center, axis, radius, falloff)
            .map(|field| Self::leaf(Operation::Vortex(field)))
    }

    pub fn sampled(grid: Arc<SampledField>) -> Self {
        Self::leaf(Operation::Sampled(grid))
    }

    pub fn sum(&self, other: &Self) -> Result<Self, PhysicsError> {
        self.combine(other, Operation::Add)
    }

    /// Component-wise product. A grid with equal XYZ components acts as a
    /// scalar spatial mask; arbitrary vector inputs remain composable.
    pub fn multiply(&self, other: &Self) -> Result<Self, PhysicsError> {
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

    pub fn scaled(&self, strength: f32) -> Result<Self, PhysicsError> {
        if !strength.is_finite() {
            return Err(PhysicsError::InvalidInput(
                "vector field scale must be finite",
            ));
        }
        if self.len == MAX_OPERATIONS {
            return Err(PhysicsError::InvalidInput(
                "vector field exceeds 32 operations",
            ));
        }
        let mut result = self.clone();
        result.operations[self.len] = Operation::Scale(strength);
        result.len += 1;
        Ok(result)
    }
}

impl VectorField for FieldValue {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let mut stack = [[0.0; 3]; MAX_OPERATIONS];
        let mut depth = 0;
        for operation in &self.operations[..self.len] {
            let leaf = match operation {
                Operation::Uniform(field) => Some(field.sample(position)),
                Operation::Radial(field) => Some(field.sample(position)),
                Operation::Vortex(field) => Some(field.sample(position)),
                Operation::Sampled(field) => Some(field.sample(position)),
                Operation::Add | Operation::Multiply => {
                    let rhs = stack[depth - 1];
                    depth -= 1;
                    let lhs = &mut stack[depth - 1];
                    for axis in 0..3 {
                        lhs[axis] = match operation {
                            Operation::Add => lhs[axis] + rhs[axis],
                            _ => lhs[axis] * rhs[axis],
                        };
                    }
                    None
                }
                Operation::Scale(strength) => {
                    for component in &mut stack[depth - 1] {
                        *component *= strength;
                    }
                    None
                }
                Operation::Unused => unreachable!("field program is privately constructed"),
            };
            if let Some(value) = leaf {
                stack[depth] = value;
                depth += 1;
            }
        }
        debug_assert_eq!(depth, 1);
        stack[0]
    }
}

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
