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
use std::borrow::Cow;

/// Stable type ID for [`Source`].
pub const SOURCE_TYPE_ID: &str = "system.source";

/// Stable type ID for [`FinalOutput`].
pub const FINAL_OUTPUT_TYPE_ID: &str = "system.final_output";

/// Stable type ID for [`GeneratorInput`].
pub const GENERATOR_INPUT_TYPE_ID: &str = "system.generator_input";

const GENERATOR_INPUT_OUTPUTS: [NodeOutput; 9] = [
    NodePort {
        name: Cow::Borrowed("time"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    // Frame-normalized timestep: `delta_seconds * 60`, i.e. 1.0 at 60fps. This
    // is the timestep particle integrators want (motion stays real-time-
    // consistent regardless of actual frame rate) — the same value the loose
    // atoms compute as `dt_scaled` in their run(). Sourced from the frame clock
    // (not a host param) so a FUSED kernel wired to this matches the unfused
    // chain bit-for-bit. The buffer-fusion installer wires member `dt_scaled`
    // fields here.
    NodePort {
        name: Cow::Borrowed("frame_delta"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    // Integer frame counter as f32 (exact below ~16M frames ≈ 3 days at 60fps).
    // Drives per-frame hash reseeding (anti-clump, diffusion). Sourced from the
    // frame clock for the same fused/unfused parity reason as `frame_delta`.
    NodePort {
        name: Cow::Borrowed("frame_count"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("beat"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("aspect"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("trigger_count"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("anim_progress"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("output_width"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("output_height"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Output,
        required: false,
    },
];

const SOURCE_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const FINAL_OUTPUT_INPUTS: [NodeInput; 1] = [NodePort {
    name: Cow::Borrowed("in"),
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
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }
    // depth_rule: the host pre-binds an externally-supplied frame to this
    // node's output slot every frame (design doc DEPTH_RELIGHT_DESIGN.md D1)
    // — there is no upstream to inherit FROM, but it is the depth chain's
    // entry point, not a dead end, so `Inherit` (not `Terminal`) per the
    // task brief's explicit boundary-node ruling.
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
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
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }
    // depth_rule: passes the input's depth straight through to the host
    // display — the exit boundary, symmetric with Source's entry boundary.
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
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
    fn is_liveness_root(&self) -> bool {
        // FinalOutput writes to the host display — always live by definition.
        true
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
        name: Cow::Borrowed("time"),
        label: "Time (s)",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("beat"),
        label: "Beat",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("aspect"),
        label: "Aspect Ratio",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(1.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("trigger_count"),
        label: "Trigger Count",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("anim_progress"),
        label: "Anim Progress",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(0.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("output_width"),
        label: "Output Width",
        ty: crate::node_graph::parameters::ParamType::Float,
        default: ParamValue::Float(1920.0),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("output_height"),
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
/// Nine scalar outputs. Seven (`time`, `beat`, `aspect`, `trigger_count`,
/// `anim_progress`, `output_width`, `output_height`) are each driven by a
/// same-named float parameter the host updates each frame via the standard
/// [`Graph::set_param`](crate::node_graph::Graph::set_param) path; `evaluate`
/// reads them and writes the matching output slots. Two (`frame_delta`,
/// `frame_count`) are read straight from the frame clock (`ctx.time`) instead —
/// they have no host param — so a fused particle kernel wired to them is
/// bit-identical to the unfused chain, whose atoms read the same `ctx.time`.
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
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }
    // depth_rule: emits only control-rate scalars (time/beat/aspect/…), no
    // texture — a generator graph's depth chain has no origin here.
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
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
        // Frame-clock-sourced (not host params): these must match exactly what
        // the standalone particle atoms read in their run() so a fused kernel
        // wired here is bit-identical to the unfused chain. `frame_delta` is the
        // ×60 frame-normalized timestep euler/radial-burst call `dt_scaled`;
        // `frame_count` is the integer counter anti-clump/diffusion reseed on.
        let frame_delta = ctx.time.delta.0 as f32 * 60.0;
        let frame_count = ctx.time.frame_count as f32;
        ctx.outputs
            .set_scalar("frame_delta", ParamValue::Float(frame_delta));
        ctx.outputs
            .set_scalar("frame_count", ParamValue::Float(frame_count));
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
    fn generator_input_declares_nine_scalar_outputs() {
        let g = GeneratorInput::new();
        assert_eq!(g.inputs().len(), 0);
        let outs = g.outputs();
        assert_eq!(outs.len(), 9);
        let names: Vec<&str> = outs.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "time",
                "frame_delta",
                "frame_count",
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

    /// frame_delta / frame_count are frame-CLOCK-sourced (not host params), so
    /// they have no matching param entry — the output count exceeds the param
    /// count by exactly those two. Guards the fused/unfused parity contract:
    /// these must track ctx.time, never a host-settable value.
    #[test]
    fn frame_delta_and_frame_count_are_clock_sourced_not_params() {
        let g = GeneratorInput::new();
        let out_names: Vec<&str> = g.outputs().iter().map(|p| p.name.as_ref()).collect();
        let param_names: Vec<&str> = g.parameters().iter().map(|p| p.name.as_ref()).collect();
        assert!(out_names.contains(&"frame_delta"));
        assert!(out_names.contains(&"frame_count"));
        assert!(!param_names.contains(&"frame_delta"));
        assert!(!param_names.contains(&"frame_count"));
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
        let names: Vec<&str> = g.parameters().iter().map(|p| p.name.as_ref()).collect();
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
