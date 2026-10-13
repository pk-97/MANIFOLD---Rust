//! `system.mesh_input`: a mesh boundary whose buffers the owner prebinds.
//! The node performs no copies; consumers read the prebound buffers on the
//! same GPU encoder.

use crate::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use crate::parameters::ParamDef;
use crate::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::mesh::MeshVertex;
use std::borrow::Cow;

pub struct MeshInput {
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
    fn boundary_reason(&self) -> Option<crate::freeze::classify::BoundaryReason> {
        Some(crate::freeze::classify::BoundaryReason::NonGpu)
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
inventory::submit! { crate::persistence::PrimitiveFactory { type_id: "system.mesh_input", create: || Box::new(MeshInput { ty: EffectNodeType::new("system.mesh_input") }), picker: None } }
