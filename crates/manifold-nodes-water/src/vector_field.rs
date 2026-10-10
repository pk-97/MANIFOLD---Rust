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

    use manifold_node_engine::{exec::backend::Backend, exec::backend::MockBackend, exec::cpu_values::CpuWireWrites, bindings::NodeInputs, bindings::NodeOutputs, ports::PortType, exec::execution_plan::ResourceId};

    use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
    use manifold_node_engine::exec::execution::Executor;
    use manifold_node_engine::exec::execution_plan::compile;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::parameters::ParamDef;
    use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind};

    struct ExternalFieldNode {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
    }

    impl ExternalFieldNode {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.external_field"),
                outputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("field"),
                    ty: PortType::VectorField,
                    kind: PortKind::Output,
                    required: false,
                }],
            }
        }
    }

    impl EffectNode for ExternalFieldNode {
        fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
            manifold_node_engine::scene::depth_rule::DepthRule::Terminal
        }

        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }

        fn inputs(&self) -> &[NodeInput] {
            &[]
        }

        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }

        fn parameters(&self) -> &[ParamDef] {
            &[]
        }

        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            ctx.outputs.set_cpu_value(
                "field",
                manifold_physics::FieldValue::uniform([1.0, -2.0, 3.0]).expect("finite uniform field"),
            );
        }
    }

    #[test]
    fn external_field_output_survives_cpu_execute_frame() {
        use manifold_physics::VectorField;
        let mut graph = Graph::new();
        let image = graph.add_node(Box::new(manifold_node_engine::scene::boundary_nodes::Source::new()));
        let out = graph.add_node(Box::new(manifold_node_engine::scene::boundary_nodes::FinalOutput::new()));
        graph.connect((image, "out"), (out, "in")).unwrap();
        let source = graph.add_node(Box::new(ExternalFieldNode::new()));
        assert!(compile(&graph).unwrap().steps().iter().all(|step| step.node != source));
        graph.add_external_output(source, "field").unwrap();
        let plan = compile(&graph).unwrap();
        let resource = plan.steps().iter().find(|step| step.node == source).unwrap().outputs[0].1;

        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(0.0),
            delta: manifold_core::Seconds(1.0 / 60.0),
            frame_count: 0,
        });

        let slot = executor
            .backend()
            .slot_for(resource)
            .expect("external output slot remains bound after frame");
        let field = executor
            .backend()
            .cpu_values()
            .get::<manifold_physics::FieldValue>(slot)
            .expect("external field was published by execute_frame");
        assert_eq!(field.sample([0.0, 0.0, 0.0]), [1.0, -2.0, 3.0]);
    }

    fn field() -> FieldValue {
        FieldValue::uniform([1.0, -2.0, 3.0]).expect("finite uniform field")
    }

    #[test]
    fn vector_field_cpu_wire_round_trip_preserves_value() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::VectorField, None, (0, 0));
        let bindings: &[(&'static str, manifold_node_engine::bindings::Slot)] = &[("field", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut writes = CpuWireWrites::default();
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
            .with_cpu_value_writes(&mut writes);
            outputs.set_cpu_value("field", value.clone());
        }
        writes.commit(backend.cpu_values_mut());

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        let got = inputs
            .cpu_value::<FieldValue>("field")
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
                manifold_node_engine::exec::metal_backend::MetalBackend::without_device(
                    1,
                    1,
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                ),
            ),
        ];
        for backend in &mut backends {
            for clear in [false, true] {
                let slot = backend.acquire(ResourceId(0), PortType::VectorField, None, (0, 0));
                backend.cpu_values_mut().set(slot, field());
                assert!(backend.cpu_values().get::<FieldValue>(slot).is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::VectorField, None, (0, 0));
                }
                assert!(backend.cpu_values().get::<FieldValue>(slot).is_none());
            }
        }
    }
}
