//! `node.absolute_value` — per-pixel `abs(input.rgb)`, alpha pass-through.
//! Zero parameters.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: AbsTexture,
    type_id: "node.absolute_value",
    purpose: "Per-pixel abs(input.rgb). Alpha passes through. Useful after node.sin_texture (abs(sin(x)) is a humped positive-only pattern with twice the spatial frequency) or after scale_offset_texture to fold a signed field into a 'V' curve.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: Inherit,
    composition_notes: "Maps [-a, a] → [0, a]. Common downstream of node.scale_offset_image (which recovers signed noise from [0, 1] generators) and node.sin_texture / cos_texture.",
    examples: [],
    picker: { label: "Absolute Value", category: Atom },
    summary: "Flips every negative value positive, leaving positives alone. Handy after a signed field or a sine to fold it into a V shape.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["absolute value", "abs texture", "abs", "magnitude"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/abs_texture_body.wgsl"),
}

impl Primitive for AbsTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: paramless atom — the generated kernel binds no
            // uniform, so its textures start at binding 0, matching the bindings
            // below. abs_texture.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.absolute_value standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.absolute_value",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.absolute_value",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn abs_texture_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(AbsTexture::TYPE_ID, "node.absolute_value");
        assert_eq!(AbsTexture::INPUTS.len(), 1);
        assert_eq!(AbsTexture::OUTPUTS.len(), 1);
        assert_eq!(AbsTexture::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn abs_texture_has_no_params() {
        assert!(AbsTexture::PARAMS.is_empty());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = AbsTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.absolute_value");
    }
}
