//! [`MirrorFX`] — Mirror, graph-backed.
//!
//! Internally:
//!
//! ```text
//! Source ──▶ UVTransform[mode=Foldᴹ] ──▶ Mix.b
//! Source ───────────────────────────────▶ Mix.a
//! Mix.out ─────────────────────────────▶ FinalOutput.in
//! ```
//!
//! Migrated from the legacy compute-shader `MirrorFX` in this same file
//! per `docs/NODE_GRAPH_SYSTEM.md` §11 Phase A — visual output preserved
//! pixel-for-pixel, implementation now lives in primitives. The
//! `legacy_discriminant: Some(21)` carries forward so saved projects
//! that loaded the old Mirror under that id keep loading.
//!
//! Exposes:
//! - `Amount` (param 0) → `Mix.amount` (0 = original, 1 = full fold)
//! - `Mode`   (param 1) → `UVTransform.mode` after legacy→fold remap
//!   (0=Horiz=FoldX, 1=Vert=FoldY, 2=Both=FoldBoth)

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::composites::{build_mirror, legacy_mirror_mode_to_uv, CompositeHandle};
use crate::node_graph::{
    compile, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend,
    NodeInstanceId, ParamValue, PortType, ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::MIRROR,
        display_name: "Mirror",
        category: "Post-Process",
        available: true,
        osc_prefix: "mirror",
        legacy_discriminant: Some(21),
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::whole_labels("Mode", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Mode"),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::MIRROR,
        create: |device| Box::new(MirrorFX::new(device)),
    }
}

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct MirrorFX {
    type_id: EffectTypeId,
    graph: Graph,
    plan: ExecutionPlan,
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

impl MirrorFX {
    pub fn new(_device: &GpuDevice) -> Self {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let handle = build_mirror(&mut graph, (src, "out"))
            .expect("build_mirror should never fail with a valid source");
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect(handle.output(), (final_out, "in"))
            .expect("wire Mix.out → FinalOutput.in");

        let plan = compile(&graph).expect("compile mirror plan");

        let source_resource = output_resource(&plan, src, "out");
        let output_resource = output_resource(&plan, handle.output().0, "out");

        Self {
            type_id: EffectTypeId::MIRROR,
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
        plan: &ExecutionPlan,
        source_resource: ResourceId,
        output_resource: ResourceId,
    ) -> Self {
        let mut backend = MetalBackend::without_device(width, height, GRAPH_FORMAT);
        let mut source_slot: Option<Slot> = None;
        let mut output_slot: Option<Slot> = None;

        // Pre-bind every Texture2D resource — `MetalBackend::without_device`
        // panics on lazy-alloc. Mirror's intermediate is `UVTransform.out`
        // (between UVTransform and Mix), so we cannot just hand-pick
        // source + output.
        for i in 0..plan.resource_count() {
            let id = ResourceId(i as u32);
            if !matches!(plan.resource_type(id), Some(PortType::Texture2D)) {
                continue;
            }
            let (label, is_source, is_output) = if id == source_resource {
                ("mirror-source", true, false)
            } else if id == output_resource {
                ("mirror-output", false, true)
            } else {
                ("mirror-intermediate", false, false)
            };
            let target = RenderTarget::new(device, width, height, GRAPH_FORMAT, label);
            let slot = backend.pre_bind_texture_2d(id, target);
            if is_source {
                source_slot = Some(slot);
            } else if is_output {
                output_slot = Some(slot);
            }
        }

        let executor = Executor::new(Box::new(backend));

        Self {
            executor,
            source_slot: source_slot.expect("source_resource must be a Texture2D in the plan"),
            output_slot: output_slot.expect("output_resource must be a Texture2D in the plan"),
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

impl PostProcessEffect for MirrorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &self.type_id
    }

    /// Skip when amount = 0 — a fully-original output is identity.
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(1.0) <= 0.0
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
        let needs_build = match &self.state {
            None => true,
            Some(s) => s.width != ctx.width || s.height != ctx.height,
        };
        if needs_build {
            self.state = Some(RenderState::build(
                gpu.device,
                ctx.width,
                ctx.height,
                &self.plan,
                self.source_resource,
                self.output_resource,
            ));
        }
        let state = self.state.as_mut().expect("state initialized above");

        let backend = state.executor.backend();
        let source_tex = backend
            .texture_2d(state.source_slot)
            .expect("source slot pre-bound");
        gpu.copy_texture_to_texture(source, source_tex, ctx.width, ctx.height);

        // Param order matches the ParamSpec list above.
        let amount = fx.param_values.first().copied().unwrap_or(1.0);
        let mode_legacy = fx.param_values.get(1).copied().unwrap_or(0.0).round() as u32;
        let mode_uv = legacy_mirror_mode_to_uv(mode_legacy);
        self.handle
            .set_param(&mut self.graph, "amount", ParamValue::Float(amount))
            .expect("route amount");
        self.handle
            .set_param(&mut self.graph, "mode", ParamValue::Enum(mode_uv))
            .expect("route mode");

        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        state
            .executor
            .execute_frame_with_gpu(&mut self.graph, &self.plan, frame_time, gpu);

        let output_tex = state
            .executor
            .backend()
            .texture_2d(state.output_slot)
            .expect("output slot pre-bound");
        gpu.copy_texture_to_texture(output_tex, target, ctx.width, ctx.height);
    }
}
