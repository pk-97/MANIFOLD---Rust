//! `node.trigger_gate` — gate a `trigger_count` scalar stream so
//! downstream consumers only see advances while the `enable` toggle is
//! true. Advances that happen while disabled are silently absorbed —
//! re-enabling does NOT fire a backlog of pending triggers.
//!
//! The legacy NestedCubes generator did this internally: `triggered`
//! flag gated whether each `trigger_count != last_trigger_count` event
//! actually advanced the angles. Extracting it as a primitive lets the
//! graph express "this generator listens to clip triggers only when
//! the toggle is on" as wiring, not as a property of the consumer.
//!
//! Pair with `system.generator_input.trigger_count` upstream and any
//! number of consumers downstream — `cycle_table_row`,
//! `scalar_array_accumulator`, `nested_cubes_geometry`, etc.
//!
//! Wiring `initial_count` supplies the counter immediately before a real
//! pending event. It lets a fresh gate pass that event on its first sample.
//! Unwired gates retain first-sample suppression. Disabled gates absorb the
//! first event too; re-enabling never replays it.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: TriggerGate,
    type_id: "node.trigger_gate",
    purpose: "Gate a trigger_count scalar stream. When `enable` is true, advances pass through (output += input - previous_input) and emit a one-frame `pulse`; when false, advances are absorbed — output stays frozen and re-enabling does NOT fire a backlog. Equivalent to the legacy `if triggered { advance }` gate pattern, expressed as a graph wire instead of consumer-internal state.",
    inputs: {
        trigger_count: ScalarF32 optional,
        enable: ScalarF32 optional,
        initial_count: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
        pulse: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("enable"),
            label: "Enable",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Both `trigger_count` and `enable` are port-shadows-param. When the enable wire is present, values > 0.5 are treated as enabled (BoolThreshold semantics). When absent, the `enable` param drives. `pulse` is 1.0 only on an enabled advance and 0.0 on every other evaluation, so it can drive one-shot reset or seed paths. State (last_input, output_count) is fresh on rebuild per the graph-editor-is-authoring-not-perform rule.",
    examples: ["NestedCubes"],
    picker: { label: "Trigger Gate", category: Driver },
    summary: "Passes a trigger stream through only while it is enabled, so you can switch a clip-trigger source on and off.",
    category: Control,
    role: Control,
    aliases: ["trigger gate", "gate", "enable"],
    boundary_reason: NonGpu,
    extra_fields: {
        last_input: Option<u32> = None,
        output_count: u32 = 0,
        pulse: f32 = 0.0,
    },
}

impl Primitive for TriggerGate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let raw_input = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let input = raw_input.round().max(0.0) as u32;

        // BoolThreshold semantics: > 0.5 → enabled. Wire wins; param
        // is the fallback. Both Float (slider) and Bool (toggle) variants
        // collapse to the same threshold check.
        let enabled = match ctx.inputs.scalar("enable") {
            Some(ParamValue::Float(f)) => f > 0.5,
            Some(ParamValue::Bool(b)) => b,
            _ => match ctx.params.get("enable") {
                Some(ParamValue::Bool(b)) => *b,
                Some(ParamValue::Float(f)) => *f > 0.5,
                _ => true,
            },
        };

        let delta = match self.last_input {
            Some(last) => input.saturating_sub(last),
            None => ctx
                .inputs
                .scalar("initial_count")
                .and_then(|value| value.as_scalar())
                .filter(|value| value.is_finite())
                .map_or(0, |baseline| {
                    input.saturating_sub(baseline.round().max(0.0) as u32)
                }),
        };
        if enabled {
            self.output_count = self.output_count.saturating_add(delta);
        }
        self.pulse = if enabled && delta > 0 { 1.0 } else { 0.0 };
        self.last_input = Some(input);

        ctx.outputs
            .set_scalar("out", ParamValue::Float(self.output_count as f32));
        ctx.outputs
            .set_scalar("pulse", ParamValue::Float(self.pulse));
    }

    fn clear_state(&mut self) {
        self.last_input = None;
        self.output_count = 0;
        self.pulse = 0.0;
    }

    /// BUG-104: `output_count` only ever accumulates while `enable` is
    /// true — flipping `enable` back off freezes it but never releases it,
    /// so a downstream mux/consumer stays parked on the frozen count
    /// forever. See `EffectNode::is_trigger_latch`.
    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::MockBackend;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_core::{Beats, Seconds};

    #[test]
    fn declares_three_optional_inputs_and_scalar_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let inputs = TriggerGate::INPUTS;
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name, "trigger_count");
        assert!(!inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(inputs[1].name, "enable");
        assert!(!inputs[1].required);
        assert_eq!(inputs[2].name, "initial_count");
        assert!(!inputs[2].required);

        assert_eq!(TriggerGate::OUTPUTS.len(), 2);
        assert_eq!(TriggerGate::OUTPUTS[0].name, "out");
        assert_eq!(
            TriggerGate::OUTPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(TriggerGate::OUTPUTS[1].name, "pulse");
        assert_eq!(
            TriggerGate::OUTPUTS[1].ty,
            PortType::Scalar(ScalarType::F32)
        );
    }

    #[test]
    fn declares_single_enable_param_defaulting_true() {
        let params = TriggerGate::PARAMS;
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "enable");
        assert_eq!(params[0].ty, ParamType::Bool);
        assert!(matches!(params[0].default, ParamValue::Bool(true)));
    }

    #[test]
    fn primitive_registers_as_palette_driver() {
        use crate::node_graph::palette::{PaletteCategory, palette_atoms};
        let atoms = palette_atoms();
        let entry = atoms
            .iter()
            .find(|e| e.type_id == TriggerGate::TYPE_ID)
            .expect("trigger_gate should be registered as a palette atom");
        assert_eq!(entry.label, "Trigger Gate");
        assert!(matches!(entry.category, PaletteCategory::Driver));
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        use crate::node_graph::EffectNode;
        let prim = TriggerGate::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }

    fn evaluate_actual(node: &mut TriggerGate, trigger_count: f32, enable: bool) -> (f32, f32) {
        let mut backend = MockBackend::new();
        let trigger_slot = backend.acquire(
            ResourceId(0),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let enable_slot = backend.acquire(
            ResourceId(1),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let out_slot = backend.acquire(
            ResourceId(2),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let pulse_slot = backend.acquire(
            ResourceId(3),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        backend.set_scalar(trigger_slot, ParamValue::Float(trigger_count));
        backend.set_scalar(enable_slot, ParamValue::Bool(enable));

        let params = ParamValues::default();
        let inputs_bindings = [("trigger_count", trigger_slot), ("enable", enable_slot)];
        let outputs_bindings = [("out", out_slot), ("pulse", pulse_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        {
            let inputs = NodeInputs::new(&inputs_bindings, &backend, &[]);
            let outputs = NodeOutputs::new(
                &outputs_bindings,
                &backend,
                &mut scalar_scratch,
                &mut camera_scratch,
                &mut light_scratch,
                &mut material_scratch,
                &mut transform_scratch,
                &mut atmosphere_scratch,
                &mut render_mode_scratch,
                &mut object_scratch,
            );
            let frame_time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            };
            let mut ctx = EffectNodeContext::new(frame_time, &params, inputs, outputs, None);
            node.run(&mut ctx);
        }
        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }
        let read = |slot| match backend.scalar(slot) {
            Some(ParamValue::Float(value)) => value,
            other => panic!("expected scalar output, got {other:?}"),
        };
        (read(out_slot), read(pulse_slot))
    }

    #[test]
    fn pulse_is_one_frame_for_enabled_advance_and_absorbs_disabled_backlog() {
        let mut node = TriggerGate::new();
        assert_eq!(evaluate_actual(&mut node, 0.0, true), (0.0, 0.0));
        assert_eq!(evaluate_actual(&mut node, 1.0, true), (1.0, 1.0));
        assert_eq!(evaluate_actual(&mut node, 1.0, true), (1.0, 0.0));
        assert_eq!(evaluate_actual(&mut node, 3.0, false), (1.0, 0.0));
        assert_eq!(evaluate_actual(&mut node, 4.0, true), (2.0, 1.0));
    }

    #[test]
    fn clear_state_clears_pulse_and_rearms_baseline() {
        let mut node = TriggerGate::new();
        assert_eq!(evaluate_actual(&mut node, 4.0, true), (0.0, 0.0));
        assert_eq!(evaluate_actual(&mut node, 5.0, true), (1.0, 1.0));
        node.clear_state();
        assert_eq!(evaluate_actual(&mut node, 5.0, true), (0.0, 0.0));
        assert_eq!(evaluate_actual(&mut node, 6.0, true), (1.0, 1.0));
    }
}
