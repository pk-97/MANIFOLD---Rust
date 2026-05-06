//! [`MirrorGraphFX`] — Mirror, rebuilt as a graph-backed composite.
//!
//! Internally `Source → UVTransform[mode=Mirror] → FinalOutput`. The
//! `Source` boundary node is bound at apply time to the layer chain's
//! input texture, so the graph effectively maps every output pixel
//! `(u, v)` to `source(1-u, v)`.
//!
//! Coexists with the legacy [`crate::effects::mirror::MirrorEffect`]
//! per `docs/NODE_GRAPH_SYSTEM.md` §11 — same visual output, different
//! implementation. Drag "Mirror (Graph)" onto a clip, click its cog
//! to inspect the topology in the editor canvas.

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::composites::build_mirror;
use crate::node_graph::{
    compile, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend,
    NodeInstanceId, ResourceId, Slot, Source,
};
use crate::render_target::RenderTarget;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::MIRROR_GRAPH,
        display_name: "Mirror (Graph)",
        category: "Post-Process",
        available: true,
        osc_prefix: "mirror_graph",
        legacy_discriminant: None,
        // No tunable params for V1 — Mirror is a fixed alias preset of
        // UVTransform[mode=Mirror]. Future versions can expose
        // translate/scale/rotation through here.
        params: &[],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::MIRROR_GRAPH,
        create: |device| Box::new(MirrorGraphFX::new(device)),
    }
}

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct MirrorGraphFX {
    type_id: EffectTypeId,
    /// Graph topology + execution plan — built eagerly so the editor
    /// canvas can show the graph as soon as the effect is constructed,
    /// independent of GPU/resolution.
    graph: Graph,
    plan: ExecutionPlan,
    /// `Source.out` — the slot we rebind to the layer's source texture
    /// each frame.
    source_resource: ResourceId,
    /// `UVTransform.out` — the slot the result lands in.
    output_resource: ResourceId,
    /// GPU resources — built lazily on first `apply` once the layer's
    /// render resolution is known.
    state: Option<RenderState>,
}

struct RenderState {
    executor: Executor,
    /// Slot the `Source.out` resource is pre-bound to. The layer's
    /// source texture is copied into this slot's RenderTarget each
    /// frame before execution.
    source_slot: Slot,
    /// Slot the `UVTransform.out` resource is pre-bound to. The graph
    /// writes into this; we then blit it to the layer chain's `target`.
    output_slot: Slot,
    width: u32,
    height: u32,
}

impl MirrorGraphFX {
    pub fn new(_device: &GpuDevice) -> Self {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let mirror_handle = build_mirror(&mut graph, (src, "out"))
            .expect("build_mirror should never fail with a valid source");
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect(mirror_handle.output(), (final_out, "in"))
            .expect("wire UVTransform.out → FinalOutput.in");

        let plan = compile(&graph).expect("compile mirror-graph plan");

        let source_resource = output_resource(&plan, src, "out");
        let output_resource = output_resource(&plan, mirror_handle.output().0, "out");

        Self {
            type_id: EffectTypeId::MIRROR_GRAPH,
            graph,
            plan,
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
            RenderTarget::new(device, width, height, GRAPH_FORMAT, "mirror-graph-source");
        let output_target =
            RenderTarget::new(device, width, height, GRAPH_FORMAT, "mirror-graph-output");

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

impl PostProcessEffect for MirrorGraphFX {
    fn effect_type(&self) -> &EffectTypeId {
        &self.type_id
    }

    /// Always run — no skip param. Mirror is meaningful at every
    /// parameter setting (and there are no parameters in V1).
    fn should_skip(&self, _fx: &EffectInstance) -> bool {
        false
    }

    /// Editor canvas snapshot — available immediately after
    /// construction, regardless of whether the effect has rendered yet.
    fn graph_snapshot(&self) -> Option<crate::node_graph::GraphSnapshot> {
        Some(crate::node_graph::GraphSnapshot::from_graph(&self.graph))
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        _fx: &EffectInstance,
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

        // 2. Copy the layer chain's source texture into the graph's
        //    pre-bound Source slot. The graph reads from there.
        let backend = state.executor.backend();
        let source_tex = backend
            .texture_2d(state.source_slot)
            .expect("source slot pre-bound");
        gpu.copy_texture_to_texture(source, source_tex, ctx.width, ctx.height);

        // 3. Run the graph.
        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        state
            .executor
            .execute_frame_with_gpu(&mut self.graph, &self.plan, frame_time, gpu);

        // 4. Blit the graph's output into the layer chain's target.
        let output_tex = state
            .executor
            .backend()
            .texture_2d(state.output_slot)
            .expect("output slot pre-bound");
        gpu.copy_texture_to_texture(output_tex, target, ctx.width, ctx.height);
    }
}
