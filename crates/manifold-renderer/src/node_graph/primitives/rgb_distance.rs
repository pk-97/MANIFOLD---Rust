//! `node.rgb_distance` — per-pixel Euclidean distance from a scalar-bound RGB
//! target colour.
//!
//! This is the scalar-bindable counterpart to `node.chroma_key`: it exposes
//! the same RGB distance as three independent port-shadowed parameters so a
//! graph can drive red, green, and blue without a Vec3 binding.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

fn finite_unit(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

crate::primitive! {
    name: RgbDistance,
    type_id: "node.rgb_distance",
    purpose: "Output the per-pixel Euclidean RGB distance from a target colour. Red, green, and blue are independent scalar port-shadowed controls so a graph can drive the target without a Vec3 binding.",
    inputs: {
        in: Texture2D required,
        red: ScalarF32 optional,
        green: ScalarF32 optional,
        blue: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("red"),
            label: "Target Red",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("green"),
            label: "Target Green",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("blue"),
            label: "Target Blue",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Output RGB is the Euclidean distance from the target and alpha is 1.0. Wire scalar producers into red/green/blue to animate the target, then feed the distance into node.smoothstep and node.invert for a selectable colour mask. This keeps the existing node.chroma_key Vec3 ABI unchanged.",
    examples: [],
    picker: { label: "RGB Distance", category: Atom },
    summary: "Measures each pixel's Euclidean RGB distance from a scalar-bindable target colour.",
    category: Mask,
    role: Filter,
    aliases: ["rgb distance", "colour distance", "color distance", "colour proximity"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rgb_distance_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RgbDistanceUniforms {
    red: f32,
    green: f32,
    blue: f32,
    _pad: f32,
}

impl Primitive for RgbDistance {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let red = finite_unit(ctx.scalar_or_param("red", 1.0), 1.0);
        let green = finite_unit(ctx.scalar_or_param("green", 0.0), 0.0);
        let blue = finite_unit(ctx.scalar_or_param("blue", 0.0), 0.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if out_tex.width == 0 || out_tex.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = RgbDistanceUniforms {
            red,
            green,
            blue,
            _pad: 0.0,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.rgb_distance",
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
    fn blob_v2_rgb_distance_declares_scalar_target_ports() {
        assert_eq!(RgbDistance::TYPE_ID, "node.rgb_distance");
        assert_eq!(RgbDistance::INPUTS.len(), 4);
        assert_eq!(RgbDistance::INPUTS[0].name, "in");
        assert!(RgbDistance::INPUTS[0].required);
        assert_eq!(RgbDistance::INPUTS[0].ty, PortType::Texture2D);
        for (port, name) in RgbDistance::INPUTS[1..]
            .iter()
            .zip(["red", "green", "blue"])
        {
            assert_eq!(port.name, name);
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(RgbDistance::OUTPUTS.len(), 1);
        assert_eq!(RgbDistance::OUTPUTS[0].name, "out");
        assert_eq!(RgbDistance::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn blob_v2_rgb_distance_defaults_and_ranges_match_colour_contract() {
        let params = RgbDistance::PARAMS;
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "red");
        assert_eq!(params[0].default, ParamValue::Float(1.0));
        assert_eq!(params[1].name, "green");
        assert_eq!(params[1].default, ParamValue::Float(0.0));
        assert_eq!(params[2].name, "blue");
        assert_eq!(params[2].default, ParamValue::Float(0.0));
        for param in params {
            assert_eq!(param.ty, ParamType::Float);
            assert_eq!(param.range, Some((0.0, 1.0)));
        }
    }

    #[test]
    fn blob_v2_rgb_distance_fixture_matches_euclidean_formula() {
        let target: [f32; 3] = [0.2, 0.4, 0.8];
        let pixels: [[f32; 3]; 3] = [[0.2, 0.4, 0.8], [1.0, 0.0, 0.5], [0.0, 1.0, 0.0]];
        let expected: [f32; 3] = [0.0, 0.9433981, 1.0198039];
        for (pixel, expected) in pixels.into_iter().zip(expected) {
            let distance = ((pixel[0] - target[0]).powi(2)
                + (pixel[1] - target[1]).powi(2)
                + (pixel[2] - target[2]).powi(2))
            .sqrt();
            assert!((distance - expected).abs() < 1e-6);
        }
        assert_eq!(finite_unit(f32::NAN, 0.2), 0.2);
        assert_eq!(finite_unit(-1.0, 0.2), 0.0);
        assert_eq!(finite_unit(2.0, 0.2), 1.0);

        let node = RgbDistance::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(node.type_id().as_str(), "node.rgb_distance");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::TextureDiff;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::render_target::RenderTarget;
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };

    const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

    fn upload_fixture(device: &manifold_gpu::GpuDevice) -> GpuTexture {
        let pixels: [[f32; 4]; 3] = [
            [0.2, 0.4, 0.8, 0.25],
            [1.0, 0.0, 0.5, 0.75],
            [0.0, 1.0, 0.0, 0.5],
        ];
        let mut packed = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            packed.extend(pixel.map(f16::from_f32));
        }
        let texture = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 1,
            depth: 1,
            format: FORMAT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "blob-v2-rgb-distance-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                packed.as_ptr().cast::<u8>(),
                std::mem::size_of_val(packed.as_slice()),
            )
        };
        device.upload_texture(&texture, bytes);
        texture
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        input: &GpuTexture,
        params: &[u8],
        label: &str,
        standalone: bool,
    ) -> RenderTarget {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        let output = RenderTarget::new(device, input.width, input.height, FORMAT, label);
        let mut encoder = device.create_encoder(label);
        let mut bindings = vec![
            GpuBinding::Bytes {
                binding: 0,
                data: params,
            },
            GpuBinding::Texture {
                binding: 1,
                texture: input,
            },
        ];
        if standalone {
            bindings.push(GpuBinding::Sampler {
                binding: 2,
                sampler: &sampler,
            });
        }
        bindings.push(GpuBinding::Texture {
            binding: if standalone { 3 } else { 2 },
            texture: &output.texture,
        });
        encoder.dispatch_compute(
            &pipeline,
            &bindings,
            [input.width.div_ceil(16), input.height.div_ceil(16), 1],
            label,
        );
        encoder.commit_and_wait_completed();
        output
    }

    fn readback(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> Vec<[f32; 4]> {
        let bytes_per_row = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
        let mut encoder = device.create_encoder("blob-v2-rgb-distance-readback");
        encoder.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        encoder.commit_and_wait_completed();
        let ptr = buffer
            .mapped_ptr()
            .expect("RGB distance readback should be mapped");
        let words: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        words
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

    #[test]
    fn blob_v2_rgb_distance_standalone_and_fused_match_fixture() {
        let device = crate::test_device();
        let input = upload_fixture(&device);
        let params = RgbDistanceUniforms {
            red: 0.2,
            green: 0.4,
            blue: 0.8,
            _pad: 0.0,
        };
        let params = bytemuck::bytes_of(&params);
        let standalone = standalone_for_spec::<RgbDistance>()
            .expect("RGB distance standalone codegen should succeed");
        let fused = generate_fused(&FusionRegion {
            nodes: vec![RegionNode {
                node_id: NodeInstanceId(0),
                fusion_kind: FusionKind::Pointwise,
                body: RgbDistance::WGSL_BODY.expect("RGB distance body"),
                params: RgbDistance::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![],
                node_inputs: RgbDistance::INPUTS,
                node_outputs: RgbDistance::OUTPUTS,
                node_includes: &[],
                derived_uniforms: &[],
                type_id: String::new(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 1,
            outputs: vec![(NodeInstanceId(0), "out".to_owned())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: vec![],
            sampled_externals: vec![],
            camera_externals: 0,
            output_capacity: None,
        })
        .expect("RGB distance fused codegen should succeed")
        .wgsl;

        let standalone_output = dispatch(
            &device,
            &standalone,
            &input,
            params,
            "blob-v2-rgb-standalone",
            true,
        );
        let fused_output = dispatch(&device, &fused, &input, params, "blob-v2-rgb-fused", false);
        let standalone_pixels = readback(&device, &standalone_output.texture);
        let fused_pixels = readback(&device, &fused_output.texture);
        let expected = [0.0, 0.9433981, 1.0198039];
        for ((standalone, fused), expected) in
            standalone_pixels.iter().zip(&fused_pixels).zip(expected)
        {
            for channel in 0..3 {
                assert!((standalone[channel] - expected).abs() < 0.002);
                assert!((fused[channel] - expected).abs() < 0.002);
            }
            assert!((standalone[3] - 1.0).abs() < 0.002);
            assert!((fused[3] - 1.0).abs() < 0.002);
        }

        let diff = TextureDiff::new(&device).compare(
            &device,
            &standalone_output.texture,
            &fused_output.texture,
            0.002,
            0.002,
        );
        assert!(
            diff.passes(0.0),
            "standalone/fused RGB distance mismatch: {diff:?}"
        );
    }
}
