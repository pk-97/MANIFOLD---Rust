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
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::state_store::{OwnerKey, StateStore};

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
    /// Per-step scratch the executor hands to [`NodeOutputs`] so control-rate
    /// nodes can queue scalar writes. Drained back into the backend after
    /// each node's `evaluate` returns.
    scalar_write_scratch: Vec<(Slot, ParamValue)>,
    /// Persistent resources whose first acquisition has been cleared to
    /// opaque black. Subsequent frames find them in this set and skip
    /// the clear — the buffer's contents are now valid producer writes
    /// carrying state across the frame boundary.
    initialized_persistent: ahash::AHashSet<ResourceId>,
}

impl Executor {
    /// Construct an executor with the given backend.
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            input_scratch: Vec::new(),
            output_scratch: Vec::new(),
            scalar_write_scratch: Vec::new(),
            initialized_persistent: ahash::AHashSet::default(),
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
    /// GPU work (boundary nodes, stub primitives).
    ///
    /// Panics with a clean diagnostic *at entry* if the compiled plan
    /// contains any node that declares it [`requires`](crate::node_graph::EffectNode::requires)
    /// a `GpuEncoder` or a `StateStore` — that's a programmer error
    /// (wrong entry point for the graph), not a per-node `.expect()`.
    pub fn execute_frame(&mut self, graph: &mut Graph, plan: &ExecutionPlan, time: FrameTime) {
        let r = plan.requires();
        assert!(
            !r.gpu_encoder,
            "Executor::execute_frame called with a plan containing node(s) that require a GpuEncoder \
             — dispatch through `execute_frame_with_gpu` instead.",
        );
        assert!(
            !r.state_store,
            "Executor::execute_frame called with a plan containing node(s) that require a StateStore \
             — dispatch through `execute_frame_with_state` instead.",
        );
        self.execute_frame_inner(graph, plan, time, None, None, 0);
    }

    /// Run one frame of the graph with a real `GpuEncoder` available to
    /// every node. Used by the production renderer integration; pairs with
    /// [`MetalBackend`](crate::node_graph::MetalBackend) for real
    /// `GpuTexture` allocation.
    ///
    /// Panics with a clean diagnostic *at entry* if the plan contains
    /// any node that declares it requires a `StateStore` — those
    /// graphs must dispatch through `execute_frame_with_state`.
    pub fn execute_frame_with_gpu(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        gpu: &mut GpuEncoder<'_>,
    ) {
        assert!(
            !plan.requires().state_store,
            "Executor::execute_frame_with_gpu called with a plan containing node(s) that require \
             a StateStore — dispatch through `execute_frame_with_state` instead. \
             (Common cause: a chain containing `temporal::Feedback` dispatched via a code path \
             that hasn't been ported to the StateStore-aware execute method.)",
        );
        self.execute_frame_inner(graph, plan, time, Some(gpu), None, 0);
    }

    /// Run one frame of the graph with a real `GpuEncoder` plus a
    /// `StateStore` for stateful nodes (Bloom mip chains, Feedback prev-
    /// frame buffers, etc.). The `owner_key` is forwarded to every node
    /// via `EffectNodeContext::owner_key` and keys per-clip / per-layer
    /// state in the store.
    ///
    /// This entry point provides every runtime service today's nodes
    /// can declare, so there's no entry-side panic for plan-vs-services
    /// mismatch.
    pub fn execute_frame_with_state(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        gpu: &mut GpuEncoder<'_>,
        state: &mut StateStore,
        owner_key: OwnerKey,
    ) {
        self.execute_frame_inner(graph, plan, time, Some(gpu), Some(state), owner_key);
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
        mut state: Option<&mut StateStore>,
        owner_key: OwnerKey,
    ) {
        // Wipe any skip-passthrough aliases installed during the previous
        // frame. Without this, a slot that was aliased-on-skip last frame
        // would shadow its real write this frame and downstream reads
        // would still see the old upstream texture. Host-installed
        // borrows (e.g. the chain source slot's per-frame
        // `replace_texture_2d`) are untouched.
        self.backend.clear_skip_aliases();

        // Pre-acquire persistent resources before the step loop.
        // These are wires that close a per-frame feedback loop through
        // the StateStore (their consumer node declared
        // `breaks_dependency_cycle`). The consumer runs at step 0 — its
        // `slot_for(res_id)` would panic if the resource hadn't been
        // acquired yet, because the producer that writes the resource
        // runs LATER in the same frame's step order. Acquiring here is
        // idempotent on existing bindings, so the first frame allocates
        // a slot; subsequent frames find the slot already bound from
        // last frame and carry the producer's prior-frame write into
        // the consumer's read.
        //
        // On a resource's FIRST-EVER acquisition by this executor we
        // also clear the underlying texture to opaque black, so
        // first-frame consumers don't read uninitialised pixels. Only
        // applies when a `GpuEncoder` is available — mock-backend code
        // paths (used by logic tests) skip this and rely on the test
        // primitive's tolerance for the mock's zero slots.
        for &res_id in plan.persistent_resources() {
            let ty = plan
                .resource_type(res_id)
                .expect("persistent resource type known from compile()");
            let fmt = plan.resource_format(res_id);
            let slot = self.backend.acquire(res_id, ty, fmt);
            if self.initialized_persistent.insert(res_id)
                && let Some(gpu) = gpu.as_deref_mut()
                && let Some(tex) = self.backend.texture_2d(slot)
            {
                gpu.clear_texture(tex, 0.0, 0.0, 0.0, 0.0);
            }
        }

        for step in plan.steps() {
            // 1. Acquire output slots.
            self.output_scratch.clear();
            for &(port_name, res_id) in &step.outputs {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let fmt = plan.resource_format(res_id);
                let slot = self.backend.acquire(res_id, ty, fmt);
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

            // 3. Evaluate (or skip-passthrough alias). The context holds
            // an immutable backend ref for typed accessor resolution and
            // (optionally) a per-step mutable reborrow of the host's
            // GpuEncoder + StateStore. Scoped tightly so the borrows end
            // before the release loop's mutable borrow below.
            if let Some(inst) = graph.get_node_mut(step.node) {
                // Query skip-passthrough BEFORE building the full context.
                // If the node declares itself a no-op, alias the input
                // slot's texture onto the output slot — zero GPU work
                // — and skip evaluate. Matches the legacy chain
                // dispatch's "skip + don't swap" semantic without the
                // per-skip blit a naive fix would require.
                let skip_alias = inst.node.skip_passthrough(&inst.params);
                let mut performed_alias = false;
                if let Some((in_port, out_port)) = skip_alias {
                    let in_slot = self
                        .input_scratch
                        .iter()
                        .find(|(name, _)| *name == in_port)
                        .map(|(_, s)| *s);
                    let out_slot = self
                        .output_scratch
                        .iter()
                        .find(|(name, _)| *name == out_port)
                        .map(|(_, s)| *s);
                    if let (Some(i), Some(o)) = (in_slot, out_slot)
                        && self.backend.alias_2d(i, o)
                    {
                        performed_alias = true;
                    }
                }

                if !performed_alias {
                    self.scalar_write_scratch.clear();
                    {
                        let backend_ref: &dyn Backend = &*self.backend;
                        let inputs = NodeInputs::new(&self.input_scratch, backend_ref);
                        let outputs = NodeOutputs::new(
                            &self.output_scratch,
                            backend_ref,
                            &mut self.scalar_write_scratch,
                        );
                        // Canvas dims are no longer hung off the
                        // context as a side-channel. Primitives that
                        // need them (`scatter_particles` and friends)
                        // declare `width`/`height` as required scalar
                        // input ports and the JSON preset wires them
                        // from `system.generator_input.output_width /
                        // output_height` — the value is visible in the
                        // graph editor and the chain validator catches
                        // missing wires at preset-load instead of at
                        // runtime via a sub-rect render bug.
                        let mut ctx = EffectNodeContext::with_state(
                            time,
                            &inst.params,
                            inputs,
                            outputs,
                            gpu.as_deref_mut(),
                            state.as_deref_mut(),
                            step.node,
                            owner_key,
                        );
                        inst.node.evaluate(&mut ctx);
                    }
                    // Drain scalar writes back into the backend so
                    // downstream readers in the same frame see them via
                    // `NodeInputs::scalar`. Synchronous — control wires
                    // evaluate in topological order, so producers always
                    // precede consumers.
                    for (slot, value) in self.scalar_write_scratch.drain(..) {
                        self.backend.set_scalar(slot, value);
                    }
                }
            }

            // 4. Release dead resources.
            for &res_id in &step.free_after {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let fmt = plan.resource_format(res_id);
                self.backend.release(res_id, ty, fmt);
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

    use crate::node_graph::EffectNode;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
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

    // --- NodeRequires entry-point validation -----------------------

    /// Test node that declares a `state_store` requirement.
    struct NeedsStateNode {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
    }

    impl NeedsStateNode {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("needs_state"),
                outputs: vec![output("out", PortType::Texture2D)],
            }
        }
    }

    impl EffectNode for NeedsStateNode {
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
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
        fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
            crate::node_graph::effect_node::NodeRequires {
                state_store: true,
                gpu_encoder: false,
            }
        }
    }

    #[test]
    #[should_panic(expected = "require a StateStore")]
    fn execute_frame_panics_on_state_requiring_node() {
        let mut g = Graph::new();
        g.add_node(Box::new(NeedsStateNode::new()));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn plan_requires_reflects_node_declaration() {
        let mut g = Graph::new();
        g.add_node(Box::new(NeedsStateNode::new()));
        let plan = compile(&g).unwrap();
        assert!(plan.requires().state_store);
        assert!(!plan.requires().gpu_encoder);
    }

    #[test]
    fn plan_requires_default_for_stateless_graph() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        g.add_node(Box::new(RecordingNode::new(
            "stateless",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log,
        )));
        let plan = compile(&g).unwrap();
        assert!(!plan.requires().state_store);
        assert!(!plan.requires().gpu_encoder);
    }
}
