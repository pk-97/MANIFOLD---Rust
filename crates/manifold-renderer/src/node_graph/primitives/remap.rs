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

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const REMAP_WRAP_MODES: &[&str] = &["Clamp", "Repeat", "Mirror"];
pub const REMAP_FIELD_MODES: &[&str] = &["Absolute", "Relative"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RemapUniforms {
    wrap: u32,
    mode: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: Remap,
    type_id: "node.remap",
    purpose: "Resample `source` at the per-pixel UV coordinates in `uv_field`'s R/G channels (TouchDesigner's Remap TOP). `mode` = Absolute: out(p) = source(uv_field(p).rg); Relative: out(p) = source(p + uv_field(p).rg) — treat the field as a UV *offset* so it sums cleanly with other offset fields. `wrap` picks the out-of-[0,1] policy (Clamp / Repeat / Mirror). The generic UV-warp atom — pair with a coordinate-field producer (polar_field, centered_uv, scale_offset_texture, block_displace_field, optical flow) to build kaleidoscope / mirror / edge-stretch / chromatic split / glitch displace / lens distortion as a visible `coordinate-math → remap → blend` graph rather than a bespoke single-effect shader.",
    inputs: {
        source: Texture2D required,
        uv_field: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("wrap"),
            label: "Wrap",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: REMAP_WRAP_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Field Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: REMAP_FIELD_MODES,
        },
    ],
    depth_rule: Warp,
    composition_notes: "Absolute (default): uv_field carries target UVs in R (u) / G (v), [0,1] over the canvas — build it from centered_uv / polar_field / scale_offset_texture / field_combine chains, or feed a flow field. Relative: uv_field carries a signed UV *offset* added to each pixel's own coordinate — this is how displacement fields (block_displace_field, scanline_jitter_field) compose: sum them with node.mix(Add) then remap once. Wrap is applied per-component in the shader (Clamp = saturate, Repeat = fract, Mirror = triangle), so no sampler-state juggling. Pure resample — blend the result against the original with node.mix downstream when an effect wants a wet/dry.",
    examples: ["preset.effect.kaleidoscope"],
    picker: { label: "Remap", category: Atom },
    summary: "Resamples the image through a coordinate map, reading each pixel from wherever the map points. This is the node that turns a Mirror, Kaleidoscope, or any coordinate field into an actual warped picture.",
    category: DistortAndWarp,
    role: Filter,
    aliases: ["remap", "uv map", "displace", "Remap TOP"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/remap_body.wgsl"),
    input_access: [Gather, Coincident],
}

impl Primitive for Remap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let wrap = match ctx.params.get("wrap") {
            Some(ParamValue::Enum(e)) => *e,
            _ => 0,
        };
        let mode = match ctx.params.get("mode") {
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
            // Single-source: the generated kernel binds the two textures
            // consecutively (source, uv_field) then the sampler, so the binding
            // set below is reordered to match (the hand remap.wgsl interleaved
            // the sampler between them). `source` is a Gather input — the body
            // samples it at a computed coord. remap.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.remap standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.remap",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = RemapUniforms {
            wrap,
            mode,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: uv_tex,
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
            "node.remap",
        );
    }
}
