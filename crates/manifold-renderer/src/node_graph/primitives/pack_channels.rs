//! `node.pack_rgba` — combine four texture inputs into one RGBA
//! texture by reading the R channel of each input into one channel of
//! the output.
//!
//! The fundamental "recompose after atomic per-channel processing"
//! atom. Use when a decomposition has computed each channel of a target
//! texture separately (one scalar field per chain) and needs to glue
//! them into a single RGBA for downstream consumers that expect packed
//! data (PBR material samplers, packed vector fields, HSL → RGB
//! recomposition).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: PackChannels,
    type_id: "node.pack_rgba",
    purpose: "Pack four single-channel textures into one RGBA output by reading the R channel of each input into the matching output channel. Optional inputs default to the constant `default_a` (0.0 for r/g/b, 1.0 for a). Use when an atomic decomposition has computed each channel separately and downstream consumers expect packed data.",
    inputs: {
        r: Texture2D optional,
        g: Texture2D optional,
        b: Texture2D optional,
        a: Texture2D optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("default_r"),
            label: "Default R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("default_g"),
            label: "Default G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("default_b"),
            label: "Default B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("default_a"),
            label: "Default A",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "Each input reads `.r` of the source — to pack a multi-channel input use `node.scale_offset_image` or `node.field_combine` upstream to project the desired channel onto R. When an input port is unwired the corresponding output channel takes the `default_*` value. All wired inputs must share dimensions; output matches.",
    examples: [],
    picker: { label: "Pack RGBA", category: Atom },
    summary: "Combines four single-channel images into one RGBA image, one image per colour channel. The opposite of pulling an image apart.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["pack rgba", "pack channels", "combine channels", "merge channels"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/pack_channels_body.wgsl"),
}

// Standalone-codegen uniform layout: PARAMS order (default_r..default_a) first,
// then the injected use_r/g/b/a flags — contiguous, unlike the hand uniform which
// put the use flags first and the defaults as a trailing vec4.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PackUniforms {
    default_r: f32,
    default_g: f32,
    default_b: f32,
    default_a: f32,
    use_r: u32,
    use_g: u32,
    use_b: u32,
    use_a: u32,
}

impl Primitive for PackChannels {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let default_r = match ctx.params.get("default_r") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let default_g = match ctx.params.get("default_g") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let default_b = match ctx.params.get("default_b") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let default_a = match ctx.params.get("default_a") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let r_tex = ctx.inputs.texture_2d("r");
        let g_tex = ctx.inputs.texture_2d("g");
        let b_tex = ctx.inputs.texture_2d("b");
        let a_tex = ctx.inputs.texture_2d("a");

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // 4-input Coincident with optional-input use-flags. Generated kernel
            // binds uniform(0)/r(1)/g(2)/b(3)/a(4)/samp(5)/dst(6); the body reads
            // each channel only when its injected use flag is set, else the default.
            // pack_channels.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.pack_rgba standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.pack_rgba",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = PackUniforms {
            default_r,
            default_g,
            default_b,
            default_a,
            use_r: r_tex.is_some() as u32,
            use_g: g_tex.is_some() as u32,
            use_b: b_tex.is_some() as u32,
            use_a: a_tex.is_some() as u32,
        };

        // The shader always binds 4 input texture slots; unwired inputs
        // bind to the output texture as a dummy (the shader gates them
        // off via use_* flags so the contents are never read).
        let r_bind = r_tex.unwrap_or(out_tex);
        let g_bind = g_tex.unwrap_or(out_tex);
        let b_bind = b_tex.unwrap_or(out_tex);
        let a_bind = a_tex.unwrap_or(out_tex);

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: r_bind,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: g_bind,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: b_bind,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: a_bind,
                },
                GpuBinding::Sampler {
                    binding: 5,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.pack_rgba",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_four_optional_texture_inputs() {
        use crate::node_graph::ports::PortType;
        assert_eq!(PackChannels::TYPE_ID, "node.pack_rgba");
        let names: Vec<&str> = PackChannels::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["r", "g", "b", "a"]);
        for input in PackChannels::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
            assert_eq!(input.ty, PortType::Texture2D);
        }
        assert_eq!(PackChannels::OUTPUTS.len(), 1);
        assert_eq!(PackChannels::OUTPUTS[0].name, "out");
    }

    #[test]
    fn registers_as_atom() {
        let prim = PackChannels::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.pack_rgba");
    }
}
