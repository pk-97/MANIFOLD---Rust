//! `node.set_alpha` — force the output alpha to a constant, RGB
//! pass-through.
//!
//! The display-stage opacity decision, made explicit. Manifold's
//! compositor blends premultiplied alpha, and `node.mix`'s standardized
//! alpha rule (`out.a = mix(a.a, b.a, amount)`) means an additive
//! feedback loop seeded from a black state carries alpha 0 forever —
//! the RGB accumulates light while the layer stays fully transparent
//! (the Lightning afterglow bug this atom was built for, 2026-07-16).
//! Generators that render HDR light on black end their display chain
//! opaque — `resolve_scatter`/`resolve_accumulator` bake `alpha = 1`
//! in-kernel; this atom is the same decision as a composable step for
//! chains that have no resolve stage.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SetAlphaUniforms {
    alpha: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: SetAlpha,
    type_id: "node.set_alpha",
    purpose: "Force the output alpha to a constant (default 1 = opaque), RGB pass-through. The explicit display-stage opacity decision for generator chains whose alpha has been consumed by blend semantics — e.g. an additive feedback afterglow loop, where node.mix's alpha rule locks the loop's alpha at its (black, transparent) initial state while the RGB accumulates light. Place at the end of the display chain, after the tone map. NOT for effects: effects must carry their input's alpha (the alpha-contract sweep enforces this); this atom is the deliberate exception for generator display termini, matching the baked alpha=1 in resolve_scatter / resolve_accumulator.",
    inputs: {
        in: Texture2D required,
        // Port-shadow: wire a scalar to animate layer opacity.
        alpha: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("alpha"),
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "out = vec4(in.rgb, alpha). Use at a generator's display terminus when the chain contains feedback/blend stages that zero the alpha channel. Effects should never need this — if an effect chain loses alpha, fix the blend, don't paint over it.",
    examples: ["Lightning"],
    picker: { label: "Set Alpha", category: Atom },
    summary: "Forces the image's alpha to a fixed opacity while leaving the colours untouched. Ends a generator chain whose blends have eaten the alpha channel.",
    category: Composite,
    role: Filter,
    aliases: ["set alpha", "opaque", "opacity", "force alpha", "alpha fill"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/set_alpha_body.wgsl"),
}

impl Primitive for SetAlpha {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let alpha = match ctx.inputs.scalar("alpha") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("alpha") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the
            // runtime kernel is generated from `wgsl_body` so the atom
            // fuses; shaders/set_alpha.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.set_alpha standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.set_alpha",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SetAlphaUniforms { alpha, _pad0: 0.0, _pad1: 0.0, _pad2: 0.0 };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.set_alpha",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_texture_in_optional_alpha_scalar_and_texture_out() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(SetAlpha::TYPE_ID, "node.set_alpha");
        let ins = SetAlpha::INPUTS;
        assert_eq!(ins.len(), 2);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "alpha");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(SetAlpha::OUTPUTS.len(), 1);
        assert_eq!(SetAlpha::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn alpha_param_defaults_opaque() {
        let p = SetAlpha::PARAMS.iter().find(|p| p.name == "alpha").unwrap();
        assert_eq!(p.default, ParamValue::Float(1.0));
        assert_eq!(p.range, Some((0.0, 1.0)));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SetAlpha::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.set_alpha");
    }
}

