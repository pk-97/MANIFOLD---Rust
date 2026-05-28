//! `node.strobe` — pixel-exact replacement for legacy
//! Originally `StrobeFX`. Eleventh §6.1
//! migration; fused composite.
//!
//! Beat-synced square wave flash with three modes (Opacity → black,
//! White → white, Gain → 3× boost when on). The primitive surfaces
//! `rate` as a **note-rate enum** (the user-facing convention
//! musicians work in) — the index→strobes-per-beat conversion via
//! the [`NOTE_RATES`] table happens inside `run()` before the
//! uniform reaches the shader. This keeps the V2 outer card and the
//! per-node editor consistent: both show the same note-rate
//! selector, neither surfaces a hidden conversion.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Note-rate selector labels (UI surface) — indices into
/// [`NOTE_RATE_VALUES`]. The two slices must stay length-aligned.
pub const NOTE_RATE_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64",
];

/// Strobes-per-beat values indexed by the corresponding entry in
/// [`NOTE_RATE_LABELS`]. Mirrors the legacy `StrobeFX::NOTE_RATES`
/// table bit-for-bit. Pure data — kept `pub` for parity tests.
pub const NOTE_RATE_VALUES: [f32; 10] =
    [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0];

/// Deprecated alias preserved for transitional callers. Same data as
/// [`NOTE_RATE_VALUES`].
pub const NOTE_RATES: [f32; 10] = NOTE_RATE_VALUES;

crate::primitive! {
    name: Strobe,
    type_id: "node.strobe",
    purpose: "Beat-synced square wave flash. Three modes: Opacity (flash to black), White (flash to white), Gain (3× brightness when on). Rate is a musical note division.",
    inputs: {
        in: Texture2D required,
        beat: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "amount",
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rate",
            label: "Rate",
            ty: ParamType::Enum,
            default: ParamValue::Enum(6), // index of "1/16"
            range: Some((0.0, (NOTE_RATE_LABELS.len() - 1) as f32)),
            enum_values: NOTE_RATE_LABELS,
        },
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Opacity", "White", "Gain"],
        },
        ParamDef {
            name: "beat",
            label: "Beat",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
    ],
    composition_notes: "Fused composite — atomic BeatGate + Mix would round through fp16 between passes and break parity. `rate` is a note-rate enum; the primitive converts each index to strobes-per-beat via the internal NOTE_RATE_VALUES table before the shader sees it.",
    examples: ["preset.effect.strobe"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrobeUniforms {
    amount: f32,
    rate: f32,
    mode: u32,
    beat: f32,
}

impl Primitive for Strobe {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        // `rate` is a note-rate enum index — convert to the
        // strobes-per-beat float the shader actually needs. The
        // primitive accepts `Enum` (the declared shape, used in
        // production via the binding's `EnumRound` convert) or `Int`
        // (an alternate integral form). Float / out-of-range values
        // clamp to the table bounds.
        let rate_idx = match ctx.params.get("rate") {
            Some(ParamValue::Enum(v)) => (*v as usize).min(NOTE_RATE_VALUES.len() - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as usize).min(NOTE_RATE_VALUES.len() - 1)
            }
            _ => 6, // matches the ParamDef default (index of "1/16")
        };
        let rate = NOTE_RATE_VALUES[rate_idx];
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 0,
        };
        // Port-shadows-param: wire wins when present, param is the
        // fallback. Lets a preset wire `system.generator_input.beat`
        // into this port — replaces the hardcoded
        // `apply_ctx_params_at` injection from the chain runner.
        let beat = match ctx.inputs.scalar("beat") {
            Some(ParamValue::Float(f)) => f,
            _ => read_f32(ctx, "beat", 0.0),
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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/strobe.wgsl"),
                "cs_main",
                "node.strobe",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = StrobeUniforms {
            amount,
            rate,
            mode,
            beat,
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
            "node.strobe",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
