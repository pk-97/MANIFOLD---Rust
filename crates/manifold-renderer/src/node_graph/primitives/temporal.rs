//! Temporal primitives — operations that maintain state across frames.
//!
//! V1 set: [`Feedback`].
//!
//! Temporal primitives are the first stateful nodes in the catalog. Their
//! state lives in the runtime's `StateStore`, keyed by
//! `(NodeInstanceId, OwnerKey)`, **not** in the node itself. This is the
//! pattern every future stateful primitive (frame difference, motion
//! blur, accumulators) follows.

use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTextureFormat,
};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::state_store::NodeState;
use crate::render_target::RenderTarget;

const SOURCE_INPUT: NodeInput = NodePort {
    name: "source",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Feedback — zoom/rotate/blend a previous-frame buffer with the current
// frame to produce visual trails.
// =====================================================================

pub const FEEDBACK_TYPE_ID: &str = "primitive.feedback";

pub const FEEDBACK_MODES: &[&str] = &["Screen", "Additive", "Max"];

const FEEDBACK_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const FEEDBACK_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const FEEDBACK_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        // Caller clamps to <= 0.98 in evaluate to prevent runaway feedback.
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "zoom",
        label: "Zoom",
        ty: ParamType::Float,
        default: ParamValue::Float(0.95),
        range: Some((0.9, 1.1)),
        enum_values: &[],
    },
    ParamDef {
        name: "rotation",
        label: "Rotation",
        ty: ParamType::Float,
        // Degrees on the slider; converted to radians in evaluate.
        default: ParamValue::Float(0.0),
        range: Some((-10.0, 10.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "mode",
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Screen
        range: None,
        enum_values: FEEDBACK_MODES,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FeedbackUniforms {
    feedback_amount: f32,
    zoom: f32,
    rotation: f32,
    mode: u32,
}

const FEEDBACK_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const DEG_TO_RAD: f32 = std::f32::consts::PI / 180.0;

/// Per-`(NodeInstanceId, OwnerKey)` persistent state — the previous
/// frame's output. Held by the runtime's `StateStore`.
struct FeedbackState {
    prev: RenderTarget,
    width: u32,
    height: u32,
}

impl NodeState for FeedbackState {}

/// Stateful feedback node. Reads the current frame, samples a
/// transformed previous-frame buffer, blends them, writes the result;
/// then copies the result back into the persistent buffer for next
/// frame.
///
/// State lives in the runtime's `StateStore`, keyed by
/// `(node_id, owner_key)` — different layers / clips that use the same
/// graph instance get independent feedback streams without the node
/// owning a `HashMap<owner_key, ...>` field.
pub struct Feedback {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl Feedback {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(FEEDBACK_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Feedback {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Feedback {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &FEEDBACK_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &FEEDBACK_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &FEEDBACK_PARAMS
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Resolve textures up-front. Backend-lifetime refs survive past
        // the encoder's mutable borrow below.
        let Some(source) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);
        if width == 0 || height == 0 {
            return;
        }

        // Read params with the same clamping the legacy effect did,
        // so output is bit-identical to StylizedFeedbackFX.
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        }
        .min(0.98);
        let zoom = match ctx.params.get("zoom") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.95,
        }
        .clamp(0.01, 10.0);
        let rotation_deg = match ctx.params.get("rotation") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(i)) => *i,
            _ => 0,
        };

        let uniforms = FeedbackUniforms {
            feedback_amount: amount,
            zoom,
            rotation: rotation_deg * DEG_TO_RAD,
            mode,
        };

        // Identity for the state lookup.
        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        // Split borrows: gpu and state are disjoint fields on ctx, so
        // both can be borrowed simultaneously. Helper methods that take
        // `&mut self` can't be used here for the same reason.
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("Feedback::evaluate requires a GpuEncoder");
        let store = ctx
            .state
            .as_deref_mut()
            .expect("Feedback::evaluate requires a StateStore");

        // Lazy-init the persistent prev-frame buffer. Cleared to black
        // on first allocation; resized (re-allocated) if dims change.
        let needs_alloc = match store.get::<FeedbackState>(node_id, owner_key) {
            Some(s) => s.width != width || s.height != height,
            None => true,
        };
        if needs_alloc {
            let prev = if let Some(pool) = gpu.pool {
                RenderTarget::new_pooled(pool, width, height, FEEDBACK_FORMAT, "feedback prev")
            } else {
                RenderTarget::new(gpu.device, width, height, FEEDBACK_FORMAT, "feedback prev")
            };
            gpu.clear_texture(&prev.texture, 0.0, 0.0, 0.0, 0.0);
            store.insert(
                node_id,
                owner_key,
                FeedbackState { prev, width, height },
            );
        }
        let state = store
            .get::<FeedbackState>(node_id, owner_key)
            .expect("just inserted above");

        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/feedback.wgsl"),
                "cs_main",
                "primitive.feedback",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Dispatch: source + prev → out.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &state.prev.texture,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "primitive.feedback",
        );

        // Copy the output back into the persistent buffer for next frame.
        // (Equivalent to StylizedFeedbackFX's PostBlit.)
        gpu.copy_texture_to_texture(out, &state.prev.texture, width, height);
    }

    fn clear_state(&mut self) {
        // The actual state lives in the StateStore; the host clears
        // that via StateStore::cleanup_owner / cleanup_all. This hook
        // exists so the node can release its cached pipeline / sampler
        // if it ever needs to (currently a no-op — pipeline is a
        // long-lived cache).
    }
}
