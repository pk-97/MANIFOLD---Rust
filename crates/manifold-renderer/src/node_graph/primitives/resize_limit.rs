//! `node.resize_limit` — bilinear resize with a longest-dimension cap.
//!
//! The input is reduced only when its longest dimension is larger than
//! `max_dim`. The other dimension is scaled by the same factor and rounded to
//! the nearest pixel, with a minimum of one pixel in either direction. Inputs
//! at or below the cap keep their original dimensions, so the primitive never
//! upscales an image.

use std::borrow::Cow;

use manifold_gpu::{GpuSamplerDesc, GpuTextureFormat};

use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEFAULT_MAX_DIM: u32 = 320;
const MIN_MAX_DIM: u32 = 64;
const MAX_MAX_DIM: u32 = 1024;

fn read_max_dim(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    let value = match params.get("max_dim") {
        Some(ParamValue::Float(value)) if value.is_finite() => value.round(),
        _ => DEFAULT_MAX_DIM as f32,
    };
    value.clamp(MIN_MAX_DIM as f32, MAX_MAX_DIM as f32) as u32
}

/// Return the capped dimensions using integer arithmetic so the planner and
/// runtime use the same aspect-preserving round-to-nearest rule.
fn capped_dims(width: u32, height: u32, max_dim: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }

    let longest = width.max(height);
    let cap = longest.min(max_dim.max(1));
    let round_scaled = |dimension: u32| {
        (((dimension as u64 * cap as u64) + (longest as u64 / 2)) / longest as u64).max(1) as u32
    };
    Some((round_scaled(width), round_scaled(height)))
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResizeLimitUniforms {
    max_dim: i32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: ResizeLimit,
    type_id: "node.resize_limit",
    purpose: "Resize a Texture2D with bilinear filtering so its longest dimension is at most max_dim. The aspect ratio is preserved, smaller inputs are kept at their original size, and the Rgba16Float output is safe to feed into another composable texture primitive.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_dim"),
            label: "Max Dimension",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_MAX_DIM as f32),
            range: Some((MIN_MAX_DIM as f32, MAX_MAX_DIM as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Use this at an analysis or preview boundary when a graph should preserve the complete source image while bounding GPU work. The output dimensions are resolved from the wired input dimensions, so the node does not promise a fixed canvas-relative scale. Inputs at or below max_dim pass through at their original dimensions; larger inputs are bilinearly sampled into the capped, aspect-preserving size.",
    examples: [],
    picker: { label: "Resize Limit", category: Atom },
    summary: "Bounds an image's longest side while preserving its proportions.",
    category: Routing,
    role: Filter,
    aliases: ["resize limit", "resize", "fit max dimension", "analysis resize"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/resize_limit_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for ResizeLimit {
    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "out").then_some(GpuTextureFormat::Rgba16Float)
    }

    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        let (width, height) = input_dims
            .iter()
            .find(|(name, _)| *name == "in")
            .map(|(_, dims)| *dims)?;
        capped_dims(width, height, read_max_dim(params))
    }

    fn output_canvas_max_dim(
        &self,
        port: &str,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<u32> {
        (port == "out").then(|| read_max_dim(params))
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

        let max_dim = read_max_dim(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = ResizeLimitUniforms {
            max_dim: max_dim as i32,
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
            dst,
            "node.resize_limit",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;

    fn params(max_dim: f32) -> crate::node_graph::effect_node::ParamValues {
        let mut values = ahash::AHashMap::default();
        values.insert(Cow::Borrowed("max_dim"), ParamValue::Float(max_dim));
        values
    }

    #[test]
    fn declares_capped_texture_input_and_int_param() {
        assert_eq!(ResizeLimit::TYPE_ID, "node.resize_limit");
        assert_eq!(ResizeLimit::INPUTS.len(), 1);
        assert_eq!(ResizeLimit::INPUTS[0].name, "in");
        assert_eq!(ResizeLimit::INPUTS[0].ty, PortType::Texture2D);
        assert!(ResizeLimit::INPUTS[0].required);
        assert_eq!(ResizeLimit::OUTPUTS.len(), 1);
        assert_eq!(ResizeLimit::OUTPUTS[0].name, "out");
        assert_eq!(ResizeLimit::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(ResizeLimit::PARAMS.len(), 1);
        assert_eq!(ResizeLimit::PARAMS[0].name, "max_dim");
        assert_eq!(ResizeLimit::PARAMS[0].ty, ParamType::Int);
        assert_eq!(ResizeLimit::PARAMS[0].default, ParamValue::Float(320.0));
        assert_eq!(ResizeLimit::PARAMS[0].range, Some((64.0, 1024.0)));
        let node = ResizeLimit::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(
            node.output_format("out"),
            Some(GpuTextureFormat::Rgba16Float)
        );
    }

    #[test]
    fn blob_v2_dimensions_cap_longest_side_and_preserve_aspect() {
        let node = ResizeLimit::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(
            node.output_dims("out", (1920, 1080), &[("in", (1920, 1080))], &params(320.0)),
            Some((320, 180))
        );
        assert_eq!(
            node.output_dims("out", (1920, 1080), &[], &params(320.0)),
            None,
            "the planner resolves an unknown group source against the live canvas"
        );
        assert_eq!(node.output_canvas_max_dim("out", &params(320.0)), Some(320));
        assert_eq!(
            node.output_dims("out", (1920, 1080), &[("in", (800, 600))], &params(320.0)),
            Some((320, 240))
        );
        assert_eq!(
            node.output_dims("out", (1920, 1080), &[("in", (100, 50))], &params(320.0)),
            Some((100, 50))
        );
        assert_eq!(
            node.output_dims("out", (1920, 1080), &[("in", (3, 1))], &params(64.0)),
            Some((3, 1))
        );
    }

    #[test]
    fn blob_v2_dimensions_round_each_axis_and_clamp_to_one() {
        let node = ResizeLimit::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(capped_dims(5, 3, 4), Some((4, 2)));
        assert_eq!(
            node.output_dims("out", (0, 0), &[("in", (1, 4096))], &params(64.0)),
            Some((1, 64))
        );
        assert_eq!(
            node.output_dims("other", (0, 0), &[("in", (5, 3))], &params(320.0)),
            None
        );
        assert_eq!(node.output_dims("out", (0, 0), &[], &params(320.0)), None);
    }

    #[test]
    fn does_not_claim_a_canvas_scale_for_an_aspect_dependent_cap() {
        let node = ResizeLimit::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(node.output_canvas_scale("out", &params(320.0)), None);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use half::f16;
    use manifold_gpu::{GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use crate::render_target::RenderTarget;

    fn upload_source(device: &manifold_gpu::GpuDevice) -> manifold_gpu::GpuTexture {
        let mut pixels = vec![f16::from_f32(0.0); 3 * 2 * 4];
        for y in 0..2 {
            for x in 0..3 {
                let index = (y * 3 + x) * 4;
                pixels[index] = f16::from_f32(x as f32);
                pixels[index + 3] = f16::from_f32(1.0);
            }
        }
        let texture = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 2,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "resize-limit-blob-v2-source",
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
        let mut encoder = device.create_encoder("resize-limit-blob-v2-readback");
        encoder.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("resize-limit readback buffer");
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

    #[test]
    fn blob_v2_standalone_bilinear_resampling_uses_the_full_source() {
        let device = crate::test_device();
        let source = upload_source(&device);
        let output = RenderTarget::new(
            &device,
            2,
            1,
            GpuTextureFormat::Rgba16Float,
            "resize-limit-blob-v2-output",
        );
        let pipeline = device.create_compute_pipeline(
            &standalone_for_spec::<ResizeLimit>().expect("resize_limit standalone codegen"),
            ENTRY,
            "node.resize_limit.blob-v2-gpu-test",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let uniforms = ResizeLimitUniforms {
            max_dim: 2,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        let mut encoder = device.create_encoder("resize-limit-blob-v2-dispatch");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &output.texture,
                },
            ],
            [1, 1, 1],
            "node.resize_limit.blob-v2-gpu-test",
        );
        encoder.commit_and_wait_completed();

        let pixels = readback(&device, &output.texture);
        assert_eq!(pixels.len(), 2);
        assert!(
            (pixels[0][0] - 0.25).abs() < 0.05,
            "left bilinear sample: {:?}",
            pixels[0]
        );
        assert!(
            (pixels[1][0] - 1.75).abs() < 0.05,
            "right bilinear sample: {:?}",
            pixels[1]
        );
        for pixel in pixels {
            assert!(
                (pixel[3] - 1.0).abs() < 0.01,
                "alpha should survive resize: {pixel:?}"
            );
        }
    }
}
