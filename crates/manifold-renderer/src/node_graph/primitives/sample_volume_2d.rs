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

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleVolumeUniforms {
    slice_z: f32,
    uv_scale: f32,
    center_x: f32,
    center_y: f32,
}

crate::primitive! {
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
        let uv_scale = match ctx.params.get("uv_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let center_x = match ctx.params.get("center_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let center_y = match ctx.params.get("center_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // MIXED-DIM: 2D output (2D wrapper), 3D `in` volume gathered at a body-
            // computed slice coord. Generated kernel binds uniform(0)/tex_in(1)/
            // samp(2)/dst(3). sample_volume_2d.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.slice_volume standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.slice_volume",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SampleVolumeUniforms {
            slice_z,
            uv_scale,
            center_x,
            center_y,
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
                    texture: volume,
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
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.slice_volume",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn sample_volume_declares_texture_3d_in_and_texture_2d_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(SampleVolume2D::TYPE_ID, "node.slice_volume");
        assert_eq!(SampleVolume2D::INPUTS.len(), 1);
        assert_eq!(SampleVolume2D::INPUTS[0].name, "in");
        assert_eq!(SampleVolume2D::INPUTS[0].ty, PortType::Texture3D);
        assert_eq!(SampleVolume2D::OUTPUTS.len(), 1);
        assert_eq!(SampleVolume2D::OUTPUTS[0].name, "out");
        assert_eq!(SampleVolume2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn sample_volume_has_slice_uv_center_params() {
        let names: Vec<&str> = SampleVolume2D::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["slice_z", "uv_scale", "center_x", "center_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SampleVolume2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.slice_volume");
    }
}
