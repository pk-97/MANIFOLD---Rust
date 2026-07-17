//! `node.scanline_jitter_field` — a per-row random horizontal-offset
//! field, as a pure generator. The VHS / horizontal-tearing building
//! block.
//!
//! Hashes each scanline row (animated by `time`) and emits one texture:
//!   - `offset` (R): a signed horizontal UV shift per row (G=B=0,
//!     A=1), gated so only a fraction of rows tear — feed it into a
//!     `node.remap` in **Relative** mode, alone or summed with other
//!     offset fields (e.g. `node.block_displace_field`) via
//!     `node.mix(Add)`.
//!
//! Split out of the old fused `node.glitch_displace`. `time` drives the
//! hash — wired or read from `FrameTime.seconds`.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanlineJitterUniforms {
    amount: f32,
    scanline: f32,
    speed: f32,
    motion: i32,
    bands: f32,
    time: f32,
    _pad0: f32,
    _pad1: f32,
}

/// `motion` selector values. Index into the `motion` enum param and the
/// shader's `motion` uniform. `Tear` (default) is the byte-identical legacy
/// VHS jolt; `Slide` is the smooth continuous per-band value-noise drift
/// (the Latent Space website mosh slide).
pub const SCANLINE_MOTION: &[&str] = &["Tear", "Slide"];

crate::primitive! {
    name: ScanlineJitterField,
    type_id: "node.scanline_jitter_field",
    purpose: "Generator for a per-row horizontal-offset field. `motion` picks the character: Tear (default) hashes each scanline row with a sine-hash on quantised time and gates by `scanline` so only a fraction of rows JOLT (the VHS / horizontal-tearing building block); Slide drives every band with smooth value noise on continuous time for an organic, ungated drift (the Latent Space website mosh slide). `bands` sets the band count: in Tear it has no effect (Tear is always per pixel row); in Slide, 0 = no rows (the field outputs zero, so a downstream flow/domain warp carries the motion without per-row slicing), and e.g. 36 = chunky strips. Emits `offset` (R = signed horizontal UV shift per row/band, G=B=0, A=1). Feed `offset` into node.remap (Relative mode) — alone or summed with node.block_displace_field's offset via node.mix(Add). `amount`/`speed` port-shadow their params; `time` is wired or read from FrameTime.seconds.",
    inputs: {
        amount: ScalarF32 optional,
        speed: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        offset: Texture2D,
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
            name: Cow::Borrowed("scanline"),
            label: "Scanline",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("motion"),
            label: "Motion",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (SCANLINE_MOTION.len() - 1) as f32)),
            enum_values: SCANLINE_MOTION,
        },
        ParamDef {
            name: Cow::Borrowed("bands"),
            label: "Bands",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        // Backing param for the `time` input (port-shadow); run() packs the
        // resolved FrameTime/wired value, so the default below is never live.
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Tear mode: offset.r is a signed horizontal shift in UV units (~±0.08 at amount=1), gated by step(1 - scanline*amount*0.3, row_hash) so `scanline` controls how many rows tear and `amount` scales both the count and the magnitude. Slide mode: offset.r = (value_noise(band, time*speed*0.065) - 0.5) * amount * 0.05 — ungated, every band drifts smoothly; band = floor(uv.y*bands). bands=0 emits a zero offset (the slide turns off — pair with a flow/domain warp so the image still moves, the Digital Drift recipe). speed=2 reproduces the website's 0.13 time scale; `scanline` is ignored. Sum the offset with node.block_displace_field's offset via node.mix(Add), then node.remap(mode=Relative, wrap=Clamp). G/B are 0 so a relative remap leaves the vertical axis untouched.",
    examples: ["preset.effect.glitch", "preset.effect.digital_drift"],
    picker: { label: "Scanline Jitter Field", category: Atom },
    summary: "Per-row horizontal offset for sideways glitch. Tear = gated VHS jolt; Slide = smooth organic per-band drift. Set Bands for chunky strips, feed it into Remap.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["scanline jitter", "vhs", "tearing", "glitch", "slide", "drift", "mosh", "datamosh"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/scanline_jitter_field_body.wgsl"),
}

impl Primitive for ScanlineJitterField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scalar_or_param = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let amount = scalar_or_param("amount", 0.0);
        let scanline = scalar_or_param("scanline", 0.3);
        let speed = scalar_or_param("speed", 2.0);
        let motion = match ctx.params.get("motion") {
            Some(ParamValue::Enum(n)) => *n as i32,
            Some(ParamValue::Float(f)) => *f as i32,
            _ => 0,
        };
        let bands = match ctx.params.get("bands") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => ctx.time.seconds.0 as f32,
        };

        let Some(offset_out) = ctx.outputs.texture_2d("offset") else {
            return;
        };
        let (width, height) = (offset_out.width, offset_out.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Source generator: uniform(0)/dst(1). `time` is a backing param the
            // body reads. scanline_jitter_field.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.scanline_jitter_field standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.scanline_jitter_field",
            )
        });

        let uniforms = ScanlineJitterUniforms {
            amount,
            scanline,
            speed,
            motion,
            bands,
            time,
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
                    texture: offset_out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.scanline_jitter_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_single_offset_output() {
        assert_eq!(ScanlineJitterField::TYPE_ID, "node.scanline_jitter_field");
        assert_eq!(ScanlineJitterField::OUTPUTS.len(), 1);
        assert_eq!(ScanlineJitterField::OUTPUTS[0].name, "offset");
        assert_eq!(ScanlineJitterField::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn amount_speed_time_are_optional_scalar_inputs() {
        let names: Vec<&str> = ScanlineJitterField::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["amount", "speed", "time"]);
        assert!(ScanlineJitterField::INPUTS.iter().all(|p| !p.required));
        assert!(
            ScanlineJitterField::INPUTS
                .iter()
                .all(|p| p.ty == PortType::Scalar(ScalarType::F32))
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScanlineJitterField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scanline_jitter_field");
    }
}
