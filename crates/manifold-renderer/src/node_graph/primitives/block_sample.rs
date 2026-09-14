//! `node.block_sample` — full-resolution block-centre texture sampling.
//!
//! Unlike `node.downsample`, this keeps the output dimensions unchanged. Each
//! output pixel samples the geometric centre of the integer pixel block that
//! contains it, with the source coordinate clamped to the source edges.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlockSampleUniforms {
    block_size: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: BlockSample,
    type_id: "node.block_sample",
    purpose: "Sample the input at the centre of each integer pixel block while keeping the output full resolution. `block_size = 1` is an identity; larger values create clean block pixelation with edge clamping.",
    inputs: {
        in: Texture2D required,
        block_size: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("block_size"),
            label: "Block Size",
            ty: ParamType::Float,
            default: ParamValue::Float(16.0),
            range: Some((1.0, 128.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Full-resolution pixelation for retained imagery and flow fields. The scalar input shadows the `block_size` parameter; values are rounded and clamped to at least 1. `block_size = 1` samples each source texel at its own centre, and partial blocks at the right/bottom edges clamp to the last source texel. Pair with `node.remap` or `node.mix` when the block-centre sample is one stage in a larger mosh graph.",
    examples: [],
    picker: { label: "Block Sample", category: Atom },
    summary: "Pixelates an image at full resolution by repeating each block's centre sample.",
    category: Routing,
    role: Filter,
    aliases: ["block sample", "pixelate", "pixelation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/block_sample_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for BlockSample {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let block_size = ctx.scalar_or_param("block_size", 16.0).round().max(1.0);
        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if out.width == 0 || out.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = BlockSampleUniforms {
            block_size,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[src],
            Some(sampler),
            out,
            "node.block_sample",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_full_resolution_texture_and_block_size() {
        assert_eq!(BlockSample::TYPE_ID, "node.block_sample");
        assert_eq!(BlockSample::INPUTS.len(), 2);
        assert_eq!(BlockSample::INPUTS[0].name, "in");
        assert_eq!(BlockSample::INPUTS[0].ty, PortType::Texture2D);
        assert!(BlockSample::INPUTS[0].required);
        assert_eq!(BlockSample::INPUTS[1].name, "block_size");
        assert_eq!(BlockSample::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert!(!BlockSample::INPUTS[1].required);
        assert_eq!(BlockSample::OUTPUTS.len(), 1);
        assert_eq!(BlockSample::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(BlockSample::PARAMS.len(), 1);
        assert_eq!(BlockSample::PARAMS[0].name, "block_size");
        assert_eq!(BlockSample::PARAMS[0].default, ParamValue::Float(16.0));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuSamplerDesc, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };

    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use crate::render_target::RenderTarget;

    fn rgba16_gradient(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> manifold_gpu::GpuTexture {
        let mut pixels = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                pixels[i] = f16::from_f32(x as f32 / (w.saturating_sub(1).max(1)) as f32);
                pixels[i + 1] = f16::from_f32(y as f32 / (h.saturating_sub(1).max(1)) as f32);
                pixels[i + 2] = f16::from_f32(0.25);
                pixels[i + 3] = f16::from_f32((x + y) as f32 / (w + h - 2).max(1) as f32);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "block-sample-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                pixels.as_ptr().cast::<u8>(),
                std::mem::size_of_val(pixels.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn readback(
        device: &manifold_gpu::GpuDevice,
        texture: &manifold_gpu::GpuTexture,
    ) -> Vec<[f32; 4]> {
        let bytes_per_row = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
        let mut enc = device.create_encoder("block-sample-readback");
        enc.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        enc.commit_and_wait_completed();
        let ptr = buffer
            .mapped_ptr()
            .expect("readback buffer should be mapped");
        let pixels: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        pixels
            .chunks_exact(4)
            .map(|p| {
                [
                    f16::from_bits(p[0]).to_f32(),
                    f16::from_bits(p[1]).to_f32(),
                    f16::from_bits(p[2]).to_f32(),
                    f16::from_bits(p[3]).to_f32(),
                ]
            })
            .collect()
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        input: &manifold_gpu::GpuTexture,
        block_size: f32,
    ) -> Vec<[f32; 4]> {
        let pipeline = device.create_compute_pipeline(
            &standalone_for_spec::<BlockSample>().expect("block sample standalone codegen"),
            ENTRY,
            "node.block_sample.gpu-test",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(
            device,
            input.width,
            input.height,
            GpuTextureFormat::Rgba16Float,
            "block-sample-output",
        );
        let mut enc = device.create_encoder("block-sample-dispatch");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&BlockSampleUniforms {
                        block_size,
                        _pad0: 0.0,
                        _pad1: 0.0,
                        _pad2: 0.0,
                    }),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: input,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &out.texture,
                },
            ],
            [input.width.div_ceil(16), input.height.div_ceil(16), 1],
            "node.block_sample.gpu-test",
        );
        enc.commit_and_wait_completed();
        readback(device, &out.texture)
    }

    #[test]
    fn gpu_formula_preserves_coordinates_and_alpha_at_size_one() {
        let device = crate::test_device();
        let input = rgba16_gradient(&device, 4, 4);
        let output = dispatch(&device, &input, 1.0);
        for (i, pixel) in output.iter().enumerate() {
            let x = (i % 4) as f32;
            let y = (i / 4) as f32;
            let expected = [x / 3.0, y / 3.0, 0.25, (x + y) / 6.0];
            for channel in 0..4 {
                assert!(
                    (pixel[channel] - expected[channel]).abs() < 0.01,
                    "pixel {i} channel {channel}: got {} expected {}",
                    pixel[channel],
                    expected[channel]
                );
            }
        }
    }

    #[test]
    fn gpu_formula_samples_block_centres_and_clamps_edges() {
        let device = crate::test_device();
        let input = rgba16_gradient(&device, 5, 2);
        let output = dispatch(&device, &input, 4.0);
        // The first block spans x=0..3; linear sampling at its geometric
        // centre averages the two middle texels at x=1 and x=2.
        let expected_x = (1.0 / 4.0 + 2.0 / 4.0) * 0.5;
        assert!((output[0][0] - expected_x).abs() < 0.02);
        assert!((output[0][3] - 0.5).abs() < 0.02);
        // The partial right-edge block clamps to the final source texel.
        assert!((output[4][0] - 1.0).abs() < 0.02);
        assert!((output[4][3] - 1.0).abs() < 0.02);
    }
}
