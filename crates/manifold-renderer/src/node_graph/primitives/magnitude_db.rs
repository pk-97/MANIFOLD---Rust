//! `node.magnitude_db` — convert a linear magnitude texture to decibels.
//!
//! This is intentionally a single pointwise codegen atom. Palette mapping,
//! clamping, and contrast stay separate graph nodes so the conversion is
//! reusable for meters, spectrograms, and other magnitude fields.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MagnitudeDbUniforms {
    floor_db: f32,
    reference: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: MagnitudeDb,
    type_id: "node.magnitude_db",
    purpose: "Convert a non-negative linear magnitude Texture2D to decibels with `20*log10(magnitude / reference)`, clamped at `floor_db`. Alpha passes through. Keep palette mapping in separate scale, clamp, and gradient/LUT nodes.",
    inputs: {
        in: Texture2D required,
        floor_db: ScalarF32 optional,
        reference: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("floor_db"),
            label: "Floor (dB)",
            ty: ParamType::Float,
            default: ParamValue::Float(-60.0),
            range: Some((-160.0, 0.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("reference"),
            label: "Reference",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.000001, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "The output is in dB, not normalized: `20*log10(max(magnitude, 1e-6) / reference)` clamped to `floor_db`. For a -60..0 palette, follow with node.scale_offset_image (scale 0.0166667, offset 1), node.clamp (0..1), and node.gradient or node.color_lut. Scalar inputs shadow their params for live control.",
    examples: ["preset.generator.spectrogram"],
    picker: { label: "Magnitude → dB", category: Atom },
    summary: "Converts a brightness or spectrum magnitude image into a bounded decibel field for meters and palettes.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["magnitude db", "decibels", "dB", "log magnitude"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/magnitude_db_body.wgsl"),
}

impl Primitive for MagnitudeDb {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let floor_db = ctx.scalar_or_param("floor_db", -60.0);
        let reference = ctx.scalar_or_param("reference", 1.0).max(1e-6);
        let Some(input) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(output) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if output.width == 0 || output.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = MagnitudeDbUniforms {
            floor_db,
            reference,
            _pad0: 0,
            _pad1: 0,
        };
        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[input],
            Some(sampler),
            output,
            "node.magnitude_db",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn registers_as_magnitude_db() {
        let node = MagnitudeDb::new();
        let node: &dyn EffectNode = &node;
        assert_eq!(node.type_id().as_str(), "node.magnitude_db");
    }
}
