//! Per-frame graph execution.
//!
//! The [`Executor`] takes a [`Graph`] plus a precompiled [`ExecutionPlan`]
//! and runs one frame. It owns a [`ResourcePool`] that maps abstract
//! [`ResourceId`]s onto physical [`Slot`]s, recycling slots whose previous
//! occupants have been freed.
//!
//! ## Mock vs real GPU
//!
//! Step 4 (this commit) keeps slots fully abstract — they're just `u32`
//! indices. No real GPU resources are allocated, no shader dispatches happen.
//! The executor's logic — order of acquire, evaluate, release; correctness
//! of resource reuse; correct bindings handed to nodes — is testable
//! without Metal.
//!
//! Step 5 will add a `Backend` layer that maps slots to actual `GpuTexture`s
//! / scalar values, plus a `GpuEncoder` reference inside `EffectNodeContext`.

use ahash::AHashMap;

use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime};
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::ports::PortType;

/// Maps [`ResourceId`]s onto physical [`Slot`]s. Slots are recycled when
/// their previous occupant is released and a later acquire of the same
/// [`PortType`] arrives.
///
/// Slots are pooled per-`PortType`; a `Texture2D` slot is never reused for a
/// `Texture3D` (different physical buffer kind on the real GPU).
pub struct ResourcePool {
    free_by_type: AHashMap<PortType, Vec<Slot>>,
    bound: AHashMap<ResourceId, Slot>,
    next_slot: u32,
}

impl ResourcePool {
    pub fn new() -> Self {
        Self {
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
        }
    }

    /// Acquire a slot for `id`. Reuses a free slot of matching `PortType`
    /// when one is available, else allocates a fresh slot.
    pub fn acquire(&mut self, id: ResourceId, ty: PortType) -> Slot {
        let pool = self.free_by_type.entry(ty).or_default();
        let slot = pool.pop().unwrap_or_else(|| {
            let s = Slot(self.next_slot);
            self.next_slot += 1;
            s
        });
        self.bound.insert(id, slot);
        slot
    }

    /// Return `id`'s slot to the free pool of its `PortType`.
    /// Idempotent: releasing an already-released id is a no-op.
    pub fn release(&mut self, id: ResourceId, ty: PortType) {
        if let Some(slot) = self.bound.remove(&id) {
            self.free_by_type.entry(ty).or_default().push(slot);
        }
    }

    /// Slot currently bound to `id`, or `None` if not bound.
    pub fn slot_for(&self, id: ResourceId) -> Option<Slot> {
        self.bound.get(&id).copied()
    }

    /// High-water mark — total physical slots ever allocated. Useful for
    /// asserting that resource recycling actually happens.
    pub fn slot_count(&self) -> u32 {
        self.next_slot
    }

    /// Drop all bindings and free pools. Slot count (high-water mark) is
    /// retained — the next `acquire` after `clear` allocates fresh slots
    /// from `next_slot` rather than reusing across the boundary.
    pub fn clear(&mut self) {
        self.bound.clear();
        self.free_by_type.clear();
    }
}

impl Default for ResourcePool {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs a graph against a precompiled plan, one frame per call.
///
/// The executor owns the [`ResourcePool`] across frames so the high-water
/// mark stabilises after the first frame: slots allocated for frame 0's
/// peak intermediates are reused for every subsequent frame at the same
/// graph topology.
pub struct Executor {
    pool: ResourcePool,
    /// Scratch buffer reused across steps to avoid per-step allocation.
    /// (Per-frame allocation in tight loops is forbidden by CLAUDE.md.)
    input_scratch: Vec<(&'static str, Slot)>,
    output_scratch: Vec<(&'static str, Slot)>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            pool: ResourcePool::new(),
            input_scratch: Vec::new(),
            output_scratch: Vec::new(),
        }
    }

    pub fn pool(&self) -> &ResourcePool {
        &self.pool
    }

    /// Run one frame of the graph.
    ///
    /// For each step in plan order:
    ///   1. Acquire a slot for every output port (so distinct slots from inputs).
    ///   2. Look up slots for every wired input port.
    ///   3. Call `EffectNode::evaluate` with the assembled context.
    ///   4. Release slots for resources whose last reader is this step.
    ///
    /// The acquire-then-release order is correct because evaluate writes to
    /// outputs while reading from inputs; freeing inputs before allocating
    /// outputs would let the new acquire reuse the still-being-read slot.
    pub fn execute_frame(&mut self, graph: &mut Graph, plan: &ExecutionPlan, time: FrameTime) {
        for step in plan.steps() {
            // 1. Acquire output slots.
            self.output_scratch.clear();
            for &(port_name, res_id) in &step.outputs {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let slot = self.pool.acquire(res_id, ty);
                self.output_scratch.push((port_name, slot));
            }

            // 2. Look up input slots.
            self.input_scratch.clear();
            for &(port_name, res_id) in &step.inputs {
                let slot = self
                    .pool
                    .slot_for(res_id)
                    .expect("input resource was acquired in a prior step");
                self.input_scratch.push((port_name, slot));
            }

            // 3. Evaluate.
            if let Some(inst) = graph.get_node_mut(step.node) {
                let inputs = NodeInputs::new(&self.input_scratch);
                let outputs = NodeOutputs::new(&self.output_scratch);
                let mut ctx = EffectNodeContext::new(time, &inst.params, inputs, outputs);
                inst.node.evaluate(&mut ctx);
            }

            // 4. Release dead resources.
            for &res_id in &step.free_after {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                self.pool.release(res_id, ty);
            }
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
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
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, ScalarType};
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

    /// Test EffectNode that records each evaluation's bindings into a
    /// shared log. Type id is a unique short string per node so the log
    /// reads naturally in assertions.
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
        fn evaluate(&mut self, ctx: &mut EffectNodeContext) {
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
    fn pool_reuses_freed_slot_of_matching_type() {
        let mut pool = ResourcePool::new();
        let s0 = pool.acquire(ResourceId(0), PortType::Texture2D);
        pool.release(ResourceId(0), PortType::Texture2D);
        let s1 = pool.acquire(ResourceId(1), PortType::Texture2D);
        // The released slot is reused.
        assert_eq!(s0, s1);
        assert_eq!(pool.slot_count(), 1);
    }

    #[test]
    fn pool_does_not_cross_type_boundaries() {
        let mut pool = ResourcePool::new();
        pool.acquire(ResourceId(0), PortType::Texture2D);
        pool.release(ResourceId(0), PortType::Texture2D);
        // Different type — must allocate a fresh slot.
        let s = pool.acquire(ResourceId(1), PortType::Texture3D);
        assert_eq!(s.0, 1);
        assert_eq!(pool.slot_count(), 2);
    }

    #[test]
    fn linear_chain_uses_only_two_slots_via_ping_pong() {
        // A → B → C → D — texture pool should ping-pong between 2 slots.
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
        let mut exec = Executor::new();
        exec.execute_frame(&mut g, &plan, frame_time());

        assert_eq!(
            exec.pool().slot_count(),
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
        // A → B. B's input "in" should reference A's output slot.
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
        let mut exec = Executor::new();
        exec.execute_frame(&mut g, &plan, frame_time());

        let log = log.lock().unwrap();
        let a_eval = &log[0];
        let b_eval = &log[1];
        // A's "out" slot equals B's "in" slot — they refer to the same physical buffer.
        let a_out_slot = a_eval.outputs[0].1;
        let b_in_slot = b_eval.inputs[0].1;
        assert_eq!(a_out_slot, b_in_slot);
    }

    #[test]
    fn diamond_uses_three_slots() {
        // a → b, a → c, (b, c) → d. Three live resources at the diamond's
        // peak (b reading, c needing a's output, c's output not yet free).
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
        let mut exec = Executor::new();
        exec.execute_frame(&mut g, &plan, frame_time());

        assert_eq!(exec.pool().slot_count(), 3);
    }

    #[test]
    fn slot_count_is_stable_across_frames() {
        // Run the same graph 10 frames; slot count should not grow.
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
        let mut exec = Executor::new();
        for _ in 0..10 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(exec.pool().slot_count(), 1);
    }

    #[test]
    fn texture_2d_and_texture_3d_use_separate_slot_pools() {
        // Two unread outputs of different PortTypes must occupy distinct slots.
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
        let mut exec = Executor::new();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.pool().slot_count(), 2);
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
        let mut exec = Executor::new();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.pool().slot_count(), 2);
    }
}
