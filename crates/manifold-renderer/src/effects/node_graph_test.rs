//! [`NodeGraphTestFX`] — first effect in the live renderer that runs
//! through the node-graph runtime.
//!
//! Hardcoded internally as `Source × 2 → Mix → FinalOutput` with red and
//! blue input textures pre-bound to the two `Source` nodes. The single
//! `Amount` parameter drives `Mix.amount`, crossfading between the two
//! constants. Useful only as a proof of life; once the editor lands, the
//! same architecture will host arbitrary user-built graphs.
//!
//! The internal graph is sized to match the layer's render resolution
//! on first apply (and rebuilt if the resolution changes), so the final
//! `copy_texture_to_texture` into `target` is a simple full-extent
//! blit.

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    compile, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
    ParamValue, ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::NODE_GRAPH_TEST,
        display_name: "Node Graph Test",
        category: "Post-Process",
        available: true,
        osc_prefix: "node_graph_test",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::NODE_GRAPH_TEST,
        create: |device| Box::new(NodeGraphTestFX::new(device)),
    }
}

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct NodeGraphTestFX {
    type_id: EffectTypeId,
    /// Graph topology + compiled execution plan. Built once at
    /// construction — independent of GPU and resolution — so the
    /// editor canvas can show the graph as soon as a project loads,
    /// without waiting for the clip to play.
    graph: Graph,
    plan: ExecutionPlan,
    mix_node_id: NodeInstanceId,
    /// Resource ids captured at compile time so the lazy GPU-resource
    /// path can re-pre-bind on resolution change without re-walking
    /// the plan.
    r_a: ResourceId,
    r_b: ResourceId,
    r_mix_out: ResourceId,
    /// GPU resources — built on first `apply` once the layer render
    /// resolution is known, rebuilt if the resolution changes.
    state: Option<RenderState>,
}

struct RenderState {
    executor: Executor,
    /// Slot Mix's output is pinned to. Stable across frames.
    mix_output_slot: Slot,
    /// Slots of the pre-bound red/blue inputs. Used once on first
    /// dispatch to clear them to constant colors.
    red_slot: Slot,
    blue_slot: Slot,
    inputs_initialized: bool,
    width: u32,
    height: u32,
}

impl NodeGraphTestFX {
    /// Factory entry. Builds the graph topology eagerly so the editor
    /// can display it immediately; GPU resource allocation defers to
    /// the first `apply` when the layer's render resolution is known.
    pub fn new(_device: &GpuDevice) -> Self {
        let mut graph = Graph::new();
        let src_a = graph.add_node(Box::new(Source::new()));
        let src_b = graph.add_node(Box::new(Source::new()));
        let mix_node_id = graph.add_node(Box::new(Mix::new()));
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .set_param(mix_node_id, "amount", ParamValue::Float(0.5))
            .expect("set Mix.amount default");
        graph
            .connect((src_a, "out"), (mix_node_id, "a"))
            .expect("wire src_a → Mix.a");
        graph
            .connect((src_b, "out"), (mix_node_id, "b"))
            .expect("wire src_b → Mix.b");
        graph
            .connect((mix_node_id, "out"), (final_out, "in"))
            .expect("wire Mix.out → FinalOutput.in");
        let plan = compile(&graph).expect("compile node-graph-test plan");
        let r_a = output_resource(&plan, src_a, "out");
        let r_b = output_resource(&plan, src_b, "out");
        let r_mix_out = output_resource(&plan, mix_node_id, "out");
        Self {
            type_id: EffectTypeId::NODE_GRAPH_TEST,
            graph,
            plan,
            mix_node_id,
            r_a,
            r_b,
            r_mix_out,
            state: None,
        }
    }
}

impl RenderState {
    fn build(
        device: &GpuDevice,
        width: u32,
        height: u32,
        r_a: ResourceId,
        r_b: ResourceId,
        r_mix_out: ResourceId,
    ) -> Self {
        // Allocate the host-managed input + output RenderTargets.
        let red_target = RenderTarget::new(device, width, height, GRAPH_FORMAT, "ng-test-red");
        let blue_target = RenderTarget::new(device, width, height, GRAPH_FORMAT, "ng-test-blue");
        let mix_output_target =
            RenderTarget::new(device, width, height, GRAPH_FORMAT, "ng-test-mix-out");

        // Pre-bind every Texture2D resource so the lazy-alloc path is
        // never reached (the backend's `without_device` mode requires
        // this).
        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        let red_slot = backend.pre_bind_texture_2d(r_a, red_target);
        let blue_slot = backend.pre_bind_texture_2d(r_b, blue_target);
        let mix_output_slot = backend.pre_bind_texture_2d(r_mix_out, mix_output_target);

        let executor = Executor::new(Box::new(backend));

        Self {
            executor,
            mix_output_slot,
            red_slot,
            blue_slot,
            inputs_initialized: false,
            width,
            height,
        }
    }
}

fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?}");
}

impl PostProcessEffect for NodeGraphTestFX {
    fn effect_type(&self) -> &EffectTypeId {
        &self.type_id
    }

    /// Always run. `Amount = 0` is a valid state (pure red), not a skip
    /// signal; the default `param[0] <= 0` heuristic would suppress it.
    fn should_skip(&self, _fx: &EffectInstance) -> bool {
        false
    }

    /// Snapshot the graph topology for the editor canvas. Available
    /// immediately after construction — the graph isn't tied to GPU or
    /// resolution, so the editor can render it before the first frame.
    fn graph_snapshot(&self) -> Option<crate::node_graph::GraphSnapshot> {
        Some(crate::node_graph::GraphSnapshot::from_graph(&self.graph))
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        _source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // 1. Lazy-init or rebuild on resolution change.
        let needs_build = match &self.state {
            None => true,
            Some(s) => s.width != ctx.width || s.height != ctx.height,
        };
        if needs_build {
            self.state = Some(RenderState::build(
                gpu.device,
                ctx.width,
                ctx.height,
                self.r_a,
                self.r_b,
                self.r_mix_out,
            ));
        }
        let state = self.state.as_mut().expect("state initialized above");

        // 2. One-time per-resolution: clear red/blue inputs to their
        //    constant colors. Needs an encoder, so it can't happen in
        //    `RenderState::build`.
        if !state.inputs_initialized {
            let backend = state.executor.backend();
            let red_tex = backend
                .texture_2d(state.red_slot)
                .expect("red input texture pre-bound");
            let blue_tex = backend
                .texture_2d(state.blue_slot)
                .expect("blue input texture pre-bound");
            gpu.clear_texture(red_tex, 1.0, 0.0, 0.0, 1.0);
            gpu.clear_texture(blue_tex, 0.0, 0.0, 1.0, 1.0);
            state.inputs_initialized = true;
        }

        // 3. Wire the slider into Mix's `amount` parameter.
        let amount = fx.param_values.first().copied().unwrap_or(0.5);
        self.graph
            .set_param(self.mix_node_id, "amount", ParamValue::Float(amount))
            .expect("set Mix.amount each frame");

        // 4. Run the graph. Mix writes into the pre-bound mix-output
        //    RenderTarget at `mix_output_slot`.
        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        state
            .executor
            .execute_frame_with_gpu(&mut self.graph, &self.plan, frame_time, gpu);

        // 5. Blit Mix's output into the layer chain's `target` so the
        //    rest of the chain sees our result.
        let mix_tex = state
            .executor
            .backend()
            .texture_2d(state.mix_output_slot)
            .expect("mix output texture must be pre-bound");
        gpu.copy_texture_to_texture(mix_tex, target, ctx.width, ctx.height);
    }
}
