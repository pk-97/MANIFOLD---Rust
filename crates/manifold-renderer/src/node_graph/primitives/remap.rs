//! `node.remap` — resample `source` at the per-pixel UV coordinates
//! carried in `uv_field`'s R/G channels. TouchDesigner's Remap TOP /
//! Unreal's Texture-Sample-by-UV / Blender's Mapping → Image Texture.
//!
//! `out(p) = source(wrap(uv_field(p).rg))`. The generic UV-warp atom:
//! pair with any coordinate-field producer (polar fold, axis fold,
//! offset field, optical flow) to express kaleidoscope, mirror,
//! edge-stretch, chromatic split, lens distortion, twirl as a visible
//! `coordinate-math → remap → blend` graph instead of a bespoke
//! single-effect shader. `wrap` sets the out-of-[0,1] sampling policy.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const REMAP_WRAP_MODES: &[&str] = &["Clamp", "Repeat", "Mirror"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RemapUniforms {
    wrap: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: Remap,
    type_id: "node.remap",
    purpose: "Resample `source` at the per-pixel UV coordinates in `uv_field`'s R/G channels (TouchDesigner's Remap TOP). out(p) = source(uv_field(p).rg). `wrap` picks the out-of-[0,1] policy (Clamp / Repeat / Mirror). The generic UV-warp atom — pair with a coordinate-field producer (polar_field, centered_uv, scale_offset_texture, optical flow) to build kaleidoscope / mirror / edge-stretch / chromatic split / lens distortion as a visible `coordinate-math → remap → blend` graph rather than a bespoke single-effect shader.",
    inputs: {
        source: Texture2D required,
        uv_field: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "wrap",
            label: "Wrap",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: REMAP_WRAP_MODES,
        },
    ],
    composition_notes: "uv_field carries absolute target UVs in R (u) and G (v), [0,1] over the canvas. Build it from centered_uv / polar_field / scale_offset_texture / field_combine chains, or feed a flow field. Wrap is applied per-component in the shader (Clamp = saturate, Repeat = fract, Mirror = triangle), so no sampler-state juggling. Pure resample — blend the result against the original with node.mix downstream when an effect wants a wet/dry.",
    examples: ["preset.effect.kaleidoscope"],
    picker: { label: "Remap", category: Atom },
}

impl Primitive for Remap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let wrap = match ctx.params.get("wrap") {
            Some(ParamValue::Enum(e)) => *e,
            _ => 0,
        };

        let Some(src_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(uv_tex) = ctx.inputs.texture_2d("uv_field") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/remap.wgsl"),
                "cs_main",
                "node.remap",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = RemapUniforms {
            wrap,
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
                    texture: src_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: uv_tex,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.remap",
        );
    }
}
