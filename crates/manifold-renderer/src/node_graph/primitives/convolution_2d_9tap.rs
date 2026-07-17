//! `node.custom_convolution` — general 3×3 non-separable
//! convolution with a uniform-supplied kernel.
//!
//! Nine kernel weights, optional bias, optional normalisation by
//! sum(weights). Useful for arbitrary 3×3 filters: edge detection
//! (Sobel / Laplacian), embossing, sharpening, blur, watercolor-
//! style diffusion. The legacy Watercolor diffusion pass uses a
//! 9-tap pattern with hardcoded weights — this primitive lifts that
//! shape into a reusable building block where the kernel is data.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConvUniforms {
    k0: f32, k1: f32, k2: f32, k3: f32,
    k4: f32, k5: f32, k6: f32, k7: f32,
    k8: f32,
    bias: f32,
    normalise: u32,
    _pad0: u32,
}

crate::primitive! {
    name: Convolution2D9Tap,
    type_id: "node.custom_convolution",
    purpose: "General 3×3 non-separable convolution with a user-supplied kernel (9 float weights k0..k8 in row-major order, k4 = center). Optional bias and sum-normalisation. Useful for Sobel, Laplacian, emboss, sharpen, diffusion, custom edge detectors. The kernel is data — composable, modulatable, and discoverable for AI agents (vs hardcoded-weight specific primitives).",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef { name: Cow::Borrowed("k0"), label: "Kernel[0]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k1"), label: "Kernel[1]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k2"), label: "Kernel[2]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k3"), label: "Kernel[3]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k4"), label: "Kernel[4] (center)", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k5"), label: "Kernel[5]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k6"), label: "Kernel[6]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k7"), label: "Kernel[7]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("k8"), label: "Kernel[8]", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-16.0, 16.0)), enum_values: &[] },
        ParamDef {
            name: Cow::Borrowed("bias"),
            label: "Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("normalise"),
            label: "Normalise",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Kernel layout (row-major): k0 k1 k2 / k3 k4 k5 / k6 k7 k8. Defaults to identity (k4=1, rest=0) so an unconfigured node passes through. Example kernels: Laplacian = [0 -1 0 / -1 4 -1 / 0 -1 0], Sobel-X = [1 0 -1 / 2 0 -2 / 1 0 -1], box blur = [1/9 1/9 1/9 × 3 rows] (toggle normalise off), sharpen = [0 -1 0 / -1 5 -1 / 0 -1 0]. normalise=true divides by sum(weights) — useful for blurs that should preserve energy.",
    examples: [],
    picker: { label: "Custom Convolution", category: Atom },
    summary: "Runs a custom 3x3 kernel over the image, so you can build your own blur, sharpen, edge-detect, or emboss from nine weights. For when the preset filters don't do quite what you want.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["custom convolution", "9tap", "kernel", "convolve", "filter matrix", "Convolution Kernel"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/convolution_2d_9tap_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for Convolution2D9Tap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        fn f(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        }

        // Read all params upfront so the encoder borrow doesn't fight
        // with the immutable param-map borrow inside the closure.
        let normalise = matches!(ctx.params.get("normalise"), Some(ParamValue::Bool(true)));
        let k0 = f(ctx, "k0", 0.0);
        let k1 = f(ctx, "k1", 0.0);
        let k2 = f(ctx, "k2", 0.0);
        let k3 = f(ctx, "k3", 0.0);
        let k4 = f(ctx, "k4", 1.0);
        let k5 = f(ctx, "k5", 0.0);
        let k6 = f(ctx, "k6", 0.0);
        let k7 = f(ctx, "k7", 0.0);
        let k8 = f(ctx, "k8", 0.0);
        let bias = f(ctx, "bias", 0.0);

        let Some(src) = ctx.inputs.texture_2d("in") else {
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
            // Single-source: `in` is a Gather input (3×3 neighbourhood). Generated
            // kernel binds uniform(0)/tex(1)/samp(2)/dst(3); the 11 scalar params
            // (k0..k8, bias, normalise) match the hand uniform order exactly and
            // the body recovers the texel step from `dims`.
            // convolution_2d_9tap.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.custom_convolution standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.custom_convolution",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ConvUniforms {
            k0, k1, k2, k3, k4, k5, k6, k7, k8,
            bias,
            normalise: if normalise { 1 } else { 0 },
            _pad0: 0,
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
            "node.custom_convolution",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn convolution_2d_9tap_declares_texture_in_and_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Convolution2D9Tap::TYPE_ID, "node.custom_convolution");
        assert_eq!(Convolution2D9Tap::INPUTS.len(), 1);
        assert_eq!(Convolution2D9Tap::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(Convolution2D9Tap::OUTPUTS.len(), 1);
        assert_eq!(Convolution2D9Tap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn convolution_2d_9tap_has_nine_kernel_weights_bias_and_normalise() {
        let names: Vec<&str> = Convolution2D9Tap::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        for k in &["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7", "k8", "bias", "normalise"] {
            assert!(names.contains(k), "missing param {}", k);
        }
        assert_eq!(Convolution2D9Tap::PARAMS.len(), 11);
    }

    #[test]
    fn convolution_2d_9tap_default_is_identity() {
        let k4 = Convolution2D9Tap::PARAMS.iter().find(|p| p.name == "k4").unwrap();
        match k4.default {
            ParamValue::Float(v) => assert_eq!(v, 1.0),
            _ => panic!("k4 must default to 1.0 (identity)"),
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Convolution2D9Tap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.custom_convolution");
    }
}
