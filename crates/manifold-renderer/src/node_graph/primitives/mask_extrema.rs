//! `node.mask_extrema` — signed separable morphology for coverage masks.
//!
//! A positive radius expands coverage with a neighbourhood maximum. A
//! negative radius erodes it with a neighbourhood minimum. The axis is an
//! authored X/Y choice; two instances, X then Y, form a square structuring
//! element. The operation uses exact texel loads because it is a coverage
//! reduction and must never interpolate categorical or mask samples.

use std::borrow::Cow;

use manifold_gpu::GpuTextureFormat;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const MASK_EXTREMA_AXES: &[&str] = &["X", "Y"];

fn read_axis(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("axis") {
        Some(ParamValue::Enum(axis)) => (*axis).min(1),
        Some(ParamValue::Float(axis)) if axis.is_finite() => axis.round().clamp(0.0, 1.0) as u32,
        _ => 0,
    }
}

fn read_radius(ctx: &EffectNodeContext<'_, '_>) -> f32 {
    let value = match ctx.inputs.scalar("radius") {
        Some(ParamValue::Float(value)) if value.is_finite() => value,
        _ => ctx.param_f32("radius", 0.0),
    };
    value.round().clamp(-32.0, 32.0)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskExtremaUniforms {
    radius: f32,
    axis: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: MaskExtrema,
    type_id: "node.mask_extrema",
    purpose: "Apply a signed one-axis morphology to a coverage mask. Positive radius takes the neighbourhood maximum (dilate), negative radius takes the neighbourhood minimum (erode), and zero is an exact bypass. Pair X then Y instances for a square expansion or erosion; samples outside the image are zero.",
    inputs: {
        in: Texture2D required,
        radius: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: MASK_EXTREMA_AXES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "This is a coverage operation, not a label operation: it uses exact texel reads and outputs grayscale coverage with alpha 1. Pair two instances with axis X then Y to form a square structuring element. Positive radius expands foreground coverage; negative radius erodes it and treats outside-image samples as zero. Radius zero is an exact pass-through, useful for a graph-level bypass.",
    examples: [],
    picker: { label: "Mask Extrema", category: Atom },
    summary: "Expands or erodes a coverage mask along one image axis.",
    category: Mask,
    role: Filter,
    aliases: ["mask extrema", "mask dilate", "mask erode", "morphology", "coverage expand"],
    boundary_reason: BarrieredReduction,
}

impl Primitive for MaskExtrema {
    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "out").then_some(GpuTextureFormat::Rgba16Float)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if dst.width == 0 || dst.height == 0 {
            return;
        }

        let radius = read_radius(ctx);
        let axis = read_axis(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/mask_extrema.wgsl"),
                "cs_main",
                "node.mask_extrema",
            )
        });
        let uniforms = MaskExtremaUniforms {
            radius,
            axis,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: dst,
                },
            ],
            [dst.width.div_ceil(16), dst.height.div_ceil(16), 1],
            "node.mask_extrema",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_mask_input_radius_port_and_axis() {
        assert_eq!(MaskExtrema::TYPE_ID, "node.mask_extrema");
        assert_eq!(MaskExtrema::INPUTS.len(), 2);
        assert_eq!(MaskExtrema::INPUTS[0].name, "in");
        assert_eq!(MaskExtrema::INPUTS[0].ty, PortType::Texture2D);
        assert!(MaskExtrema::INPUTS[0].required);
        assert_eq!(MaskExtrema::INPUTS[1].name, "radius");
        assert_eq!(MaskExtrema::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert!(!MaskExtrema::INPUTS[1].required);
        assert_eq!(MaskExtrema::OUTPUTS.len(), 1);
        assert_eq!(MaskExtrema::OUTPUTS[0].name, "out");
        assert_eq!(MaskExtrema::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(MaskExtrema::PARAMS.len(), 2);
        assert_eq!(MaskExtrema::PARAMS[0].name, "radius");
        assert_eq!(MaskExtrema::PARAMS[0].range, Some((-32.0, 32.0)));
        assert_eq!(MaskExtrema::PARAMS[1].name, "axis");
        assert_eq!(MaskExtrema::PARAMS[1].enum_values, MASK_EXTREMA_AXES);
    }

    #[test]
    fn declares_fixed_rgba16float_output_and_boundary_classification() {
        let node = MaskExtrema::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(
            node.output_format("out"),
            Some(GpuTextureFormat::Rgba16Float)
        );
        assert_eq!(
            node.fusion_kind(),
            crate::node_graph::freeze::classify::FusionKind::Boundary
        );
        assert_eq!(
            node.boundary_reason(),
            Some(crate::node_graph::freeze::classify::BoundaryReason::BarrieredReduction)
        );
    }

    #[test]
    fn radius_and_axis_values_are_authored_in_the_declared_ranges() {
        let mut params = ahash::AHashMap::default();
        params.insert(Cow::Borrowed("axis"), ParamValue::Enum(99));
        assert_eq!(read_axis(&params), 1);
        params.insert(Cow::Borrowed("axis"), ParamValue::Float(-2.0));
        assert_eq!(read_axis(&params), 0);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage,
    };

    use crate::render_target::RenderTarget;

    fn upload_mask(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        values: &[f32],
    ) -> manifold_gpu::GpuTexture {
        assert_eq!(values.len(), (w * h) as usize);
        let mut pixels = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (index, &value) in values.iter().enumerate() {
            let value = f16::from_f32(value);
            pixels[index * 4] = value;
            pixels[index * 4 + 1] = value;
            pixels[index * 4 + 2] = value;
            pixels[index * 4 + 3] = f16::from_f32(1.0);
        }
        let texture = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "mask-extrema-blob-v2-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                pixels.as_ptr().cast::<u8>(),
                std::mem::size_of_val(pixels.as_slice()),
            )
        };
        device.upload_texture(&texture, bytes);
        texture
    }

    fn readback(
        device: &manifold_gpu::GpuDevice,
        texture: &manifold_gpu::GpuTexture,
    ) -> Vec<[f32; 4]> {
        let bytes_per_row = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
        let mut encoder = device.create_encoder("mask-extrema-blob-v2-readback");
        encoder.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("mask-extrema readback buffer");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        halves
            .chunks_exact(4)
            .map(|pixel| {
                [
                    f16::from_bits(pixel[0]).to_f32(),
                    f16::from_bits(pixel[1]).to_f32(),
                    f16::from_bits(pixel[2]).to_f32(),
                    f16::from_bits(pixel[3]).to_f32(),
                ]
            })
            .collect()
    }

    fn cpu_extrema(values: &[f32], w: u32, h: u32, radius: i32, axis: u32) -> Vec<f32> {
        let mut output = vec![0.0; values.len()];
        let extent = radius.unsigned_abs() as i32;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut result = values[(y * w as i32 + x) as usize];
                for offset in -extent..=extent {
                    let sx = if axis == 0 { x + offset } else { x };
                    let sy = if axis == 1 { y + offset } else { y };
                    let sample = if sx < 0 || sx >= w as i32 || sy < 0 || sy >= h as i32 {
                        0.0
                    } else {
                        values[(sy * w as i32 + sx) as usize]
                    };
                    result = if radius >= 0 {
                        result.max(sample)
                    } else {
                        result.min(sample)
                    };
                }
                output[(y * w as i32 + x) as usize] = result;
            }
        }
        output
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        pipeline: &GpuComputePipeline,
        input: &manifold_gpu::GpuTexture,
        radius: f32,
        axis: u32,
    ) -> Vec<[f32; 4]> {
        let output = RenderTarget::new(
            device,
            input.width,
            input.height,
            GpuTextureFormat::Rgba16Float,
            "mask-extrema-blob-v2-output",
        );
        let uniforms = MaskExtremaUniforms {
            radius,
            axis,
            _pad0: 0,
            _pad1: 0,
        };
        let mut encoder = device.create_encoder("mask-extrema-blob-v2-dispatch");
        encoder.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: input,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &output.texture,
                },
            ],
            [input.width.div_ceil(16), input.height.div_ceil(16), 1],
            "node.mask_extrema.blob-v2-gpu-test",
        );
        encoder.commit_and_wait_completed();
        readback(device, &output.texture)
    }

    #[test]
    fn blob_v2_mask_pixels() {
        let device = crate::test_device();
        let (w, h) = (5u32, 3u32);
        let values = vec![
            0.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.2, 1.0, 0.2, 0.0, 0.0, 0.2, 0.0, 0.2, 0.0,
        ];
        let input = upload_mask(&device, w, h, &values);
        let shader = include_str!("shaders/mask_extrema.wgsl");
        let pipeline =
            device.create_compute_pipeline(shader, "cs_main", "node.mask_extrema.blob-v2");

        // Radius zero must preserve every source channel exactly.
        let bypass = dispatch(&device, &pipeline, &input, 0.0, 0);
        for (index, pixel) in bypass.iter().enumerate() {
            assert!(
                (pixel[0] - values[index]).abs() < 0.001,
                "zero-radius pixel {index}: {pixel:?}"
            );
            assert!(
                (pixel[1] - values[index]).abs() < 0.001,
                "zero-radius green {index}: {pixel:?}"
            );
            assert!(
                (pixel[3] - 1.0).abs() < 0.001,
                "zero-radius alpha {index}: {pixel:?}"
            );
        }

        for (radius, axis) in [(1.0, 0), (-1.0, 1)] {
            let actual = dispatch(&device, &pipeline, &input, radius, axis);
            let expected = cpu_extrema(&values, w, h, radius as i32, axis);
            for (index, pixel) in actual.iter().enumerate() {
                assert!(
                    (pixel[0] - expected[index]).abs() < 0.01,
                    "pixel {index}, radius {radius}, axis {axis}: got {} expected {}",
                    pixel[0],
                    expected[index]
                );
                assert!(
                    (pixel[1] - pixel[0]).abs() < 0.001,
                    "coverage RGB mismatch at {index}: {pixel:?}"
                );
                assert!(
                    (pixel[2] - pixel[0]).abs() < 0.001,
                    "coverage RGB mismatch at {index}: {pixel:?}"
                );
                assert!(
                    (pixel[3] - 1.0).abs() < 0.001,
                    "alpha mismatch at {index}: {pixel:?}"
                );
            }
        }
    }
}
