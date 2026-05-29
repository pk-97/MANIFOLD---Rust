//! `node.glitch_displace` — the motion + accent field of a digital
//! glitch, as a pure generator. Emits two textures:
//!   - `uv` (RG): the per-pixel sampling UV after block displacement +
//!     scanline jitter — feed into `node.remap` to warp the source.
//!   - `mask` (R): the per-block invert accent mask (0 or 1) — feed
//!     into a downstream masked invert (`node.invert` + `node.masked_mix`).
//!
//! The block-displace + scanline-jitter math is verbatim from the fused
//! `node.glitch` / `fx_glitch`; the chromatic split and per-block invert
//! it fused into one pass are now separate graph nodes
//! (`chromatic_aberration`, `invert` + `masked_mix`). Reusable as a
//! datamosh / VHS displacement source for any source texture. `time`
//! drives the random hash — wired or read from `FrameTime.seconds`.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlitchDisplaceUniforms {
    amount: f32,
    block_size: f32,
    scanline: f32,
    speed: f32,
    time: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: GlitchDisplace,
    type_id: "node.glitch_displace",
    purpose: "Generator for the motion + accent field of a digital glitch. Emits `uv` (RG = per-pixel sampling UV after block displacement + scanline jitter — feed into node.remap) and `mask` (R = per-block invert accent, 0/1 — feed a masked invert). Block-displace + scanline-jitter math verbatim from the fused glitch; the chromatic split + per-block invert compose downstream. Reusable datamosh/VHS displacement source. `time` drives the hash (wired or FrameTime.seconds); `amount` port-shadows the param.",
    inputs: {
        amount: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        uv: Texture2D,
        mask: Texture2D,
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
    ],
    composition_notes: "Glitch = glitch_displace → remap(source, uv) → chromatic_aberration → (invert + masked_mix on `mask`) → mix(source, …, amount). amount scales displacement magnitude AND the invert/displace thresholds; wire the same scalar into the final node.mix amount so the master knob fades the whole effect. block_size is clamped to >= 4 in-shader. Sampling UV can leave [0,1] by up to ~0.15 — remap with Clamp wrap matches the legacy edge-clamp sampler.",
    examples: ["preset.effect.glitch"],
    picker: { label: "Glitch Displace", category: Atom },
}

impl Primitive for GlitchDisplace {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.inputs.scalar("amount") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("amount") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let read = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let block_size = read("block_size", 16.0).max(4.0);
        let scanline = read("scanline", 0.3);
        let speed = read("speed", 2.0);
        // Wire wins; else the playback clock's seconds — same value the
        // runtime injects into system.generator_input.time.
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => ctx.time.seconds.0 as f32,
        };

        let Some(uv_out) = ctx.outputs.texture_2d("uv") else {
            return;
        };
        let (width, height) = (uv_out.width, uv_out.height);
        if width == 0 || height == 0 {
            return;
        }
        let Some(mask_out) = ctx.outputs.texture_2d("mask") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/glitch_displace.wgsl"),
                "cs_main",
                "node.glitch_displace",
            )
        });

        let uniforms = GlitchDisplaceUniforms {
            amount,
            block_size,
            scanline,
            speed,
            time,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
                    texture: uv_out,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: mask_out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.glitch_displace",
        );
    }
}
