//! Boundary nodes — [`Source`] and [`FinalOutput`].
//!
//! Every composite graph has these two as its input and output edges. They
//! are not full effects; they exist so the graph has a well-defined place
//! for data to enter and leave.
//!
//! ## How they work
//!
//! [`Source`] declares one [`NodeOutput`] (`out`, Texture2D, V1 constraint)
//! and zero inputs. Its `evaluate` is a no-op — the runtime will, in a
//! future step, pre-bind the host's input frame to Source's output slot
//! before each frame, so downstream nodes naturally read from it via the
//! resource pool's slot lookup.
//!
//! [`FinalOutput`] is the mirror: one [`NodeInput`] (`in`, Texture2D), zero
//! outputs, evaluate is a no-op. The host fishes the result out of
//! FinalOutput's bound input slot after each frame.
//!
//! Both nodes are intentionally trivial. The interesting work — pre-binding
//! the source frame and post-reading the final output — happens in the
//! [`Executor`](crate::node_graph::Executor) once the real backend lands.

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::ParamDef;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

/// Stable type ID for [`Source`].
pub const SOURCE_TYPE_ID: &str = "system.source";

/// Stable type ID for [`FinalOutput`].
pub const FINAL_OUTPUT_TYPE_ID: &str = "system.final_output";

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

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{compile, validate, Executor, FrameTime, Graph, GraphError};

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
