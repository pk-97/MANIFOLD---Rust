//! `node.slice_volume` — sample a `Texture3D` at a fixed Z
//! slice (with optional UV transform) to produce a `Texture2D`.
//!
//! New WGSL — the existing `mri_slice_compute.wgsl` samples
//! pre-loaded 2D slice textures, not a 3D volume. This primitive
//! exists for cases where the upstream actually has a Texture3D
//! (volumetric fluid density, procedurally generated volumes,
//! future Phase D primitives) and the user wants to peel a 2D
//! display slice out of it.
//!
//! `slice_z` is in [0, 1]. UV scale + center re-frame the slice
//! into the output texture's dimensions.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleVolumeUniforms {
    slice_z: f32,
    uv_scale: f32,
    center_x: f32,
    center_y: f32,
}

manifold_node_engine::primitive! {
    name: SampleVolume2D,
    type_id: "node.slice_volume",
    purpose: "Sample a Texture3D at a fixed Z slice to produce a Texture2D. UV scale + center re-frame the slice into the output texture. Drives \"peel a 2D plane out of a volume\" use cases: MRI-style display, debug visualisation of volumetric fluid density, or any future Phase D volume rendering.",
    inputs: {
        in: Texture3D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("slice_z"),
            label: "Slice Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_scale"),
            label: "UV Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.05, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("center_x"),
            label: "Center X Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("center_y"),
            label: "Center Y Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: crosses domains: input is Texture3D (out of the 2D depth channel's scope) but the output is a genuine 2D Texture2D slice with no 2D depth to inherit, so it originates a fresh field like a generator
    depth_rule: SourceHeight,
    composition_notes: "slice_z is clamped to [0, 1] in-shader; values outside the volume's Z range produce the boundary texel (sampler clamp). Bilinear filtering across X/Y/Z; the slice is interpolated between adjacent Z layers so smooth slice_z drives produce smooth animation. Output is Rgba16Float — the shader passes through whatever channels the volume has.",
    examples: [],
    picker: { label: "Slice Volume", category: Atom },
    summary: "Takes a flat slice through a 3D volume to get a normal 2D image. The way to look inside a fluid or density field.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["sample volume 2d", "sample volume", "slice", "3d to 2d"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_volume_2d_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for SampleVolume2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let slice_z = match ctx.params.get("slice_z") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 0.5,
        };
        let uv_scale = ctx.param_f32("uv_scale", 1.0);

        let center_x = ctx.param_f32("center_x", 0.0);

        let center_y = ctx.param_f32("center_y", 0.0);

        let Some(volume) = ctx.inputs.texture_3d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SampleVolumeUniforms {
            slice_z,
            uv_scale,
            center_x,
            center_y,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[volume],
            Some(sampler),
            target,
            "node.slice_volume",
        );
    }
}
