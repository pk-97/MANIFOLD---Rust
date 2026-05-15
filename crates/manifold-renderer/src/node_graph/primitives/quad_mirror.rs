//! `node.quad_mirror` — wraps the legacy
//! [`QuadMirrorFX`](crate::effects::quad_mirror::QuadMirrorFX) effect
//! as a monolithic primitive. Same wrapper pattern as `AutoGain`,
//! `BlobTracking`, `Infrared`, and `WireframeDepth`: the shader is
//! tight and pixel-tested, so we route `evaluate` straight into the
//! legacy `apply` rather than authoring a fresh atomic primitive.

use std::sync::OnceLock;

use manifold_core::EffectTypeId;

use crate::effect::PostProcessEffect;
use crate::effects::quad_mirror::QuadMirrorFX;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::node_graph::primitives::auto_gain::{build_effect_context, build_effect_instance};

pub const QUAD_MIRROR_TYPE_ID: &str = "node.quad_mirror";

pub struct QuadMirror {
    legacy: Option<QuadMirrorFX>,
}

impl QuadMirror {
    pub fn new() -> Self {
        Self { legacy: None }
    }
}

impl Default for QuadMirror {
    fn default() -> Self {
        Self::new()
    }
}

const QUAD_MIRROR_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const QUAD_MIRROR_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const QUAD_MIRROR_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "amount",
    label: "Amount",
    ty: ParamType::Float,
    default: ParamValue::Float(1.0),
    range: Some((0.0, 1.0)),
    enum_values: &[],
}];

const QUAD_MIRROR_PARAM_ORDER: &[&str] = &["amount"];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(QUAD_MIRROR_TYPE_ID))
}

impl QuadMirror {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: QUAD_MIRROR_TYPE_ID,
            purpose: "Mirrors UVs around center in both axes with a crossfade blend by `amount`.",
            composition_notes: "Monolithic — single-pass fragment shader on Apple Silicon TBDR tile memory. At amount=0 the wrapper blits source through to target.",
            examples: &["preset.effect.quad_mirror"],
            inputs: &QUAD_MIRROR_INPUTS,
            outputs: &QUAD_MIRROR_OUTPUTS,
            params: &QUAD_MIRROR_PARAMS,
        }
    }
}

impl EffectNode for QuadMirror {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &QUAD_MIRROR_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &QUAD_MIRROR_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &QUAD_MIRROR_PARAMS
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
            _ => 1.0,
        };

        let fx = build_effect_instance(&EffectTypeId::QUAD_MIRROR, ctx, QUAD_MIRROR_PARAM_ORDER);
        let eff_ctx = build_effect_context(ctx, width, height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("node.quad_mirror requires a GpuEncoder");

        if amount <= 0.0 {
            gpu.copy_texture_to_texture(source, target, width, height);
            return;
        }

        let legacy = self
            .legacy
            .get_or_insert_with(|| QuadMirrorFX::new(gpu.device));
        legacy.apply(gpu, source, target, &fx, &eff_ctx);
    }
}
