//! `node.region_mask` — rasterize observed region labels into a coverage mask.
//!
//! Labels are categorical data. The shader therefore uses integer texel loads
//! and rounds the uploaded `label / 255` red channel before comparing it with
//! the track records. Track boxes are never used for coverage; only observed
//! labels select pixels.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuTextureFormat};

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const REGION_MASK_SELECTIONS: &[&str] = &["All", "Largest"];

fn read_selection(ctx: &EffectNodeContext<'_, '_>) -> u32 {
    let value = match ctx.inputs.scalar("selection") {
        Some(ParamValue::Float(value)) => value,
        _ => match ctx.params.get("selection") {
            Some(ParamValue::Enum(value)) => *value as f32,
            Some(ParamValue::Float(value)) => *value,
            _ => 0.0,
        },
    };
    if value.is_finite() {
        value.round().clamp(0.0, 1.0) as u32
    } else {
        0
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RegionMaskUniforms {
    selection: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: RegionMask,
    type_id: "node.region_mask",
    purpose: "Rasterize a categorical region-label texture into a grayscale coverage mask using observed tracker records. All unions observed labels; Largest chooses the observed track with greatest measured area, ties by lowest track ID.",
    inputs: {
        labels: Texture2D required,
        tracks: Channels[ID: U32, LABEL: U32, OBSERVED: U32, AGE: F32, X: F32, Y: F32, WIDTH: F32, HEIGHT: F32, CX: F32, CY: F32, VX: F32, VY: F32, AREA: F32, PAD0: U32, PAD1: U32, PAD2: U32] required,
        selection: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("selection"),
            label: "Selection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: REGION_MASK_SELECTIONS,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Labels are categorical and are read with exact textureLoad plus round(red * 255); never filter or bilinear-sample them. Only OBSERVED tracks contribute. All unions observed labels, while Largest chooses measured AREA with lowest ID as the tie-break. Feed the result into node.mask_extrema and the existing Gaussian/invert/amount mask stages.",
    examples: [],
    picker: { label: "Region Mask", category: Atom },
    summary: "Turns observed tracked region labels into a pixel-accurate mask that preserves holes.",
    category: Mask,
    role: Filter,
    aliases: ["region mask", "blob mask", "label mask", "tracked mask"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/region_mask_body.wgsl"),
    input_access: [GatherTexel, BufferIndex],
}

impl Primitive for RegionMask {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        input_dims
            .iter()
            .find(|(name, _)| *name == "labels")
            .map(|(_, dims)| *dims)
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "out").then_some(GpuTextureFormat::Rgba16Float)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(labels) = ctx.inputs.texture_2d("labels") else {
            return;
        };
        let Some(tracks) = ctx.inputs.array("tracks") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if labels.width == 0
            || labels.height == 0
            || labels.width != out.width
            || labels.height != out.height
        {
            return;
        }

        let selection = read_selection(ctx);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = RegionMaskUniforms {
            selection,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
                    texture: labels,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tracks,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [out.width.div_ceil(16), out.height.div_ceil(16), 1],
            "node.region_mask",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::ports::{
        ArrayType, ChannelElementType, ChannelSpec, MatchMode, PortType, ScalarType,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn blob_v2_region_mask_declares_labels_tracks_and_selection() {
        use crate::node_graph::channel_names::well_known;

        let expected = ArrayType::of_channels(
            &[
                ChannelSpec {
                    name: well_known::ID,
                    ty: ChannelElementType::U32,
                },
                ChannelSpec {
                    name: well_known::LABEL,
                    ty: ChannelElementType::U32,
                },
                ChannelSpec {
                    name: well_known::OBSERVED,
                    ty: ChannelElementType::U32,
                },
                ChannelSpec {
                    name: well_known::AGE,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::X,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::Y,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::WIDTH,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::HEIGHT,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::CX,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::CY,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::VX,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::VY,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::AREA,
                    ty: ChannelElementType::F32,
                },
                ChannelSpec {
                    name: well_known::PAD0,
                    ty: ChannelElementType::U32,
                },
                ChannelSpec {
                    name: well_known::PAD1,
                    ty: ChannelElementType::U32,
                },
                ChannelSpec {
                    name: well_known::PAD2,
                    ty: ChannelElementType::U32,
                },
            ],
            MatchMode::Exact,
        );
        assert_eq!(RegionMask::TYPE_ID, "node.region_mask");
        assert_eq!(RegionMask::INPUTS.len(), 3);
        assert_eq!(RegionMask::INPUTS[0].ty, PortType::Texture2D);
        assert!(RegionMask::INPUTS[0].required);
        assert_eq!(RegionMask::INPUTS[1].ty, PortType::Array(expected));
        assert!(RegionMask::INPUTS[1].required);
        assert_eq!(RegionMask::INPUTS[2].ty, PortType::Scalar(ScalarType::F32));
        assert!(!RegionMask::INPUTS[2].required);
        assert_eq!(RegionMask::OUTPUTS.len(), 1);
        assert_eq!(RegionMask::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(RegionMask::PARAMS[0].enum_values, REGION_MASK_SELECTIONS);
    }

    #[test]
    fn blob_v2_region_mask_has_analysis_resolution_and_float_output() {
        let node = RegionMask::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(
            node.output_format("out"),
            Some(GpuTextureFormat::Rgba16Float)
        );
        assert_eq!(
            node.fusion_kind(),
            crate::node_graph::freeze::classify::FusionKind::Pointwise
        );
        assert_eq!(REGION_MASK_SELECTIONS, &["All", "Largest"]);
    }

    #[test]
    fn blob_v2_region_mask_selection_clamps_nonfinite_values() {
        let mut params = ahash::AHashMap::default();
        params.insert(Cow::Borrowed("selection"), ParamValue::Enum(99));
        assert_eq!(read_selection_from_params(&params), 1);
        params.insert(Cow::Borrowed("selection"), ParamValue::Float(f32::NAN));
        assert_eq!(read_selection_from_params(&params), 0);
    }

    fn read_selection_from_params(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
        let value = match params.get("selection") {
            Some(ParamValue::Enum(value)) => *value as f32,
            Some(ParamValue::Float(value)) => *value,
            _ => 0.0,
        };
        if value.is_finite() {
            value.round().clamp(0.0, 1.0) as u32
        } else {
            0
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::region_types::TrackRecord as Track;
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::render_target::RenderTarget;
    use half::f16;
    use manifold_gpu::{GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    fn upload_labels(
        device: &manifold_gpu::GpuDevice,
        values: &[u8],
        width: u32,
        height: u32,
    ) -> manifold_gpu::GpuTexture {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for (index, &label) in values.iter().enumerate() {
            pixels[index * 4] = label;
            pixels[index * 4 + 1] = label;
            pixels[index * 4 + 2] = label;
            pixels[index * 4 + 3] = 255;
        }
        let texture = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "region-mask-labels",
            mip_levels: 1,
        });
        device.upload_texture(&texture, &pixels);
        texture
    }

    fn upload_tracks(
        device: &manifold_gpu::GpuDevice,
        tracks: &[Track; 32],
    ) -> manifold_gpu::GpuBuffer {
        let buffer = device.create_buffer_shared(std::mem::size_of_val(tracks) as u64);
        let ptr = buffer
            .mapped_ptr()
            .expect("track fixture buffer should map");
        unsafe {
            std::ptr::copy_nonoverlapping(
                tracks.as_ptr().cast::<u8>(),
                ptr.cast::<u8>(),
                std::mem::size_of_val(tracks),
            );
        }
        buffer
    }

    fn readback(
        device: &manifold_gpu::GpuDevice,
        texture: &manifold_gpu::GpuTexture,
    ) -> Vec<[f32; 4]> {
        let bytes_per_row = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
        let mut encoder = device.create_encoder("region-mask-readback");
        encoder.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("region mask readback");
        let values: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        values
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

    fn dispatch_shader(
        device: &manifold_gpu::GpuDevice,
        labels: &manifold_gpu::GpuTexture,
        tracks: &manifold_gpu::GpuBuffer,
        selection: u32,
        shader: &str,
        label: &str,
    ) -> Vec<[f32; 4]> {
        let output = RenderTarget::new(
            device,
            labels.width,
            labels.height,
            GpuTextureFormat::Rgba16Float,
            "region-mask-output",
        );
        let uniforms = RegionMaskUniforms {
            selection,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let pipeline = device.create_compute_pipeline(shader, ENTRY, label);
        let mut encoder = device.create_encoder(label);
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: labels,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tracks,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &output.texture,
                },
            ],
            [labels.width.div_ceil(16), labels.height.div_ceil(16), 1],
            label,
        );
        encoder.commit_and_wait_completed();
        readback(device, &output.texture)
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        labels: &manifold_gpu::GpuTexture,
        tracks: &manifold_gpu::GpuBuffer,
        selection: u32,
    ) -> Vec<[f32; 4]> {
        let shader = standalone_for_spec::<RegionMask>().expect("region mask standalone codegen");
        dispatch_shader(
            device,
            labels,
            tracks,
            selection,
            &shader,
            "node.region_mask.blob-v2-standalone",
        )
    }

    fn fused_shader() -> String {
        generate_fused(&FusionRegion {
            nodes: vec![RegionNode {
                node_id: NodeInstanceId(0),
                fusion_kind: FusionKind::Pointwise,
                body: RegionMask::WGSL_BODY.expect("region mask body"),
                params: RegionMask::PARAMS,
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: RegionMask::INPUT_ACCESS.to_vec(),
                node_inputs: RegionMask::INPUTS,
                node_outputs: RegionMask::OUTPUTS,
                node_includes: RegionMask::WGSL_INCLUDES,
                derived_uniforms: RegionMask::DERIVED_UNIFORMS,
                type_id: RegionMask::TYPE_ID.to_owned(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: RegionMask::STENCIL_FETCH,
                quantize_f16: false,
            }],
            num_external_inputs: 2,
            outputs: vec![(NodeInstanceId(0), "out".to_owned())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: vec![],
            sampled_externals: vec![],
            camera_externals: 0,
            output_capacity: None,
        })
        .expect("region mask fused codegen")
        .wgsl
    }

    #[test]
    fn blob_v2_mask_pixels() {
        let device = crate::test_device();
        let labels = upload_labels(&device, &[1, 1, 0, 32, 32, 0, 1, 0], 4, 2);
        let mut tracks = [Track::default(); 32];
        tracks[0] = Track {
            id: 7,
            label: 1,
            observed: 1,
            area: 0.2,
            ..Track::default()
        };
        tracks[1] = Track {
            id: 3,
            label: 32,
            observed: 1,
            area: 0.8,
            ..Track::default()
        };
        tracks[2] = Track {
            id: 1,
            label: 1,
            observed: 0,
            area: 1.0,
            ..Track::default()
        };
        let track_buffer = upload_tracks(&device, &tracks);
        let all = dispatch(&device, &labels, &track_buffer, 0);
        let largest = dispatch(&device, &labels, &track_buffer, 1);
        for (index, pixel) in all.iter().enumerate() {
            let expected = if [0, 1, 3, 4, 6].contains(&index) {
                1.0
            } else {
                0.0
            };
            assert!(
                (pixel[0] - expected).abs() < 0.01,
                "All pixel {index}: {pixel:?}"
            );
            assert!((pixel[1] - pixel[0]).abs() < 0.001 && (pixel[2] - pixel[0]).abs() < 0.001);
            assert!((pixel[3] - 1.0).abs() < 0.001);
        }
        for (index, pixel) in largest.iter().enumerate() {
            let expected = if [3, 4].contains(&index) { 1.0 } else { 0.0 };
            assert!(
                (pixel[0] - expected).abs() < 0.01,
                "Largest pixel {index}: {pixel:?}"
            );
            assert!((pixel[3] - 1.0).abs() < 0.001);
        }
    }

    #[test]
    fn blob_v2_mask_standalone_and_fused_match_fixture() {
        let device = crate::test_device();
        let labels = upload_labels(&device, &[1, 1, 0, 32, 32, 0, 1, 0], 4, 2);
        let mut tracks = [Track::default(); 32];
        tracks[0] = Track {
            id: 7,
            label: 1,
            observed: 1,
            area: 0.2,
            ..Track::default()
        };
        tracks[1] = Track {
            id: 3,
            label: 32,
            observed: 1,
            area: 0.8,
            ..Track::default()
        };
        tracks[2] = Track {
            id: 1,
            label: 1,
            observed: 0,
            area: 1.0,
            ..Track::default()
        };
        let track_buffer = upload_tracks(&device, &tracks);
        let standalone =
            standalone_for_spec::<RegionMask>().expect("region mask standalone codegen");
        let fused = fused_shader();

        for selection in [0, 1] {
            let standalone_pixels = dispatch_shader(
                &device,
                &labels,
                &track_buffer,
                selection,
                &standalone,
                "node.region_mask.parity-standalone",
            );
            let fused_pixels = dispatch_shader(
                &device,
                &labels,
                &track_buffer,
                selection,
                &fused,
                "node.region_mask.parity-fused",
            );
            for (index, (standalone, fused)) in
                standalone_pixels.iter().zip(&fused_pixels).enumerate()
            {
                let expected = if selection == 0 {
                    [0, 1, 3, 4, 6].contains(&index)
                } else {
                    [3, 4].contains(&index)
                };
                let expected = if expected { 1.0 } else { 0.0 };
                for channel in 0..3 {
                    assert!((standalone[channel] - expected).abs() < 0.01);
                    assert!((fused[channel] - expected).abs() < 0.01);
                    assert!(
                        (standalone[channel] - fused[channel]).abs() < 0.002,
                        "selection={selection} pixel={index} channel={channel}: standalone={} fused={}",
                        standalone[channel],
                        fused[channel]
                    );
                }
                assert!((standalone[3] - 1.0).abs() < 0.001);
                assert!((fused[3] - 1.0).abs() < 0.001);
            }
        }
    }
}
