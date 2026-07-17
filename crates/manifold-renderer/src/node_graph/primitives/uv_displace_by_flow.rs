//! `node.uv_displace_by_flow` — sample a source texture at UVs
//! displaced by a 2D flow vector field.
//!
//! offset = (flow.rb - bias) * weight
//! sampled_uv = uv + offset
//!
//! Companion to `node.flow_field_noise`. Also accepts any other
//! flow source where the X / Y components are packed into the R /
//! B channels (Watercolor convention). For signed flow data with
//! mean zero, set `bias = 0`.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplaceUniforms {
    weight: f32,
    bias: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: UvDisplaceByFlow,
    type_id: "node.uv_displace_by_flow",
    purpose: "Sample a source texture at UVs displaced by a 2D flow vector field. offset = (flow.rb - bias) * weight. Pair with node.flow_field_noise upstream for procedural distortion (Watercolor-style), or with any other primitive that emits 2-channel offsets packed in R/B (e.g. an upstream node.custom_convolution configured as a gradient).",
    inputs: {
        in: Texture2D required,
        flow: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("weight"),
            label: "Weight",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((-0.1, 0.1)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bias"),
            label: "Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "bias = 0.5 maps [0, 1] flow channels to [-0.5, +0.5] offset (Watercolor convention). bias = 0 treats already-signed flow data directly. weight scales the offset in UV units (Watercolor default 0.001 ≈ 1 pixel at 1080p). Sampling is bilinear; out-of-bounds UVs wrap or clamp depending on the sampler (default linear+clamp).",
    examples: [],
    picker: { label: "UV Displace by Flow", category: Atom },
    summary: "Samples the image at positions pushed by a flow field, so the picture smears along the motion. The consumer for an optical-flow or noise flow field.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["flow displace", "advect", "warp by flow", "Displace"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/uv_displace_by_flow_body.wgsl"),
    input_access: [Gather, Coincident],
}

impl Primitive for UvDisplaceByFlow {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let weight = match ctx.params.get("weight") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.001,
        };
        let bias = match ctx.params.get("bias") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(flow) = ctx.inputs.texture_2d("flow") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: `in` is a Gather input (sampled at uv + flow offset).
            // The generated kernel's bindings match the set below (textures then
            // sampler). uv_displace_by_flow.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.uv_displace_by_flow standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.uv_displace_by_flow",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DisplaceUniforms {
            weight,
            bias,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: flow,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.uv_displace_by_flow",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn uv_displace_by_flow_declares_two_texture_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(UvDisplaceByFlow::TYPE_ID, "node.uv_displace_by_flow");
        assert_eq!(UvDisplaceByFlow::INPUTS.len(), 2);
        assert_eq!(UvDisplaceByFlow::INPUTS[0].name, "in");
        assert_eq!(UvDisplaceByFlow::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(UvDisplaceByFlow::INPUTS[1].name, "flow");
        assert_eq!(UvDisplaceByFlow::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(UvDisplaceByFlow::OUTPUTS.len(), 1);
        assert_eq!(UvDisplaceByFlow::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn uv_displace_by_flow_has_weight_and_bias_params() {
        let names: Vec<&str> = UvDisplaceByFlow::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["weight", "bias"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = UvDisplaceByFlow::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.uv_displace_by_flow");
    }
}
