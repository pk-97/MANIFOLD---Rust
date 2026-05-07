//! [`StylizedFeedbackFX`] — graph-backed Stylized Feedback.
//!
//! Internally `Source → Feedback → FinalOutput`. Per-owner prev-frame
//! state lives in a runtime-owned `StateStore` keyed by
//! `(NodeInstanceId, OwnerKey)`, not in the effect itself.
//!
//! First migration of a stateful effect under the unification arc per
//! `docs/EFFECT_RUNTIME_UNIFICATION.md`. Validates that the StateStore
//! API + `EffectNodeContext` plumbing actually supports a real
//! cross-frame stateful primitive end-to-end with pixel-exact output
//! parity vs. the legacy compute-shader path.
//!
//! Behavior parity guarantees:
//! - Same `EffectTypeId::STYLIZED_FEEDBACK` (legacy_discriminant 20).
//! - Same 4 params in the same order: Amount / Zoom / Rotate / Mode.
//! - Same shader (renamed from `fx_stylized_feedback_compute.wgsl` to
//!   `primitives/shaders/feedback.wgsl`); same uniform shape, same
//!   blend math, same NaN guard.

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Feedback;
use crate::node_graph::{
    apply_param_bindings, compile, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph,
    MetalBackend, NodeInstanceId, ParamBinding, ParamConvert, ParamTarget, PortType, ResourceId,
    Slot, Source, StateStore,
};
use crate::render_target::RenderTarget;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::STYLIZED_FEEDBACK,
        display_name: "Stylized Feedback",
        category: "Post-Process",
        available: true,
        osc_prefix: "stylizedFeedback",
        legacy_discriminant: Some(20),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::continuous("zoom", "Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
            ParamSpec::continuous("rotate", "Rotate", -10.0, 10.0, 0.0, "F2", "Rotate"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Screen", "Add", "Max"], "Mode"),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::STYLIZED_FEEDBACK,
        create: |device| Box::new(StylizedFeedbackFX::new(device)),
    }
}

const GRAPH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct StylizedFeedbackFX {
    type_id: EffectTypeId,
    graph: Graph,
    plan: ExecutionPlan,
    /// Step 17: declarative routing for the host-visible params.
    /// Each entry's `ParamTarget::Node` carries the `feedback`
    /// node id captured at construction.
    bindings: Vec<ParamBinding>,
    source_resource: ResourceId,
    output_resource: ResourceId,
    /// Per-owner persistent state lives here. Each entry's key is
    /// `(feedback_node_id, owner_key)`.
    state_store: StateStore,
    /// GPU resources — built lazily on first `apply` and rebuilt on
    /// resolution change.
    render: Option<RenderState>,
}

struct RenderState {
    executor: Executor,
    source_slot: Slot,
    output_slot: Slot,
    width: u32,
    height: u32,
}

impl StylizedFeedbackFX {
    pub fn new(_device: &GpuDevice) -> Self {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let feedback = graph.add_node(Box::new(Feedback::new()));
        graph
            .connect((src, "out"), (feedback, "source"))
            .expect("wire Source.out → Feedback.source");
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        graph
            .connect((feedback, "out"), (final_out, "in"))
            .expect("wire Feedback.out → FinalOutput.in");

        let plan = compile(&graph).expect("compile stylized-feedback plan");
        let source_resource = output_resource(&plan, src, "out");
        let output_resource = output_resource(&plan, feedback, "out");

        // Bindings target the inner `Feedback` primitive directly via
        // `ParamTarget::Node`. The host-visible id `rotate` routes to
        // the inner node's compile-time param `rotation`. Mode uses an
        // `EnumRemap` table to preserve the legacy clamp-to-[0, 2]
        // behavior for out-of-range inputs.
        let bindings = vec![
            ParamBinding {
                id: Cow::Borrowed("amount"),
                spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
                target: ParamTarget::Node {
                    node: feedback,
                    param: "amount",
                },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("zoom"),
                spec: ParamSpec::continuous("zoom", "Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
                target: ParamTarget::Node {
                    node: feedback,
                    param: "zoom",
                },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("rotate"),
                spec: ParamSpec::continuous("rotate", "Rotate", -10.0, 10.0, 0.0, "F2", "Rotate"),
                target: ParamTarget::Node {
                    node: feedback,
                    param: "rotation",
                },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("mode"),
                spec: ParamSpec::whole_labels(
                    "mode",
                    "Mode",
                    0.0,
                    2.0,
                    0.0,
                    &["Screen", "Add", "Max"],
                    "Mode",
                ),
                target: ParamTarget::Node {
                    node: feedback,
                    param: "mode",
                },
                convert: ParamConvert::EnumRemap(Cow::Borrowed(&[0, 1, 2])),
            },
        ];

        Self {
            type_id: EffectTypeId::STYLIZED_FEEDBACK,
            graph,
            plan,
            bindings,
            source_resource,
            output_resource,
            state_store: StateStore::new(),
            render: None,
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

        for i in 0..plan.resource_count() {
            let id = ResourceId(i as u32);
            if !matches!(plan.resource_type(id), Some(PortType::Texture2D)) {
                continue;
            }
            let (label, is_source, is_output) = if id == source_resource {
                ("stylized-feedback-source", true, false)
            } else if id == output_resource {
                ("stylized-feedback-output", false, true)
            } else {
                ("stylized-feedback-intermediate", false, false)
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
            source_slot: source_slot.expect("source_resource must be Texture2D in plan"),
            output_slot: output_slot.expect("output_resource must be Texture2D in plan"),
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

impl PostProcessEffect for StylizedFeedbackFX {
    fn effect_type(&self) -> &EffectTypeId {
        &self.type_id
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
        if ctx.width == 0 || ctx.height == 0 {
            return;
        }

        // 1. Lazy-init or rebuild on resolution change.
        let needs_build = match &self.render {
            None => true,
            Some(r) => r.width != ctx.width || r.height != ctx.height,
        };
        if needs_build {
            // Resolution change invalidates the prev-frame buffers in
            // the StateStore — drop them so they get re-allocated black.
            // (Mirrors legacy resize() which cleared the states map.)
            self.state_store.cleanup_all();
            self.render = Some(RenderState::build(
                gpu.device,
                ctx.width,
                ctx.height,
                &self.plan,
                self.source_resource,
                self.output_resource,
            ));
        }
        let render = self.render.as_mut().expect("render initialized above");

        // 2. Step 17: route every host param via the declarative
        //    `bindings` slice directly to the `feedback` node.
        apply_param_bindings(&self.bindings, &mut self.graph, None, &fx.param_values)
            .expect("route stylized-feedback bindings");

        // 3. Copy the layer's source into the graph's pre-bound Source slot.
        let backend = render.executor.backend();
        let source_tex = backend
            .texture_2d(render.source_slot)
            .expect("source slot pre-bound");
        gpu.copy_texture_to_texture(source, source_tex, ctx.width, ctx.height);

        // 4. Run the graph with the per-owner state store.
        let frame_time = FrameTime {
            beats: manifold_core::Beats(f64::from(ctx.beat)),
            seconds: manifold_core::Seconds(f64::from(ctx.time)),
            delta: manifold_core::Seconds(f64::from(ctx.dt)),
        };
        render.executor.execute_frame_with_state(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
            &mut self.state_store,
            ctx.owner_key,
        );

        // 5. Blit the graph's output into the layer chain's target.
        let output_tex = render
            .executor
            .backend()
            .texture_2d(render.output_slot)
            .expect("output slot pre-bound");
        gpu.copy_texture_to_texture(output_tex, target, ctx.width, ctx.height);
    }

    fn clear_state(&mut self) {
        // Wipes every owner's prev-frame buffer — used on seek so trails
        // don't carry stale content.
        self.state_store.cleanup_all();
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // The next apply() observes the new ctx.width/height and rebuilds
        // automatically; clear stale state so we re-allocate at new dims.
        self.state_store.cleanup_all();
        self.render = None;
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.state_store.cleanup_owner(owner_key);
    }
}

impl StatefulEffect for StylizedFeedbackFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.state_store.cleanup_owner(owner_key);
    }
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.state_store.cleanup_owner(owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.state_store.cleanup_all();
    }
}
