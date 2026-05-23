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
use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
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
    /// Per-frame "this step is reachable from a final output via at
    /// least one live mux branch" bitset, reused across frames to
    /// avoid per-frame allocation. Populated by [`compute_live_steps`]
    /// at the top of each frame; consumed by the step loop to skip
    /// dispatches for pruned branches. Cleared (`.fill(false)`) before
    /// each rebuild; capacity grows on demand.
    live_steps: Vec<bool>,
    /// Per-frame scratch for `selected_input_branch`'s `wired_inputs`
    /// argument. Reused across nodes; cleared before each call.
    wired_scratch: Vec<&'static str>,
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
            live_steps: Vec::new(),
            wired_scratch: Vec::new(),
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

    /// Build the per-frame live-step bitset that drives mux short-
    /// circuit. Walks `plan.steps()` in reverse: every `FinalOutput`
    /// step seeds the live set, and each live step propagates
    /// liveness backwards to its inputs' producers — with one
    /// twist for branch-selector nodes (see
    /// [`EffectNode::selected_input_branch`]). When a live step is a
    /// selector with an unwired selector port, only the chosen input
    /// port's producer is marked live; the other inputs' producers
    /// stay unmarked unless some OTHER live path also depends on
    /// them. Equivalent to "every node reachable from a FinalOutput
    /// via at least one live mux branch."
    ///
    /// Why reverse-walk works in one pass: by the time we reach a
    /// step in reverse order, every downstream step that could mark
    /// it live has already been visited. So a single pass propagates
    /// liveness without iteration to convergence.
    ///
    /// Wires that close per-frame loops through a `breaks_dependency_cycle`
    /// node ARE handled correctly: those wires appear in `step.inputs`
    /// like any other, so liveness propagates through them. The
    /// state-capture semantics (resource carries across frames) is
    /// orthogonal — handled by `persistent_resources` acquisition,
    /// not live-set propagation.
    ///
    /// `wired_scratch` is reused across nodes to avoid per-frame
    /// allocation in the inner loop.
    fn compute_live_steps(&mut self, graph: &Graph, plan: &ExecutionPlan) {
        let steps = plan.steps();
        self.live_steps.clear();
        self.live_steps.resize(steps.len(), false);

        // Build producer map: ResourceId → step index that produces
        // it. Walked once; reused for every input-port propagation.
        // Per-frame allocation is a deliberate tradeoff against
        // carrying a parallel structure on ExecutionPlan — this
        // table's size is bounded by `plan.resource_count()` which
        // is small (tens to low hundreds even for the densest
        // generators), and rebuilding it keeps the executor's
        // per-frame state self-contained.
        let mut producer: ahash::AHashMap<ResourceId, usize> =
            ahash::AHashMap::with_capacity(plan.resource_count());
        for (idx, step) in steps.iter().enumerate() {
            for &(_, res_id) in &step.outputs {
                producer.insert(res_id, idx);
            }
        }

        // Seed: every FinalOutput step is live. (Multi-FinalOutput
        // graphs are unusual but legal; this handles them uniformly.)
        for (idx, step) in steps.iter().enumerate() {
            if let Some(inst) = graph.get_node(step.node)
                && inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID
            {
                self.live_steps[idx] = true;
            }
        }

        // Reverse propagation. For each live step, mark its input
        // producers live — with the mux short-circuit applied.
        for idx in (0..steps.len()).rev() {
            if !self.live_steps[idx] {
                continue;
            }
            let step = &steps[idx];
            let Some(inst) = graph.get_node(step.node) else {
                continue;
            };

            // Resolve the optional selected-input-branch hint. The
            // node sees the list of port names that have wires
            // connected — used by mux to detect a wired selector and
            // bail out of the optimisation.
            self.wired_scratch.clear();
            for &(port_name, _) in &step.inputs {
                self.wired_scratch.push(port_name);
            }
            let selected =
                inst.node.selected_input_branch(&inst.params, &self.wired_scratch);

            for &(port_name, res_id) in &step.inputs {
                if let Some(chosen) = selected
                    && port_name != chosen
                {
                    continue;
                }
                if let Some(&prod_step) = producer.get(&res_id) {
                    self.live_steps[prod_step] = true;
                }
            }
        }

        // Graphs without any FinalOutput (test fixtures, in-flight
        // editor graphs) get NO live seeds → every step skipped →
        // executor is a no-op for that frame. That matches the
        // pre-existing behaviour of `compile` filtering to
        // FinalOutput-reachable nodes only when a FinalOutput is
        // present (see execution_plan.rs `has_final_output` branch).
        // For the no-FinalOutput fallback path we want every step
        // live, otherwise tests like
        // `value::tests::value_runs_without_final_output` would
        // regress. Detect by checking whether anything got seeded.
        if !self.live_steps.iter().any(|&b| b) {
            self.live_steps.fill(true);
        }
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
    ///
    /// Mux short-circuit: [`compute_live_steps`] runs first, marking
    /// every step reachable from a FinalOutput via at least one live
    /// mux branch. Non-live steps are skipped entirely (no acquire,
    /// no evaluate, no `free_after`). The resources they would have
    /// freed remain bound to their slots — that's correct, the
    /// backend's idempotent `acquire` will hand the same slot back
    /// next frame if the consumer becomes live again. Worst-case
    /// slot count grows to "max over all branches ever selected"
    /// rather than "max over currently-selected branches," which is
    /// the right tradeoff for live-perform mode switches.
    fn execute_frame_inner(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        mut gpu: Option<&mut GpuEncoder<'_>>,
        mut state: Option<&mut StateStore>,
        owner_key: OwnerKey,
    ) {
        self.compute_live_steps(graph, plan);

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

        for (idx, step) in plan.steps().iter().enumerate() {
            if !self.live_steps[idx] {
                // Mux short-circuit: producer subgraph of an
                // unselected branch. Skip acquire / evaluate /
                // free_after entirely — slots stay bound from last
                // frame so re-selection picks up the prior state.
                continue;
            }

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

            // 2. Look up input slots. A wired input whose producer
            // step was pruned (mux short-circuit) has no slot bound
            // this frame — drop it from the input scratch so the
            // node's `NodeInputs` accessor returns `None`. Mux
            // primitives tolerate this via their port-shadows-param
            // fallback (selector resolves to a port whose `in_N` IS
            // bound); other nodes wouldn't legitimately end up with
            // a pruned input because the live-set walk only prunes
            // mux branches (the unselected `in_K`s on the mux's own
            // input list).
            self.input_scratch.clear();
            for &(port_name, res_id) in &step.inputs {
                if let Some(slot) = self.backend.slot_for(res_id) {
                    self.input_scratch.push((port_name, slot));
                }
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
        /// Optional branch-selector hint — when set, the node returns
        /// it from `selected_input_branch` so the executor's live-set
        /// walk treats only that input port as live. Interior-mutable
        /// (Arc<Mutex<…>>) so frame-to-frame selector-flip tests can
        /// mutate the hint without going through `get_node_mut` and
        /// downcast gymnastics — mirrors the production path where
        /// `mux_texture`'s selected_input_branch reads from
        /// `inst.params` (which IS mutable through the graph's
        /// `set_param`, but RecordingNode doesn't have params so we
        /// model the same write-then-rebuild behaviour via a shared
        /// handle the test holds onto).
        selected_branch: Arc<Mutex<Option<&'static str>>>,
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
                selected_branch: Arc::new(Mutex::new(None)),
            }
        }

        /// Make this node act as a branch-selector for executor
        /// live-set tests. Returns the shared `Arc<Mutex<Option<&str>>>`
        /// handle so the test can later flip the selection between
        /// frames to exercise the per-frame live-set rebuild.
        fn with_selected_branch(
            mut self,
            port: Option<&'static str>,
        ) -> (Self, Arc<Mutex<Option<&'static str>>>) {
            let handle = Arc::new(Mutex::new(port));
            self.selected_branch = handle.clone();
            (self, handle)
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
        fn selected_input_branch(
            &self,
            _params: &crate::node_graph::effect_node::ParamValues,
            _wired_inputs: &[&str],
        ) -> Option<&'static str> {
            *self.selected_branch.lock().unwrap()
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

    // --- Mux short-circuit / live-set propagation ------------------
    //
    // Switch-statement semantics for `EffectNode::selected_input_branch`:
    // only the chosen branch's producer chain evaluates each frame.
    // These tests use FinalOutput as the live-set seed (the real
    // production trigger) and a `selected_branch`-configured
    // RecordingNode as a stand-in for `node.mux_texture`, so the
    // tests stay isolated from the mux's WGSL dispatch path. The
    // mux's own selector → port-name resolution is covered in
    // primitives/mux_texture.rs.

    use crate::node_graph::FinalOutput;

    /// Build `[prod_A → mux.in_0, prod_B → mux.in_1, prod_C → mux.in_2]
    /// → FinalOutput`, mark mux as selecting `selected`, and return
    /// the graph plus the shared selector handle (for tests that
    /// flip the selection between frames) and the evaluation log.
    fn build_three_branch_mux_graph(
        selected: Option<&'static str>,
    ) -> (
        Graph,
        Arc<Mutex<Option<&'static str>>>,
        Arc<Mutex<Vec<EvaluationRecord>>>,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();

        let prod_a = g.add_node(Box::new(RecordingNode::new(
            "prod_a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let prod_b = g.add_node(Box::new(RecordingNode::new(
            "prod_b",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let prod_c = g.add_node(Box::new(RecordingNode::new(
            "prod_c",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let (mux_node, selector_handle) = RecordingNode::new(
            "mux",
            vec![
                input("in_0", PortType::Texture2D, false),
                input("in_1", PortType::Texture2D, false),
                input("in_2", PortType::Texture2D, false),
            ],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )
        .with_selected_branch(selected);
        let mux = g.add_node(Box::new(mux_node));
        let fout = g.add_node(Box::new(FinalOutput::new()));

        g.connect((prod_a, "out"), (mux, "in_0")).unwrap();
        g.connect((prod_b, "out"), (mux, "in_1")).unwrap();
        g.connect((prod_c, "out"), (mux, "in_2")).unwrap();
        g.connect((mux, "out"), (fout, "in")).unwrap();

        (g, selector_handle, log)
    }

    #[test]
    fn selected_branch_prunes_unselected_producers() {
        let (mut g, _sel, log) = build_three_branch_mux_graph(Some("in_1"));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        assert!(
            names.contains(&"prod_b".to_string()),
            "selected branch's producer must run, got: {names:?}",
        );
        assert!(
            names.contains(&"mux".to_string()),
            "mux itself must run, got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_a".to_string()),
            "unselected branch (in_0) must NOT run, got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_c".to_string()),
            "unselected branch (in_2) must NOT run, got: {names:?}",
        );
    }

    #[test]
    fn selected_branch_none_keeps_all_producers_live() {
        // `selected_branch: None` mirrors the production fallback —
        // mux returns None from `selected_input_branch` (e.g. when
        // its selector port is wired to a runtime-computed value).
        // Every input's producer must run since we can't predict
        // which one the selector will resolve to.
        let (mut g, _sel, log) = build_three_branch_mux_graph(None);
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        for required in ["prod_a", "prod_b", "prod_c", "mux"] {
            assert!(
                names.contains(&required.to_string()),
                "fallback path must run every branch; missing `{required}` in {names:?}",
            );
        }
    }

    #[test]
    fn switching_selected_branch_across_frames_flips_live_set() {
        // Wire perform-mode flow: a mux's selector slides between
        // values across frames. Each frame's live set must reflect
        // THAT frame's selection — the previous frame's selection
        // shouldn't leak into the next.
        //
        // We mutate the shared selector handle directly (interior
        // mutability via Arc<Mutex>). In production the equivalent
        // path is `set_param` writing into `inst.params`, which the
        // mux's `selected_input_branch` reads on the next frame's
        // live-set rebuild.
        let (mut g, selector, log) = build_three_branch_mux_graph(Some("in_0"));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        // Frame 0: in_0 selected → prod_a runs.
        exec.execute_frame(&mut g, &plan, frame_time());

        // Flip the selection and drain frame 0's log so frame 1's
        // assertions only see frame 1's evaluations.
        *selector.lock().unwrap() = Some("in_2");
        log.lock().unwrap().clear();

        // Frame 1: in_2 selected → prod_c runs, prod_a no longer.
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        assert!(
            names.contains(&"prod_c".to_string()),
            "frame 1 should run the newly-selected branch (prod_c), got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_a".to_string()),
            "frame 1 should NOT run the previously-selected branch (prod_a) — \
             live set must be rebuilt per frame, got: {names:?}",
        );
    }

    #[test]
    fn unselected_branch_resources_dont_grow_slot_count_per_frame() {
        // Verifies the comment in `execute_frame_inner`: skipping
        // free_after on non-live steps doesn't leak slots within a
        // single frame. Slot count after a frame with one selected
        // branch is strictly less than the count with all branches
        // live — confirms the optimization actually reduces work.
        let (mut g_all, _sel_all, _log_all) = build_three_branch_mux_graph(None);
        let plan_all = compile(&g_all).unwrap();
        let mut exec_all = Executor::with_mock();
        exec_all.execute_frame(&mut g_all, &plan_all, frame_time());
        let slots_all = exec_all.backend().slot_count();

        let (mut g_one, _sel_one, _log_one) = build_three_branch_mux_graph(Some("in_1"));
        let plan_one = compile(&g_one).unwrap();
        let mut exec_one = Executor::with_mock();
        exec_one.execute_frame(&mut g_one, &plan_one, frame_time());
        let slots_one = exec_one.backend().slot_count();

        assert!(
            slots_one < slots_all,
            "single-branch selection must allocate fewer slots than full eager evaluation; \
             eager={slots_all}, pruned={slots_one}",
        );
    }
}
