//! `primitive.auto_gain` — wraps the legacy
//! [`AutoGainFX`](crate::effects::auto_gain::AutoGainFX) effect as a
//! monolithic primitive. The effect is a CPU envelope follower
//! (program-dependent attack/release) driving a GPU apply pass with
//! optional analog-style character coloration. Both halves are
//! mature, well-tested code — re-porting them as atomic graph
//! primitives would add risk without architectural benefit, so we
//! treat it as monolithic per `docs/PRIMITIVE_LIBRARY_DESIGN.md` §6.5.
//!
//! The primitive owns one legacy `AutoGainFX` instance and routes
//! `evaluate` straight into `AutoGainFX::apply`. The legacy keys its
//! per-owner envelope state by `EffectContext::owner_key`, which we
//! pass through unchanged from `EffectNodeContext::owner_key`. At
//! cutover (§6.6) the legacy effect implementation moves under this
//! module wholesale.

use std::sync::OnceLock;

use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

use crate::effect::EffectContext;
use crate::effects::auto_gain::AutoGainFX;
use crate::effect::PostProcessEffect;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;

pub const AUTO_GAIN_TYPE_ID: &str = "primitive.auto_gain";

pub const AUTO_GAIN_CHARACTERS: &[&str] = &["Clean", "Warm", "Film", "Vivid", "Grit"];

pub struct AutoGain {
    legacy: Option<AutoGainFX>,
}

impl AutoGain {
    pub fn new() -> Self {
        Self { legacy: None }
    }
}

impl Default for AutoGain {
    fn default() -> Self {
        Self::new()
    }
}

const AUTO_GAIN_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const AUTO_GAIN_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const AUTO_GAIN_PARAMS: [ParamDef; 7] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "ratio",
        label: "Ratio",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "punch",
        label: "Punch",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "target",
        label: "Target",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "hdr_ret",
        label: "HDR Retention",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "color",
        label: "Color",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((-1.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "char",
        label: "Character",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0),
        range: Some((0.0, 4.0)),
        enum_values: AUTO_GAIN_CHARACTERS,
    },
];

const AUTO_GAIN_PARAM_ORDER: &[&str] = &[
    "amount", "ratio", "punch", "target", "hdr_ret", "color", "char",
];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(AUTO_GAIN_TYPE_ID))
}

impl AutoGain {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: AUTO_GAIN_TYPE_ID,
            purpose: "Visual dynamics compressor. GPU-side parallel luminance reduction → CPU envelope follower with program-dependent attack/release → GPU apply pass with optional analog-style character coloration.",
            composition_notes: "Monolithic — the CPU envelope + GPU measure + GPU apply trio is treated as a single primitive because its parts have no obvious reuse in the atomic library and are tightly coupled in time (CPU envelope reads previous frame's measurement).",
            examples: &["preset.effect.auto_gain"],
            inputs: &AUTO_GAIN_INPUTS,
            outputs: &AUTO_GAIN_OUTPUTS,
            params: &AUTO_GAIN_PARAMS,
        }
    }
}

impl EffectNode for AutoGain {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &AUTO_GAIN_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &AUTO_GAIN_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &AUTO_GAIN_PARAMS
    }
    fn clear_state(&mut self) {
        if let Some(legacy) = self.legacy.as_mut() {
            <AutoGainFX as PostProcessEffect>::clear_state(legacy);
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

        // Build the legacy FX instance + context BEFORE taking the
        // mutable encoder borrow — those builders read `ctx.params`,
        // `ctx.time`, and `ctx.owner_key` immutably, which conflicts
        // with `ctx.gpu.as_deref_mut()`.
        let fx = build_effect_instance(&EffectTypeId::AUTO_GAIN, ctx, AUTO_GAIN_PARAM_ORDER);
        let eff_ctx = build_effect_context(ctx, width, height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("primitive.auto_gain requires a GpuEncoder");
        let legacy = self
            .legacy
            .get_or_insert_with(|| AutoGainFX::new(gpu.device));
        legacy.apply(gpu, source, target, &fx, &eff_ctx);
    }
}

/// Build a legacy `EffectInstance` from the primitive's named params.
/// Mirrors the positional param layout the legacy `EffectMetadata`
/// declares — `param_order` must list names in the registered order.
pub(super) fn build_effect_instance(
    type_id: &EffectTypeId,
    ctx: &EffectNodeContext<'_, '_>,
    param_order: &[&str],
) -> EffectInstance {
    let mut fx = EffectInstance::new(type_id.clone());
    fx.align_to_definition();
    fx.enabled = true;
    for (i, name) in param_order.iter().enumerate() {
        let Some(slot) = fx.param_values.get_mut(i) else {
            continue;
        };
        let value = match ctx.params.get(*name) {
            Some(ParamValue::Float(f)) => *f,
            Some(ParamValue::Int(i)) => *i as f32,
            Some(ParamValue::Enum(e)) => *e as f32,
            Some(ParamValue::Bool(b)) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            _ => continue,
        };
        slot.value = value;
    }
    fx
}

/// Build a legacy `EffectContext` from the graph's `EffectNodeContext`.
/// Width/height come from the output texture; time/beat/dt from the
/// frame's `FrameTime`. Fields the graph doesn't track (`is_clip_level`,
/// `edge_stretch_width`, `frame_count`, `output_width/height`) get
/// sensible defaults — primitives that need any of them should be
/// ported rather than wrapped.
pub(super) fn build_effect_context(
    ctx: &EffectNodeContext<'_, '_>,
    width: u32,
    height: u32,
) -> EffectContext {
    EffectContext {
        time: ctx.time.seconds.0 as f32,
        beat: ctx.time.beats.0 as f32,
        dt: ctx.time.delta.0 as f32,
        width,
        height,
        output_width: width,
        output_height: height,
        owner_key: ctx.owner_key,
        is_clip_level: false,
        edge_stretch_width: 0.5625,
        frame_count: 0,
    }
}
