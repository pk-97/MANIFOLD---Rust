//! `node.switch_value` — N-way scalar selector.
//!
//! Same shape as [`crate::node_graph::primitives::MuxTexture`] but for
//! `Scalar(F32)` ports. Picks one of `in_0..in_7` based on the
//! `selector` input and routes it to `out`. No GPU dispatch — runs in
//! the executor's control-rate pass.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: MuxScalar,
    type_id: "node.switch_value",
    purpose: "N-way scalar selector. Routes one of in_0..in_7 (Scalar F32) to the output based on the selector input (rounded, clamped). Every input is port-shadows-param — wire dynamic sources into the ports, or set the inline `in_N` params for static option tables (curated frequency rates, side counts, gate constants). Useful for trigger-driven parameter switching.",
    inputs: {
        selector: ScalarF32 required,
        in_0: ScalarF32 optional,
        in_1: ScalarF32 optional,
        in_2: ScalarF32 optional,
        in_3: ScalarF32 optional,
        in_4: ScalarF32 optional,
        in_5: ScalarF32 optional,
        in_6: ScalarF32 optional,
        in_7: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("selector"),
            label: "Selector",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 7.0)),
            enum_values: &[],
        },
        ParamDef { name: Cow::Borrowed("in_0"), label: "In 0", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_1"), label: "In 1", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_2"), label: "In 2", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_3"), label: "In 3", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_4"), label: "In 4", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_5"), label: "In 5", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_6"), label: "In 6", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("in_7"), label: "In 7", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Selector value rounds to nearest int, clamps to [0, 8). Both `selector` and every `in_N` data input is port-shadows-param: wire dynamic sources when needed, otherwise set the static fallback inline on the param. This lets a 5-option curated mux ship as ONE node with five inline params rather than six nodes (mux + five `node.value` constants feeding it); the future node UI surfaces the inline values on the node body. No GPU dispatch. Mux-shaped 'input selection' is the documented §7 exception to the no-dead-state rule — the user's mental model of a mux accommodates non-selected inputs being inert.",
    examples: [],
    picker: { label: "Switch (value)", category: Atom },
    summary: "Picks one of several incoming values and passes it through, chosen by a selector number. Use it to flip between sources live.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "mux scalar", "selector", "Switch CHOP"],
    boundary_reason: NonGpu,
}

const MUX_SCALAR_INPUT_PORT_NAMES: [&str; 8] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
];

impl Primitive for MuxScalar {
    fn selected_input_branch(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        // Same wired-selector rule as MuxTexture — see that primitive's
        // docstring for the rationale.
        if wired_inputs.contains(&"selector") {
            return None;
        }
        let selector = match params.get("selector") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let idx = selector.round().clamp(0.0, 7.0) as usize;
        Some(MUX_SCALAR_INPUT_PORT_NAMES[idx])
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let selector = ctx.scalar_or_param("selector", 0.0);
        let raw_idx = selector.round().clamp(0.0, 7.0) as usize;
        // Port-shadows-param on every data input: wired source wins,
        // otherwise the inline `in_N` param value (defaults to 0.0)
        // drives the selected slot. Collapses static option tables
        // from "mux + N node.value constants + N wires" down to "mux
        // with N inline params" — the same UX as a curated enum mux
        // but without the param-list explosion of a sealed primitive.
        let value = ctx.scalar_or_param(MUX_SCALAR_INPUT_PORT_NAMES[raw_idx], 0.0);
        ctx.outputs.set_scalar("out", ParamValue::Float(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::Value;
    use crate::node_graph::{Executor, FrameTime, Graph, compile};
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn mux_scalar_declares_one_selector_and_eight_optional_inputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let inputs = MuxScalar::INPUTS;
        assert_eq!(inputs.len(), 9);
        assert_eq!(inputs[0].name, "selector");
        assert!(inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        for port in inputs.iter().skip(1) {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(MuxScalar::OUTPUTS.len(), 1);
        assert_eq!(MuxScalar::OUTPUTS[0].name, "out");
        assert_eq!(MuxScalar::OUTPUTS[0].ty, PortType::Scalar(ScalarType::F32));
    }

    #[test]
    fn mux_scalar_compiles_and_executes_in_a_graph() {
        // Build mux with 3 wired Value inputs and a wired selector.
        // The execute pass releases mux.out's resource immediately
        // (no downstream consumer), so we can't inspect the value
        // here — just confirm the graph compiles and runs without
        // panicking. End-to-end value-flow is covered once a Tier 1
        // generator wires a mux into something downstream.
        let mut g = Graph::new();

        let sel = g.add_node(Box::new(Value::new()));
        g.set_param(sel, "value", ParamValue::Float(1.0)).unwrap();
        let v0 = g.add_node(Box::new(Value::new()));
        let v1 = g.add_node(Box::new(Value::new()));
        let v2 = g.add_node(Box::new(Value::new()));
        g.set_param(v0, "value", ParamValue::Float(11.0)).unwrap();
        g.set_param(v1, "value", ParamValue::Float(22.0)).unwrap();
        g.set_param(v2, "value", ParamValue::Float(33.0)).unwrap();

        let mux = g.add_node(Box::new(MuxScalar::new()));
        g.connect((sel, "out"), (mux, "selector")).unwrap();
        g.connect((v0, "out"), (mux, "in_0")).unwrap();
        g.connect((v1, "out"), (mux, "in_1")).unwrap();
        g.connect((v2, "out"), (mux, "in_2")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn mux_scalar_registers_with_palette() {
        let prim = MuxScalar::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.switch_value");
    }
}
