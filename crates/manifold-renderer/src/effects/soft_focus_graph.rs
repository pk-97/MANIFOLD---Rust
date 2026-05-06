//! [`SoftFocusGraphFX`] — Soft Focus, rebuilt as a graph-backed
//! composite. Internally:
//!
//! ```text
//! Source ──▶ Blur ──▶ Mix.b
//! Source ───────────▶ Mix.a
//! Mix.out ─────────▶ FinalOutput.in
//! ```
//!
//! First graph-backed effect with branching topology (the source
//! fans to both Blur and Mix.a). Exposes two parameters routed
//! through the [`CompositeHandle`]:
//! - `radius` → `Blur.radius` (0..32 typical, capped at 32 in shader)
//! - `amount` → `Mix.amount` (0 = sharp original, 1 = full blur)

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::composites::{build_soft_focus, CompositeHandle};
use crate::node_graph::{
    compile, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend,
    NodeInstanceId, ParamValue, ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::SOFT_FOCUS_GRAPH,
        display_name: "Soft Focus (Graph)",
        category: "Post-Process",
        available: true,
        osc_prefix: "soft_focus_graph",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("Radius", 0.0, 32.0, 6.0, "F1", "px"),
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::SOFT_FOCUS_GRAPH,
        create: |device| Box::new(SoftFocusGraphFX::new(device)),
    }
}

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct SoftFocusGraphFX {
    type_id: EffectTypeId,
    /// Graph topology + execution plan — built eagerly so the editor
    /// canvas can show the graph immediately, independent of GPU.
    graph: Graph,
    plan: ExecutionPlan,
    /// CompositeHandle from `build_soft_focus`, retained so the
    /// effect's exposed `radius` / `amount` slots route to the right
    /// inner nodes each frame.
    handle: CompositeHandle,
    source_resource: ResourceId,
    output_resource: ResourceId,
    state: Option<RenderState>,
}

struct RenderState {
    executor: Executor,
    source_slot: Slot,
    output_slot: Slot,
    width: u32,
    height: u32,
}

impl SoftFocusGraphFX {
    pub fn new(_device: &GpuDevice) -> Self {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let handle = build_soft_focus(&mut graph, (src, "out"))
            .expect("build_soft_focus should never fail with a valid source");
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect(handle.output(), (final_out, "in"))
            .expect("wire Mix.out → FinalOutput.in");

        let plan = compile(&graph).expect("compile soft-focus-graph plan");

        let source_resource = output_resource(&plan, src, "out");
        let output_resource = output_resource(&plan, handle.output().0, "out");

        Self {
            type_id: EffectTypeId::SOFT_FOCUS_GRAPH,
            graph,
            plan,
            handle,
            source_resource,
            output_resource,
            state: None,
        }
    }
}

impl RenderState {
    fn build(
        device: &GpuDevice,
        width: u32,
        height: u32,
        source_resource: ResourceId,
        output_resource: ResourceId,
    ) -> Self {
        let source_target =
            RenderTarget::new(device, width, height, GRAPH_FORMAT, "soft-focus-source");
        let output_target =
            RenderTarget::new(device, width, height, GRAPH_FORMAT, "soft-focus-output");

        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        let source_slot = backend.pre_bind_texture_2d(source_resource, source_target);
        let output_slot = backend.pre_bind_texture_2d(output_resource, output_target);

        let executor = Executor::new(Box::new(backend));

        Self {
            executor,
            source_slot,
            output_slot,
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

impl PostProcessEffect for SoftFocusGraphFX {
    fn effect_type(&self) -> &EffectTypeId {
        &self.type_id
    }

    /// Skip when amount = 0 — a fully sharp original is identity. The
    /// default `param[0] <= 0` heuristic would kick in on radius = 0
    /// instead, which is also a valid "skip" but slightly less
    /// principled. Pin to the amount slot.
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.get(1).copied().unwrap_or(0.0) <= 0.0
    }

    fn graph_snapshot(&self) -> Option<crate::node_graph::GraphSnapshot> {
        Some(crate::node_graph::GraphSnapshot::from_graph(&self.graph))
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
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
                self.source_resource,
                self.output_resource,
            ));
        }
        let state = self.state.as_mut().expect("state initialized above");

        // 2. Copy the layer's source into the graph's pre-bound Source slot.
        let backend = state.executor.backend();
        let source_tex = backend
            .texture_2d(state.source_slot)
            .expect("source slot pre-bound");
        gpu.copy_texture_to_texture(source, source_tex, ctx.width, ctx.height);

        // 3. Route the effect-card sliders into the inner nodes via the
        //    CompositeHandle. Order matches the ParamSpec list above.
        let radius = fx.param_values.first().copied().unwrap_or(6.0);
        let amount = fx.param_values.get(1).copied().unwrap_or(0.5);
        self.handle
            .set_param(&mut self.graph, "radius", ParamValue::Float(radius))
            .expect("route radius");
        self.handle
            .set_param(&mut self.graph, "amount", ParamValue::Float(amount))
            .expect("route amount");

        // 4. Run the graph.
        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        state
            .executor
            .execute_frame_with_gpu(&mut self.graph, &self.plan, frame_time, gpu);

        // 5. Blit the graph's output into the layer chain's target.
        let output_tex = state
            .executor
            .backend()
            .texture_2d(state.output_slot)
            .expect("output slot pre-bound");
        gpu.copy_texture_to_texture(output_tex, target, ctx.width, ctx.height);
    }
}
