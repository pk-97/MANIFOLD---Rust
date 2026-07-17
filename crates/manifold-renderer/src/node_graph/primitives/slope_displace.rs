//! `node.slope_displace` — emboss-style displacement. Soft-light-blends
//! a `base` layer over an `image` layer, takes the luminance Sobel
//! gradient of that blend (at a configurable pixel `step`), and
//! displaces `image` by the gradient × `weight`. Output is `image`
//! resampled at the displaced UV.
//!
//! Watercolor's slope pass extracted as a reusable atom — the
//! pigment-pooling edge-pull that follows contrast contours. Reusable
//! wherever a height-from-contrast displacement is wanted.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SlopeDisplaceUniforms {
    strength: f32,
    step: f32,
    weight: f32,
    _pad0: f32,
}

crate::primitive! {
    name: SlopeDisplace,
    type_id: "node.slope_displace",
    purpose: "Emboss-style displacement: soft-light-blend `base` over `image`, take the luminance Sobel gradient of the blend at a `step`-pixel offset, and displace `image` by gradient × `weight`. Output is `image` resampled at the displaced UV. Watercolor's slope pass as a reusable atom — the pigment-pooling edge-pull that follows contrast contours.",
    inputs: {
        base: Texture2D required,
        image: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("strength"),
            label: "Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(5.0),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("step"),
            label: "Step (px)",
            ty: ParamType::Float,
            default: ParamValue::Float(5.0),
            range: Some((1.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("weight"),
            label: "Weight",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.02)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Sobel uses Rec.709 luma. `base` is the soft-light blend layer (Watercolor wires the original source); `image` is both the blend's lower layer and the texture that gets displaced (Watercolor wires the diffusion-blurred result). step is in pixels, weight in UV units (Watercolor: strength 5, step 5, weight = displace 0.001).",
    examples: ["preset.effect.watercolor"],
    picker: { label: "Slope Displace", category: Atom },
    summary: "Pushes pixels along the slope of an embossed version of the image, an emboss-driven warp for liquid and paint looks.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["slope displace", "emboss warp", "paint"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/slope_displace_body.wgsl"),
    input_access: [Gather, Gather],
}

impl Primitive for SlopeDisplace {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let strength = read("strength", 5.0);
        let step = read("step", 5.0);
        let weight = read("weight", 0.001);

        let Some(base) = ctx.inputs.texture_2d("base") else {
            return;
        };
        let Some(image) = ctx.inputs.texture_2d("image") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `base` + `image` are both Gather inputs (neighbour taps + a final
            // dependent sample of image at the displaced UV). Generated kernel binds
            // uniform(0)/base(1)/image(2)/samp(3)/dst(4). slope_displace.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.slope_displace standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.slope_displace",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SlopeDisplaceUniforms {
            strength,
            step,
            weight,
            _pad0: 0.0,
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
                    texture: base,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: image,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.slope_displace",
        );
    }
}
