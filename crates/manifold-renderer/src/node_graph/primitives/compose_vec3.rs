//! `node.compose_vec3` — construct a CPU scalar Vec3 from three scalar inputs.
//!
//! Each component is port-shadowed by a same-named float parameter, so a
//! static scene bound can be overridden independently by a control wire.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ComposeVec3,
    type_id: "node.compose_vec3",
    purpose: "Construct a ScalarVec3 from three independently wired scalar components. Each component falls back to its same-named float parameter when unwired.",
    inputs: {
        x: ScalarF32 optional,
        y: ScalarF32 optional,
        z: ScalarF32 optional,
    },
    outputs: {
        out: ScalarVec3,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("x"),
            label: "X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("y"),
            label: "Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z"),
            label: "Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Use for CPU-authored vector context such as static scene bounds. Wire any subset of x, y, and z; unwired components retain their float parameters.",
    examples: [],
    picker: { label: "Compose Vec3", category: Driver },
    summary: "Combines three scalar controls into one Vec3 wire.",
    category: Control,
    role: Control,
    aliases: ["compose vector", "xyz", "vector components"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for ComposeVec3 {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let x = ctx.scalar_or_param("x", 0.0);
        let y = ctx.scalar_or_param("y", 0.0);
        let z = ctx.scalar_or_param("z", 0.0);
        ctx.outputs.set_scalar("out", ParamValue::Vec3([x, y, z]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::MockBackend;
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn evaluate(params: [f32; 3], wires: &[(&'static str, f32)]) -> [f32; 3] {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Scalar(ScalarType::Vec3), None, (0, 0));

        let mut values = ParamValues::default();
        for (name, value) in [("x", params[0]), ("y", params[1]), ("z", params[2])] {
            values.insert(Cow::Borrowed(name), ParamValue::Float(value));
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        for (index, &(name, value)) in wires.iter().enumerate() {
            let slot = backend.acquire(
                ResourceId(index as u32 + 1),
                PortType::Scalar(ScalarType::F32),
                None,
                (0, 0),
            );
            backend.set_scalar(slot, ParamValue::Float(value));
            wire_slots.push((name, slot));
        }

        let mut primitive = ComposeVec3::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let mut context = EffectNodeContext::new(
            frame_time(),
            &values,
            inputs,
            outputs,
            None,
        );
        Primitive::run(&mut primitive, &mut context);

        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }
        match backend.scalar(out_slot).expect("Vec3 output should be set") {
            ParamValue::Vec3(value) => value,
            other => panic!("expected Vec3 output, got {other:?}"),
        }
    }

    #[test]
    fn compose_vec3_defaults_and_params_produce_exact_components() {
        assert_eq!(evaluate([1.25, -2.5, 3.75], &[]), [1.25, -2.5, 3.75]);
    }

    #[test]
    fn compose_vec3_wired_component_overrides_only_that_param() {
        assert_eq!(
            evaluate([1.0, 2.0, 3.0], &[("y", 9.0)]),
            [1.0, 9.0, 3.0]
        );
    }

    #[test]
    fn compose_vec3_repeated_evaluation_tracks_changed_scalar_input() {
        assert_eq!(evaluate([0.0, 0.0, 0.0], &[("x", 1.0)]), [1.0, 0.0, 0.0]);
        assert_eq!(evaluate([0.0, 0.0, 0.0], &[("x", 4.0)]), [4.0, 0.0, 0.0]);
    }
}
