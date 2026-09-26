//! `VectorField` — owned CPU payload carried on vector-field graph wires.

//! The wire stores [`manifold_physics::FieldValue`] so native physics
//! adapters can receive an owned evaluator without borrowing graph nodes.

use manifold_physics::{FieldValue, VectorField};

/// Interpolate two retained field evaluations at one physics time. This blends
/// the sampled vectors, without rebuilding a program or allocating per sample.
/// `origin` translates native domain-local positions into scene coordinates.
pub(crate) struct ContinuousField<'a> {
    pub before: Option<&'a FieldValue>,
    pub after: Option<&'a FieldValue>,
    pub alpha: f32,
    pub origin: [f32; 3],
}

impl ContinuousField<'_> {
    pub fn is_empty(&self) -> bool {
        self.before.is_none() && self.after.is_none()
    }
}

impl VectorField for ContinuousField<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let position = std::array::from_fn(|axis| position[axis] + self.origin[axis]);
        let before = self.before.map_or([0.0; 3], |field| field.sample(position));
        let after = self.after.map_or([0.0; 3], |field| field.sample(position));
        // Avoid an unused endpoint contaminating the exact boundary with NaN.
        if self.alpha <= 0.0 {
            before
        } else if self.alpha >= 1.0 {
            after
        } else {
            std::array::from_fn(|axis| before[axis] + (after[axis] - before[axis]) * self.alpha)
        }
    }
}

#[cfg(test)]
mod tests {
    use manifold_physics::{FieldValue, VectorField};

    use super::super::{Backend, MockBackend, NodeInputs, NodeOutputs, PortType, ResourceId};

    fn field() -> FieldValue {
        FieldValue::uniform([1.0, -2.0, 3.0]).expect("finite uniform field")
    }

    #[test]
    fn vector_field_cpu_wire_round_trip_preserves_value() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::VectorField, None, (0, 0));
        let bindings: &[(&'static str, crate::node_graph::Slot)] = &[("field", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut writes = Vec::new();
        let value = field();
        {
            let mut outputs = NodeOutputs::new(
                bindings,
                &backend,
                &mut scalar,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            )
            .with_vector_field_writes(&mut writes);
            outputs.set_vector_field("field", value.clone());
        }
        for (slot, value) in writes.drain(..) {
            backend.set_vector_field(slot, value);
        }

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        let got = inputs
            .vector_field("field")
            .expect("vector field should be wired");
        assert_eq!(got, value);
        assert_eq!(got.sample([2.0, 4.0, 6.0]), [1.0, -2.0, 3.0]);
    }

    #[test]
    fn vector_field_port_type_is_distinct_from_other_cpu_wires() {
        assert_ne!(PortType::VectorField, PortType::FluidRole);
        assert_ne!(PortType::VectorField, PortType::MeshSource);
        assert_ne!(PortType::VectorField, PortType::RigidBody);
        assert_ne!(PortType::VectorField, PortType::Object);
    }

    #[test]
    fn vector_field_release_and_clear_drop_values() {
        let mut backends: [Box<dyn Backend>; 2] = [
            Box::new(MockBackend::new()),
            Box::new(
                crate::node_graph::metal_backend::MetalBackend::without_device(
                    1,
                    1,
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                ),
            ),
        ];
        for backend in &mut backends {
            for clear in [false, true] {
                let slot = backend.acquire(ResourceId(0), PortType::VectorField, None, (0, 0));
                backend.set_vector_field(slot, field());
                assert!(backend.vector_field(slot).is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::VectorField, None, (0, 0));
                }
                assert!(backend.vector_field(slot).is_none());
            }
        }
    }
}
