//! `node.blob_track` — wraps the legacy
//! [`BlobTrackingFX`](crate::effects::blob_tracking::BlobTrackingFX)
//! effect as a monolithic primitive. The effect drives a native
//! CPU-side blob detector (via the FFI plugin) with a One-Euro
//! smoother and a GPU overlay render. Tightly coupled to the native
//! plugin's lifecycle, so we wrap rather than re-port. See
//! `docs/PRIMITIVE_LIBRARY_DESIGN.md` §6.5.
//!
//! The legacy `apply` early-returns at `amount <= 0` without writing
//! the target texture (the chain's `should_skip_default` would have
//! prevented `apply` being called). The graph runtime has no such
//! skip mechanism, so this wrapper explicitly blits source → target
//! at `amount <= 0` to keep the output well-defined.

use std::sync::OnceLock;

use manifold_core::EffectTypeId;

use crate::effects::blob_tracking::BlobTrackingFX;
use crate::effect::PostProcessEffect;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::node_graph::primitives::auto_gain::{build_effect_context, build_effect_instance};

pub const BLOB_TRACKING_TYPE_ID: &str = "node.blob_track";

pub struct BlobTracking {
    legacy: Option<BlobTrackingFX>,
}

impl BlobTracking {
    pub fn new() -> Self {
        Self { legacy: None }
    }
}

impl Default for BlobTracking {
    fn default() -> Self {
        Self::new()
    }
}

const BLOB_TRACKING_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const BLOB_TRACKING_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const BLOB_TRACKING_PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "thresh",
        label: "Threshold",
        ty: ParamType::Float,
        default: ParamValue::Float(0.65),
        range: Some((0.05, 0.9)),
        enum_values: &[],
    },
    ParamDef {
        name: "sens",
        label: "Sensitivity",
        ty: ParamType::Float,
        default: ParamValue::Float(0.85),
        range: Some((0.2, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "smooth",
        label: "Smoothing",
        ty: ParamType::Float,
        default: ParamValue::Float(0.7),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "connect",
        label: "Connect",
        ty: ParamType::Float,
        default: ParamValue::Float(0.35),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

const BLOB_TRACKING_PARAM_ORDER: &[&str] =
    &["amount", "thresh", "sens", "smooth", "connect"];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(BLOB_TRACKING_TYPE_ID))
}

impl BlobTracking {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: BLOB_TRACKING_TYPE_ID,
            purpose: "Native CPU blob detector with One-Euro smoothing and GPU overlay render. Spawns a background detection worker; up to 8 blobs tracked per frame with grace-period matching.",
            composition_notes: "Monolithic — coupled to the native FFI plugin's lifecycle. No atomic-primitive decomposition planned. At amount=0 the wrapper blits source through to target unchanged.",
            examples: &["preset.effect.blob_tracking"],
            inputs: &BLOB_TRACKING_INPUTS,
            outputs: &BLOB_TRACKING_OUTPUTS,
            params: &BLOB_TRACKING_PARAMS,
        }
    }
}

impl EffectNode for BlobTracking {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLOB_TRACKING_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLOB_TRACKING_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLOB_TRACKING_PARAMS
    }
    fn clear_state(&mut self) {
        if let Some(legacy) = self.legacy.as_mut() {
            <BlobTrackingFX as PostProcessEffect>::clear_state(legacy);
        }
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (target.width, target.height);

        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let fx = build_effect_instance(
            &EffectTypeId::BLOB_TRACKING,
            ctx,
            BLOB_TRACKING_PARAM_ORDER,
        );
        let eff_ctx = build_effect_context(ctx, width, height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("node.blob_track requires a GpuEncoder");

        // Mirror the legacy's should_skip-equivalent early-return:
        // the chain would have prevented apply() from running entirely.
        if amount <= 0.0 {
            gpu.copy_texture_to_texture(source, target, width, height);
            return;
        }

        let legacy = self
            .legacy
            .get_or_insert_with(|| BlobTrackingFX::new(gpu.device));
        legacy.apply(gpu, source, target, &fx, &eff_ctx);
    }
}
