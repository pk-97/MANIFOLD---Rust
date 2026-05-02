//! Per-frame graph execution.
//!
//! The [`Executor`] takes a [`Graph`] plus a precompiled [`ExecutionPlan`]
//! and runs one frame, delegating physical resource allocation to a
//! [`Backend`].
//!
//! ## Mock vs real GPU
//!
//! [`execute_frame`](Executor::execute_frame) runs without a `GpuEncoder` —
//! suitable for [`MockBackend`] tests that exercise resource lifetime
//! logic without touching Metal. [`execute_frame_with_gpu`](Executor::execute_frame_with_gpu)
//! threads a real encoder through to nodes that issue compute / render
//! passes, and is the production entry point alongside [`MetalBackend`].
//!
//! [`MetalBackend`]: crate::node_graph::MetalBackend

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::backend::{Backend, MockBackend};
use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime};
use crate::node_graph::execution_plan::ExecutionPlan;
use crate::node_graph::graph::Graph;

/// Runs a graph against a precompiled plan, one frame per call.
///
/// The executor owns its [`Backend`] across frames so the high-water mark
/// stabilises after the first frame: slots allocated for frame 0's peak
/// intermediates are reused for every subsequent frame at the same graph
/// topology.
pub struct Executor {
    backend: Box<dyn Backend>,
    /// Scratch buffer reused across steps to avoid per-step allocation.
    /// (Per-frame allocation in tight loops is forbidden by CLAUDE.md.)
    input_scratch: Vec<(&'static str, Slot)>,
    output_scratch: Vec<(&'static str, Slot)>,
}

impl Executor {
    /// Construct an executor with the given backend.
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            input_scratch: Vec::new(),
            output_scratch: Vec::new(),
        }
    }

    /// Convenience constructor with a fresh [`MockBackend`]. Used by tests
    /// and any code that doesn't need real GPU resources.
    pub fn with_mock() -> Self {
        Self::new(Box::new(MockBackend::new()))
    }

    pub fn backend(&self) -> &dyn Backend {
        &*self.backend
    }

    pub fn backend_mut(&mut self) -> &mut dyn Backend {
        &mut *self.backend
    }

    /// Run one frame of the graph without a GPU encoder.
    ///
    /// Convenience entry point for tests against [`MockBackend`] and any
    /// scenario where the graph contains only nodes that don't issue real
    /// GPU work (boundary nodes, stub primitives). Nodes that require an
    /// encoder will panic via [`EffectNodeContext::gpu_encoder`].
    pub fn execute_frame(&mut self, graph: &mut Graph, plan: &ExecutionPlan, time: FrameTime) {
        self.execute_frame_inner(graph, plan, time, None);
    }

    /// Run one frame of the graph with a real `GpuEncoder` available to
    /// every node. Used by the production renderer integration; pairs with
    /// [`MetalBackend`](crate::node_graph::MetalBackend) for real
    /// `GpuTexture` allocation.
    pub fn execute_frame_with_gpu(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        gpu: &mut GpuEncoder<'_>,
    ) {
        self.execute_frame_inner(graph, plan, time, Some(gpu));
    }

    /// Shared implementation. For each step in plan order:
    ///   1. Acquire a slot for every output port (so distinct slots from inputs).
    ///   2. Look up slots for every wired input port.
    ///   3. Call `EffectNode::evaluate` with the assembled context.
    ///   4. Release slots for resources whose last reader is this step.
    ///
    /// The acquire-then-release order is correct because evaluate writes to
    /// outputs while reading from inputs; freeing inputs before allocating
    /// outputs would let the new acquire reuse the still-being-read slot.
    fn execute_frame_inner(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        mut gpu: Option<&mut GpuEncoder<'_>>,
    ) {
        for step in plan.steps() {
            // 1. Acquire output slots.
            self.output_scratch.clear();
            for &(port_name, res_id) in &step.outputs {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let slot = self.backend.acquire(res_id, ty);
                self.output_scratch.push((port_name, slot));
            }

            // 2. Look up input slots.
            self.input_scratch.clear();
            for &(port_name, res_id) in &step.inputs {
                let slot = self
                    .backend
                    .slot_for(res_id)
                    .expect("input resource was acquired in a prior step");
                self.input_scratch.push((port_name, slot));
            }

            // 3. Evaluate. The context holds an immutable backend ref for
            // typed accessor resolution and (optionally) a per-step
            // mutable reborrow of the host's GpuEncoder. Scoped tightly so
            // the borrow ends before the release loop's mutable borrow
            // below.
            if let Some(inst) = graph.get_node_mut(step.node) {
                let backend_ref: &dyn Backend = &*self.backend;
                let inputs = NodeInputs::new(&self.input_scratch, backend_ref);
                let outputs = NodeOutputs::new(&self.output_scratch, backend_ref);
                let mut ctx = EffectNodeContext::new(
                    time,
                    &inst.params,
                    inputs,
                    outputs,
                    gpu.as_deref_mut(),
                );
                inst.node.evaluate(&mut ctx);
            }

            // 4. Release dead resources.
            for &res_id in &step.free_after {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                self.backend.release(res_id, ty);
            }
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::with_mock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::compile;
    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };
    use crate::node_graph::EffectNode;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
        }
    }

    fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name,
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name,
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }

    /// Test EffectNode that records each evaluation's bindings into a shared log.
    struct RecordingNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        log: Arc<Mutex<Vec<EvaluationRecord>>>,
    }

    #[derive(Debug, Clone, PartialEq)]
    struct EvaluationRecord {
        type_name: String,
        inputs: Vec<(&'static str, Slot)>,
        outputs: Vec<(&'static str, Slot)>,
    }

    impl RecordingNode {
        fn new(
            name: &'static str,
            inputs: Vec<NodeInput>,
            outputs: Vec<NodeOutput>,
            log: Arc<Mutex<Vec<EvaluationRecord>>>,
        ) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                log,
            }
        }
    }

    impl EffectNode for RecordingNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let inputs: Vec<_> = ctx.inputs.iter().collect();
            let outputs: Vec<_> = ctx.outputs.iter().collect();
            self.log.lock().unwrap().push(EvaluationRecord {
                type_name: self.type_id.as_str().to_string(),
                inputs,
                outputs,
            });
        }
    }

    #[test]
    fn linear_chain_uses_only_two_slots_via_ping_pong() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let d = g.add_node(Box::new(RecordingNode::new(
            "d",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        g.connect((c, "out"), (d, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        assert_eq!(
            exec.backend().slot_count(),
            2,
            "linear chain should ping-pong between 2 physical slots"
        );

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4);
        let names: Vec<_> = log.iter().map(|r| r.type_name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn evaluate_sees_correct_input_and_output_bindings() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let log = log.lock().unwrap();
        let a_eval = &log[0];
        let b_eval = &log[1];
        let a_out_slot = a_eval.outputs[0].1;
        let b_in_slot = b_eval.inputs[0].1;
        assert_eq!(a_out_slot, b_in_slot);
    }

    #[test]
    fn diamond_uses_three_slots() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let d = g.add_node(Box::new(RecordingNode::new(
            "d",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((a, "out"), (c, "in")).unwrap();
        g.connect((b, "out"), (d, "a")).unwrap();
        g.connect((c, "out"), (d, "b")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 3);
    }

    #[test]
    fn slot_count_is_stable_across_frames() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..10 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(exec.backend().slot_count(), 1);
    }

    #[test]
    fn texture_2d_and_texture_3d_use_separate_slot_pools() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let _ = g.add_node(Box::new(RecordingNode::new(
            "mixed",
            vec![],
            vec![
                output("color", PortType::Texture2D),
                output("volume", PortType::Texture3D),
            ],
            log.clone(),
        )));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 2);
    }

    #[test]
    fn scalar_inputs_and_textures_are_pooled_separately() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let _ = g.add_node(Box::new(RecordingNode::new(
            "mix",
            vec![],
            vec![
                output("tex", PortType::Texture2D),
                output("k", PortType::Scalar(ScalarType::F32)),
            ],
            log.clone(),
        )));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 2);
    }
}
