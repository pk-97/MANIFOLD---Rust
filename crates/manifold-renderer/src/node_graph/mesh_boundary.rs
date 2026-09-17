//! Shared mesh resources at the boundary between a scene and its render views.
//! These nodes perform no copies. The parent owns the buffers; a view borrows
//! them after resource installation and consumes them on the same GPU encoder.

use super::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use super::parameters::ParamDef;
use super::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::generators::mesh_common::MeshVertex;
use std::borrow::Cow;

pub struct MeshInput {
    ty: EffectNodeType,
}
pub struct MeshOutput {
    ty: EffectNodeType,
}

const INPUT_OUTPUTS: &[NodeOutput] = &[
    NodePort {
        name: Cow::Borrowed("depth"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("vertices"),
        ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("weights"),
        ty: PortType::Array(ArrayType::of_known::<f32>()),
        kind: PortKind::Output,
        required: false,
    },
];
const OUTPUT_INPUTS: &[NodeInput] = &[
    NodePort {
        name: Cow::Borrowed("depth"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("vertices"),
        ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("weights"),
        ty: PortType::Array(ArrayType::of_known::<f32>()),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("trigger_count"),
        ty: PortType::Scalar(super::ports::ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("trigger_baseline"),
        ty: PortType::Scalar(super::ports::ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

impl EffectNode for MeshInput {
    fn type_id(&self) -> &EffectNodeType {
        &self.ty
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        INPUT_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        (port == "depth").then_some(manifold_gpu::GpuTextureFormat::R32Float)
    }
    fn depth_rule(&self) -> super::depth_rule::DepthRule {
        super::depth_rule::DepthRule::Terminal
    }
    fn boundary_reason(&self) -> Option<super::freeze::classify::BoundaryReason> {
        Some(super::freeze::classify::BoundaryReason::NonGpu)
    }
    fn array_output_capacity(&self, port: &str, _: &ParamValues, _: &[(&str, u32)]) -> Option<u32> {
        // A standalone preparation has no owner. Its minimum storage is only
        // used by pure admission estimates; native views prebind real buffers.
        match port {
            "vertices" => Some(1536),
            "weights" => Some(1),
            _ => None,
        }
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
impl EffectNode for MeshOutput {
    fn type_id(&self) -> &EffectNodeType {
        &self.ty
    }
    fn inputs(&self) -> &[NodeInput] {
        OUTPUT_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn depth_rule(&self) -> super::depth_rule::DepthRule {
        super::depth_rule::DepthRule::Terminal
    }
    fn boundary_reason(&self) -> Option<super::freeze::classify::BoundaryReason> {
        Some(super::freeze::classify::BoundaryReason::NonGpu)
    }
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn carries_resources(&self) -> bool {
        true
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
inventory::submit! { super::persistence::PrimitiveFactory { type_id: "system.mesh_input", create: || Box::new(MeshInput { ty: EffectNodeType::new("system.mesh_input") }), picker: None } }
inventory::submit! { super::persistence::PrimitiveFactory { type_id: "system.mesh_output", create: || Box::new(MeshOutput { ty: EffectNodeType::new("system.mesh_output") }), picker: None } }
