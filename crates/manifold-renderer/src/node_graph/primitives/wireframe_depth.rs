//! `node.wireframe_depth` — wraps the legacy
//! [`WireframeDepthFX`](crate::effects::wireframe_depth::WireframeDepthFX)
//! effect as a monolithic primitive. The effect runs a 15-pass
//! pipeline driven by a MiDaS depth DNN worker plus optional native
//! optical flow — too tightly coupled (depth state, flow state, mesh
//! pyramid) to decompose into atomic graph primitives.
//!
//! Same wrapper pattern as AutoGain (§6.5 c1) and BlobTracking
//! (§6.5 c2). Legacy `apply` early-returns at amount=0 without
//! writing the target, so the wrapper blits source → target in that
//! case for the graph runtime which has no skip mechanism.

use std::sync::OnceLock;

use manifold_core::EffectTypeId;

use crate::effect::PostProcessEffect;
use crate::effects::wireframe_depth::WireframeDepthFX;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::node_graph::primitives::auto_gain::{build_effect_context, build_effect_instance};

pub const WIREFRAME_DEPTH_TYPE_ID: &str = "node.wireframe_depth";

pub const WIREFRAME_DEPTH_BLEND_MODES: &[&str] = &[
    "Normal", "Add", "Multiply", "Screen", "Overlay", "Stencil", "Opaque",
];
pub const WIREFRAME_DEPTH_MESH_RATES: &[&str] = &["Every", "Half", "Third", "Quarter"];
pub const WIREFRAME_DEPTH_ONOFF: &[&str] = &["Off", "On"];

pub struct WireframeDepth {
    legacy: Option<WireframeDepthFX>,
}

impl WireframeDepth {
    pub fn new() -> Self {
        Self { legacy: None }
    }
}

impl Default for WireframeDepth {
    fn default() -> Self {
        Self::new()
    }
}

const WIREFRAME_DEPTH_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const WIREFRAME_DEPTH_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const WIREFRAME_DEPTH_PARAMS: [ParamDef; 12] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "density",
        label: "Density",
        ty: ParamType::Float,
        default: ParamValue::Float(260.0),
        range: Some((16.0, 280.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "width",
        label: "Width",
        ty: ParamType::Float,
        default: ParamValue::Float(1.5),
        range: Some((0.4, 3.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "z_scale",
        label: "Z Scale",
        ty: ParamType::Float,
        default: ParamValue::Float(1.35),
        range: Some((0.0, 2.5)),
        enum_values: &[],
    },
    ParamDef {
        name: "smooth",
        label: "Smoothing",
        ty: ParamType::Float,
        default: ParamValue::Float(0.90),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "subject",
        label: "Subject",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "blend",
        label: "Blend",
        ty: ParamType::Enum,
        default: ParamValue::Enum(6),
        range: Some((0.0, 6.0)),
        enum_values: WIREFRAME_DEPTH_BLEND_MODES,
    },
    ParamDef {
        name: "wire_res",
        label: "Wire Res",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.5, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "mesh_rate",
        label: "Mesh Rate",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0),
        range: Some((1.0, 4.0)),
        enum_values: WIREFRAME_DEPTH_MESH_RATES,
    },
    ParamDef {
        name: "flow",
        label: "Flow",
        ty: ParamType::Enum,
        default: ParamValue::Enum(1),
        range: Some((0.0, 1.0)),
        enum_values: WIREFRAME_DEPTH_ONOFF,
    },
    ParamDef {
        name: "lock",
        label: "Lock",
        ty: ParamType::Enum,
        default: ParamValue::Enum(1),
        range: Some((0.0, 1.0)),
        enum_values: WIREFRAME_DEPTH_ONOFF,
    },
    ParamDef {
        name: "edge_follow",
        label: "Edge Follow",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

const WIREFRAME_DEPTH_PARAM_ORDER: &[&str] = &[
    "amount",
    "density",
    "width",
    "z_scale",
    "smooth",
    "subject",
    "blend",
    "wire_res",
    "mesh_rate",
    "flow",
    "lock",
    "edge_follow",
];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(WIREFRAME_DEPTH_TYPE_ID))
}

impl WireframeDepth {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: WIREFRAME_DEPTH_TYPE_ID,
            purpose: "MiDaS depth-driven wireframe overlay. 15-pass pipeline: depth inference → adaptive density mesh → wireframe render → flow-locked compositing with 7 blend modes.",
            composition_notes: "Monolithic — DNN inference, flow state, and mesh-rate temporal scheduling are tightly coupled. At amount=0 the wrapper blits source through to target.",
            examples: &["preset.effect.wireframe_depth"],
            inputs: &WIREFRAME_DEPTH_INPUTS,
            outputs: &WIREFRAME_DEPTH_OUTPUTS,
            params: &WIREFRAME_DEPTH_PARAMS,
        }
    }
}

impl EffectNode for WireframeDepth {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &WIREFRAME_DEPTH_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &WIREFRAME_DEPTH_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &WIREFRAME_DEPTH_PARAMS
    }
    fn clear_state(&mut self) {
        if let Some(legacy) = self.legacy.as_mut() {
            <WireframeDepthFX as PostProcessEffect>::clear_state(legacy);
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
            &EffectTypeId::WIREFRAME_DEPTH,
            ctx,
            WIREFRAME_DEPTH_PARAM_ORDER,
        );
        let eff_ctx = build_effect_context(ctx, width, height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("node.wireframe_depth requires a GpuEncoder");

        if amount <= 0.0 {
            gpu.copy_texture_to_texture(source, target, width, height);
            return;
        }

        let legacy = self
            .legacy
            .get_or_insert_with(|| WireframeDepthFX::new(gpu.device));
        legacy.apply(gpu, source, target, &fx, &eff_ctx);
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: WIREFRAME_DEPTH_TYPE_ID,
        create: || Box::new(WireframeDepth::new()),
    }
}
