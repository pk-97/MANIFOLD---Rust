//! `node.flash` — modulate an image's brightness by a scalar `amount`
//! in one of three modes:
//!   - **Opacity** (0): `col * (1 - amount)` — flash toward black.
//!   - **White**   (1): `mix(col, white, amount)` — flash toward white.
//!   - **Gain**    (2): `col * mix(1, 3, amount)` — brighten (3× at 1).
//!
//! The brightness-apply half of Strobe, with the gate computation
//! factored out: wire `node.beat_gate` into `amount` for a beat-synced
//! strobe, or wire an LFO / audio envelope / MIDI for any other pulsing
//! flash. A scalar-driven, mode-switched brightness modulator — the same
//! shape as `node.mix` (8 blend modes) or `node.math` (13 ops): one
//! composable atom that branches on a `mode` param.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Display labels for the `mode` enum. Index = enum value, matching the
/// legacy Strobe mode discriminants (0=Opacity, 1=White, 2=Gain).
pub const FLASH_MODES: &[&str] = &["Opacity", "White", "Gain"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FlashUniforms {
    amount: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: Flash,
    type_id: "node.flash",
    purpose: "Modulate image brightness by a scalar `amount` in one of three modes: Opacity (col*(1-amount), toward black), White (mix toward white), Gain (col*mix(1,3,amount), brighten 3x). The brightness-apply half of Strobe with the gate factored out — wire node.beat_gate into `amount` for a beat-synced strobe, or an LFO/audio/MIDI for any pulsing flash. `amount` port-shadows the param for live modulation.",
    inputs: {
        in: Texture2D required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (FLASH_MODES.len() - 1) as f32)),
            enum_values: FLASH_MODES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Strobe = node.beat_gate (rate, amount, duty) → node.flash (mode). beat_gate already multiplies by its own `amount`, so the effect's strobe depth lives on the gate and flash just applies the resulting scalar. amount=0 passes through unchanged in all modes. Wire wins over the inline param.",
    examples: ["preset.effect.strobe"],
    picker: { label: "Flash", category: Atom },
    summary: "Pulses the whole image brighter, toward white, or toward black from a single amount. Wire a beat gate or envelope into the amount for strobes and hits.",
    category: Stylize,
    role: Filter,
    aliases: ["flash", "strobe", "pulse", "hit"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/flash_body.wgsl"),
}

impl Primitive for Flash {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.inputs.scalar("amount") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("amount") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.flash standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flash",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FlashUniforms {
            amount,
            mode,
            _pad0: 0.0,
            _pad1: 0.0,
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
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.flash",
        );
    }
}
