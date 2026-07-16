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
            // fuses; shaders/set_alpha.wgsl is the parity oracle only.
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Generated-vs-hand parity — the machine check that proves the atom
    //! is genuinely on the fusable codegen path (ADDING_PRIMITIVES.md
    //! "The codegen path is mandatory"). Harness pattern follows
    //! `rotate_2d.rs`.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::SetAlpha;
    use crate::render_target::RenderTarget;

    fn upload_rgba16f(device: &GpuDevice, w: u32, h: u32, px: &[f16]) -> GpuTexture {
        assert_eq!(px.len(), (w * h * 4) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "set-alpha-in",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("set-alpha-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    fn dispatch(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        src: &GpuTexture,
        w: u32,
        h: u32,
        alpha: f32,
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "set-alpha-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let uniforms: [f32; 4] = [alpha, 0.0, 0.0, 0.0];
        let mut enc = device.create_encoder("set-alpha-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "set-alpha-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    #[test]
    fn generated_set_alpha_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (8u32, 4u32);
        // Fixture: HDR rgb spread, varied (soon-to-be-overwritten) alpha.
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let f = i as f32;
            px[i * 4] = f16::from_f32(f * 0.13);
            px[i * 4 + 1] = f16::from_f32(4.0 - f * 0.1);
            px[i * 4 + 2] = f16::from_f32(f * f * 0.01);
            px[i * 4 + 3] = f16::from_f32(f * 0.03);
        }
        let src = upload_rgba16f(&device, w, h, &px);

        let hand = device.create_compute_pipeline(
            include_str!("shaders/set_alpha.wgsl"),
            "cs_main",
            "set-alpha-hand",
        );
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SetAlpha>()
            .expect("set_alpha codegen");
        let generated = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "set-alpha-generated",
        );

        for alpha in [1.0f32, 0.25, 0.0] {
            let from_hand = dispatch(&device, &hand, &src, w, h, alpha);
            let from_gen = dispatch(&device, &generated, &src, w, h, alpha);
            for (i, (hp, gp)) in from_hand.iter().zip(&from_gen).enumerate() {
                for c in 0..4 {
                    assert_eq!(
                        hp[c].to_bits(),
                        gp[c].to_bits(),
                        "texel {i} channel {c} at alpha={alpha}: hand={} gen={}",
                        hp[c],
                        gp[c],
                    );
                }
                assert!(
                    (gp[3] - alpha).abs() < 1e-3,
                    "texel {i}: alpha {} != {alpha}",
                    gp[3],
                );
            }
        }
    }
}
