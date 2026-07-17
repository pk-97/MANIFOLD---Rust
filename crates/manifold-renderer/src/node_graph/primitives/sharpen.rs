//! `node.sharpen` — single-knob 4-neighbour Laplacian unsharp mask.
//!
//! Curated wrapper over the same convolution `convolution_2d_9tap`
//! could express, but with a single `amount` knob: driving sharpening
//! from one outer slider via convolution_2d_9tap would require five
//! `affine_scalar` nodes computing kernel weights (`-amount * 0.5` and
//! `1 + 2 * amount`) and fanning into 9 ports — that's the §6.1 cue
//! to ship the primitive instead.
//!
//! Bit-equivalent to the legacy `mri_slice_compute.wgsl` sharpen pass
//! when applied to a grayscale source; the per-channel math
//! generalises to RGBA without changing the grayscale result.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SharpenUniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Sharpen,
    type_id: "node.sharpen",
    purpose: "Single-knob 4-neighbour Laplacian unsharp mask. `amount = 0` passes the source through unchanged; positive values add increasingly aggressive edge enhancement. Curated wrapper over the same math node.custom_convolution can express, factored out so one outer-card slider drives sharpening directly without needing five affine_scalar nodes to compute kernel weights.",
    inputs: {
        in: Texture2D required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 3.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Port-shadowable amount input — wire a single outer-card Sharpen slider straight to this node's amount port via presetMetadata.bindings (convert: Float). amount = 0 fast-paths to a passthrough (no Laplacian taps). The tap spacing reads `textureDimensions` of the source, so the kernel scales with source resolution. For wider blurs prefer node.variable_blur — this primitive is for crisp edge enhancement, not arbitrary radius work.",
    examples: [],
    picker: { label: "Sharpen", category: Atom },
    summary: "Sharpens the image by boosting the difference between each pixel and its neighbours. At 0 it passes through, higher values make edges crisper.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["sharpen", "unsharp mask", "crisp"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sharpen_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for Sharpen {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = ctx.scalar_or_param("amount", 1.0).max(0.0);

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: `in` is a Gather input (4-neighbour Laplacian). The
            // generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3), matching the
            // set below. sharpen.wgsl is retained as the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.sharpen standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.sharpen",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SharpenUniforms {
            amount,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.sharpen",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{PortType, ScalarType};

    #[test]
    fn sharpen_declares_texture_in_amount_port_and_texture_out() {
        assert_eq!(Sharpen::TYPE_ID, "node.sharpen");
        assert_eq!(Sharpen::INPUTS.len(), 2);
        assert_eq!(Sharpen::INPUTS[0].name, "in");
        assert_eq!(Sharpen::INPUTS[0].ty, PortType::Texture2D);
        assert!(Sharpen::INPUTS[0].required);
        assert_eq!(Sharpen::INPUTS[1].name, "amount");
        assert_eq!(Sharpen::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert!(!Sharpen::INPUTS[1].required);
        assert_eq!(Sharpen::OUTPUTS.len(), 1);
        assert_eq!(Sharpen::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn sharpen_has_one_amount_param() {
        let names: Vec<&str> = Sharpen::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["amount"]);
    }

    #[test]
    fn primitive_registers() {
        let prim = Sharpen::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.sharpen");
    }
}
