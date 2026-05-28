//! `node.glitch` — pixel-exact replacement for legacy
//! Originally `GlitchFX`. Thirteenth and
//! **final** §6.1 migration; fused composite.
//!
//! Block displacement + scanline jitter + RGB channel split +
//! per-block invert in one compute pass. The atomic decomposition
//! (Hash → BlockDisplace → Scanline → ChromaticOffset + per-block
//! invert) would round through fp16 between every pass and shatter
//! bit-exact parity, so the legacy single-pass shader ships as a
//! fused composite primitive. Fusion compiler will replace it later.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Glitch,
    type_id: "node.glitch",
    purpose: "Composite glitch: block displacement, scanline jitter, RGB channel split, and per-block invert. Time-driven; the `time` and `speed` inputs together set the rate at which the random hash advances.",
    inputs: {
        in: Texture2D required,
        time: ScalarF32 optional,
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
            name: "block_size",
            label: "Block Size",
            ty: ParamType::Float,
            default: ParamValue::Float(16.0),
            range: Some((4.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rgb_shift",
            label: "RGB Shift",
            ty: ParamType::Float,
            default: ParamValue::Float(0.01),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "scanline",
            label: "Scanline",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "speed",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "time",
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
    ],
    composition_notes: "Fused composite — last primitive to split when the fusion compiler lands. `time` is exposed as an input parameter rather than read from frame-time globals so the primitive can be exercised from non-post-process contexts (animation tests, generator outputs, etc.).",
    examples: ["preset.effect.glitch"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlitchUniforms {
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32,
    speed: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

impl Primitive for Glitch {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        // Legacy CPU-clamps block_size to >= 4 before uniform pack.
        let block_size = read_f32(ctx, "block_size", 16.0).max(4.0);
        let rgb_shift = read_f32(ctx, "rgb_shift", 0.01);
        let scanline = read_f32(ctx, "scanline", 0.3);
        let speed = read_f32(ctx, "speed", 2.0);
        // Port-shadows-param: wire wins when present, param is the
        // fallback. Lets a preset wire `system.generator_input.time`
        // into this port — same surface every other ctx-driven
        // primitive uses.
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => read_f32(ctx, "time", 0.0),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        let resolution_x = width as f32;
        let resolution_y = height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/glitch.wgsl"),
                "cs_main",
                "node.glitch",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = GlitchUniforms {
            amount,
            block_size,
            rgb_shift,
            scanline,
            speed,
            time,
            resolution_x,
            resolution_y,
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
            "node.glitch",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
