//! `node.infrared` — wraps the legacy
//! [`InfraredFX`](crate::effects::infrared::InfraredFX) effect as a
//! monolithic primitive.
//!
//! Infrared bakes 10 palette LUT textures at 512×1 resolution at
//! construction time and runs a single compute pass that samples
//! pixel luminance into the chosen LUT. Decomposing it into
//! `BakedPalette → ColorLut` primitives would either break bit-exact
//! parity (the graph runtime doesn't support per-slot texture
//! resolutions yet, so a baked palette at full render resolution
//! would interpolate differently than the legacy 512×1 LUT) or
//! require runtime work that's out of scope here. Treat it monolithic
//! per `docs/PRIMITIVE_LIBRARY_DESIGN.md` §6.5, same shape as
//! AutoGain / BlobTracking / WireframeDepth.

use std::sync::OnceLock;

use manifold_core::EffectTypeId;

use crate::effect::PostProcessEffect;
use crate::effects::infrared::InfraredFX;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::node_graph::primitives::auto_gain::{build_effect_context, build_effect_instance};

pub const INFRARED_TYPE_ID: &str = "node.infrared";

pub const INFRARED_PALETTES: &[&str] = &[
    "White Hot",
    "Black Hot",
    "Green NV",
    "Iron Bow",
    "Rainbow",
    "Lava",
    "Arctic",
    "Magenta",
    "Electric",
    "Toxic",
];

pub struct Infrared {
    legacy: Option<InfraredFX>,
}

impl Infrared {
    pub fn new() -> Self {
        Self { legacy: None }
    }
}

impl Default for Infrared {
    fn default() -> Self {
        Self::new()
    }
}

const INFRARED_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const INFRARED_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const INFRARED_PARAMS: [ParamDef; 3] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "palette",
        label: "Palette",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0),
        range: Some((0.0, 9.0)),
        enum_values: INFRARED_PALETTES,
    },
    ParamDef {
        name: "contrast",
        label: "Contrast",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.5, 3.0)),
        enum_values: &[],
    },
];

const INFRARED_PARAM_ORDER: &[&str] = &["amount", "palette", "contrast"];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(INFRARED_TYPE_ID))
}

impl Infrared {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: INFRARED_TYPE_ID,
            purpose: "Thermal-vision palette mapping. Pre-bakes 10 palette LUTs (White Hot, Black Hot, Green NV, Iron Bow, Rainbow, Lava, Arctic, Magenta, Electric, Toxic) at 512×1 resolution and dispatches a single compute pass that samples pixel luminance into the chosen LUT.",
            composition_notes: "Monolithic — the 10 baked LUTs are internal state owned by this primitive. A future `BakedPalette → node.color_lut` decomposition would need per-slot texture resolution support in the graph runtime to preserve bit-exact parity with the legacy 512×1 LUT.",
            examples: &["preset.effect.infrared"],
            inputs: &INFRARED_INPUTS,
            outputs: &INFRARED_OUTPUTS,
            params: &INFRARED_PARAMS,
        }
    }
}

impl EffectNode for Infrared {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &INFRARED_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &INFRARED_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &INFRARED_PARAMS
    }
    fn clear_state(&mut self) {
        if let Some(legacy) = self.legacy.as_mut() {
            <InfraredFX as PostProcessEffect>::clear_state(legacy);
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

        let fx = build_effect_instance(&EffectTypeId::INFRARED, ctx, INFRARED_PARAM_ORDER);
        let eff_ctx = build_effect_context(ctx, width, height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("node.infrared requires a GpuEncoder");
        let legacy = self
            .legacy
            .get_or_insert_with(|| InfraredFX::new(gpu.device));
        legacy.apply(gpu, source, target, &fx, &eff_ctx);
    }
}
