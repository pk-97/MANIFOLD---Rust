//! Boundary nodes — [`Source`], [`FinalOutput`], and [`GeneratorInput`].
//!
//! Every composite graph has these as its input and output edges. They
//! are not full effects; they exist so the graph has a well-defined place
//! for data to enter and leave.
//!
//! ## Effect graphs vs generator graphs
//!
//! Effects sit between an upstream texture and the final output:
//! `Source → … → FinalOutput`. [`Source`] surfaces the host's input
//! frame as the graph's `out`, [`FinalOutput`] consumes the graph's
//! result back to the host.
//!
//! Generators have no upstream texture — they produce one. Their graphs
//! look like `GeneratorInput → … → FinalOutput`. [`GeneratorInput`]
//! surfaces the runtime values a generator needs (time, beat, aspect,
//! trigger count, anim progress) as scalar outputs the primitives can
//! wire into. The host updates these values per frame via
//! [`GeneratorInput::set_frame_context`].
//!
//! [`FinalOutput`] is shared — both effect and generator graphs end at
//! the same boundary. Evaluate is a no-op; the host reads the result
//! from FinalOutput's bound input slot after each frame.
//!
//! All boundary nodes are intentionally trivial. The interesting work —
//! pre-binding inputs and post-reading outputs — happens in the
//! [`Executor`](crate::node_graph::Executor).

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};

/// Stable type ID for [`Source`].
pub const SOURCE_TYPE_ID: &str = "system.source";

/// Stable type ID for [`FinalOutput`].
pub const FINAL_OUTPUT_TYPE_ID: &str = "system.final_output";

/// Stable type ID for [`GeneratorInput`].
pub const GENERATOR_INPUT_TYPE_ID: &str = "system.generator_input";

const GENERATOR_INPUT_OUTPUTS: [NodeOutput; 7] = [
    NodePort {
        name: "time",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "beat",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "aspect",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "trigger_count",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "anim_progress",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "output_width",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: "output_height",
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
];

const SOURCE_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const FINAL_OUTPUT_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

/// Boundary node at the input edge of an effect graph. Declares one
/// `Texture2D` output, no inputs. Evaluate is a no-op; the host pre-binds
/// the input frame to Source's output slot before each frame.
pub struct Source {
    type_id: EffectNodeType,
}

impl Source {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(SOURCE_TYPE_ID),
        }
    }
}

impl Default for Source {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Source {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &SOURCE_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
        // No-op. Host pre-binds the input frame to this node's output slot.
    }
}

/// Boundary node at the output edge of an effect graph. Declares one
/// `Texture2D` input, no outputs. Evaluate is a no-op; the host reads the
/// final result from FinalOutput's bound input slot after each frame.
pub struct FinalOutput {
    type_id: EffectNodeType,
}

impl FinalOutput {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(FINAL_OUTPUT_TYPE_ID),
        }
    }
}

impl Default for FinalOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for FinalOutput {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &FINAL_OUTPUT_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
        // No-op. Host reads the final result from this node's input slot.
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: SOURCE_TYPE_ID,
        create: || Box::new(Source::new()),
        picker: None,
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: FINAL_OUTPUT_TYPE_ID,
        create: || Box::new(FinalOutput::new()),
        picker: None,
    }
}

const GENERATOR_INPUT_PARAMS: [ParamDef; 7] = [
    ParamDef {
        name: "time",
        label: "Time (s)",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "beat",
        label: "Beat",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "aspect",
        label: "Aspect Ratio",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(1.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "trigger_count",
        label: "Trigger Count",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "anim_progress",
        label: "Anim Progress",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "output_width",
        label: "Output Width",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(1920.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "output_height",
        label: "Output Height",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(1080.0),
        range: None,
        enum_values: &[],
    },
];

/// Boundary node at the input edge of a generator graph. Surfaces the
/// host's per-frame timing + trigger state as scalar outputs that
/// generator primitives can wire into.
///
/// Five scalar outputs (`time`, `beat`, `aspect`, `trigger_count`,
/// `anim_progress`), each driven by a same-named float parameter. The
/// host updates the params each frame via the standard
/// [`Graph::set_param`](crate::node_graph::Graph::set_param) path;
/// `evaluate` reads them and writes to the matching output slots.
///
/// Using params instead of internal mutable state means there's no
/// downcast or `as_any_mut` plumbing — the host updates the node
/// through the same path it would update any other primitive.
pub struct GeneratorInput {
    type_id: EffectNodeType,
}

impl GeneratorInput {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(GENERATOR_INPUT_TYPE_ID),
        }
    }
}

impl Default for GeneratorInput {
    fn default() -> Self {
        Self::new()
    }
}

fn read_f(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}

impl EffectNode for GeneratorInput {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &GENERATOR_INPUT_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &GENERATOR_INPUT_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let time = read_f(ctx, "time", 0.0);
        let beat = read_f(ctx, "beat", 0.0);
        let aspect = read_f(ctx, "aspect", 1.0);
        let trigger_count = read_f(ctx, "trigger_count", 0.0);
        let anim_progress = read_f(ctx, "anim_progress", 0.0);
        let output_width = read_f(ctx, "output_width", 1920.0);
        let output_height = read_f(ctx, "output_height", 1080.0);
        ctx.outputs.set_scalar("time", ParamValue::Float(time));
        ctx.outputs.set_scalar("beat", ParamValue::Float(beat));
        ctx.outputs.set_scalar("aspect", ParamValue::Float(aspect));
        ctx.outputs
            .set_scalar("trigger_count", ParamValue::Float(trigger_count));
        ctx.outputs
            .set_scalar("anim_progress", ParamValue::Float(anim_progress));
        ctx.outputs
            .set_scalar("output_width", ParamValue::Float(output_width));
        ctx.outputs
            .set_scalar("output_height", ParamValue::Float(output_height));
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: GENERATOR_INPUT_TYPE_ID,
        create: || Box::new(GeneratorInput::new()),
        picker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{Executor, FrameTime, Graph, GraphError, compile, validate};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn source_and_final_output_have_correct_port_shape() {
        let s = Source::new();
        assert_eq!(s.inputs().len(), 0);
        assert_eq!(s.outputs().len(), 1);
        assert_eq!(s.outputs()[0].name, "out");
        assert_eq!(s.outputs()[0].ty, PortType::Texture2D);

        let f = FinalOutput::new();
        assert_eq!(f.inputs().len(), 1);
        assert_eq!(f.outputs().len(), 0);
        assert_eq!(f.inputs()[0].name, "in");
        assert!(f.inputs()[0].required);
    }

    #[test]
    fn passthrough_graph_compiles_and_executes() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (out, "in")).unwrap();
        validate(&g).unwrap();

        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 2);
        assert_eq!(plan.steps()[0].node, src);
        assert_eq!(plan.steps()[1].node, out);

        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        // Source produces one Texture2D resource; FinalOutput consumes it.
        // Single physical slot used.
        assert_eq!(exec.backend().slot_count(), 1);
    }

    #[test]
    fn final_output_unwired_is_a_validation_error() {
        let mut g = Graph::new();
        g.add_node(Box::new(Source::new()));
        g.add_node(Box::new(FinalOutput::new()));
        // FinalOutput's `in` is required and not wired.
        assert!(matches!(
            validate(&g),
            Err(GraphError::RequiredInputUnwired { .. })
        ));
    }

    #[test]
    fn generator_input_declares_seven_scalar_outputs() {
        let g = GeneratorInput::new();
        assert_eq!(g.inputs().len(), 0);
        let outs = g.outputs();
        assert_eq!(outs.len(), 7);
        let names: Vec<&str> = outs.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "time",
                "beat",
                "aspect",
                "trigger_count",
                "anim_progress",
                "output_width",
                "output_height",
            ]
        );
        for out in outs {
            assert_eq!(out.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn generator_input_in_a_graph_compiles_and_executes() {
        // A bare GeneratorInput is a legal generator-shaped graph root.
        // No downstream consumers needed for the compile path to work
        // (its outputs are optional, just like Source's).
        let mut g = Graph::new();
        g.add_node(Box::new(GeneratorInput::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn generator_input_declares_seven_float_params() {
        let g = GeneratorInput::new();
        let names: Vec<&str> = g.parameters().iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "time",
                "beat",
                "aspect",
                "trigger_count",
                "anim_progress",
                "output_width",
                "output_height",
            ]
        );
        for p in g.parameters() {
            assert!(matches!(p.default, ParamValue::Float(_)));
        }
    }

    #[test]
    fn generator_input_params_drive_scalar_outputs() {
        // Build a graph: generator_input → final_output (no actual
        // downstream consumer of the scalars, but the executor will
        // call evaluate and the assertion is that nothing panics).
        let mut g = Graph::new();
        let gi = g.add_node(Box::new(GeneratorInput::new()));
        g.set_param(gi, "time", ParamValue::Float(2.5)).unwrap();
        g.set_param(gi, "beat", ParamValue::Float(1.25)).unwrap();
        g.set_param(gi, "aspect", ParamValue::Float(1.78)).unwrap();
        g.set_param(gi, "trigger_count", ParamValue::Float(7.0))
            .unwrap();
        g.set_param(gi, "anim_progress", ParamValue::Float(0.5))
            .unwrap();
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn source_alone_is_valid_and_executable() {
        // A "generator-shaped" graph has a Source-less interpretation, but a
        // bare Source by itself with no FinalOutput should still be a
        // legal (if useless) graph: Source has no required ports.
        let mut g = Graph::new();
        g.add_node(Box::new(Source::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }
}
