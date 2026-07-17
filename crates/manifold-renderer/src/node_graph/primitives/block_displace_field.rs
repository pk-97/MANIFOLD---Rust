//! `node.block_displace_field` — a per-block random UV-offset field, as
//! a pure generator. The datamosh / block-glitch building block.
//!
//! Quantises the canvas into `block_size`-pixel blocks, hashes each
//! block (animated by `time`), and emits two textures:
//!   - `offset` (RG): a signed per-block UV displacement (x in R, y in
//!     G), gated so only a fraction of blocks move — feed it into a
//!     `node.remap` in **Relative** mode (or sum it with other offset
//!     fields via `node.mix(Add)` first) to warp a source.
//!   - `hash` (R): the raw per-block hash in [0, 1). Threshold it
//!     (`node.exposure` → `node.smoothstep`) for any per-block
//!     accent that must align with the displaced blocks — e.g. the
//!     Glitch invert mask.
//!
//! Split out of the old fused `node.glitch_displace` (which bundled
//! block displace + scanline jitter + invert mask into one pass).
//! `time` drives the hash — wired or read from `FrameTime.seconds`.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// Standalone-codegen uniform layout: PARAMS order (amount, block_size, speed,
// time) then the injected multi-output write flags (write_offset, write_hash),
// padded to 32 bytes. The hand uniform was just {amount, block_size, speed, time}
// (16 bytes); both outputs are always consumed so the flags are always 1.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlockDisplaceUniforms {
    amount: f32,
    block_size: f32,
    speed: f32,
    time: f32,
    write_offset: u32,
    write_hash: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: BlockDisplaceField,
    type_id: "node.block_displace_field",
    purpose: "Generator for a per-block random UV-offset field (the datamosh / block-glitch building block). Quantises the canvas into block_size-pixel blocks, hashes each (animated by time), and emits `offset` (RG = signed per-block UV displacement, gated so only a fraction of blocks move) and `hash` (R = raw per-block hash in [0,1) for downstream per-block accents that must align with the displaced blocks). Feed `offset` into node.remap (Relative mode) — alone or summed with other offset fields via node.mix(Add). `amount`/`speed` port-shadow their params; `time` is wired or read from FrameTime.seconds.",
    inputs: {
        amount: ScalarF32 optional,
        speed: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        offset: Texture2D,
        hash: Texture2D,
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
            name: Cow::Borrowed("block_size"),
            label: "Block Size",
            ty: ParamType::Float,
            default: ParamValue::Float(16.0),
            range: Some((4.0, 64.0)),
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
        // Backing param for the `time` input (port-shadow). Normally wired from
        // generator_input.time or read from FrameTime.seconds in run(); the param
        // gives the freeze codegen a uniform field to read (run() packs the
        // resolved time, so the default below is never the live value).
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
    composition_notes: "offset is a signed displacement in UV units (~±0.15 in x, ±0.03 in y at amount=1), gated by step(1 - amount*0.6, block_hash) so it grows from sparse to dense as amount rises. Sum it with node.scanline_jitter_field's offset via node.mix(Add), then node.remap(mode=Relative, wrap=Clamp) to warp the source. hash carries the same block_hash the gate uses, so a `hash → node.exposure(amount) → node.smoothstep(0.91, 0.92)` chain reproduces the legacy per-block invert accent and stays aligned with the moved blocks. block_size is clamped to >= 4 in-shader.",
    examples: ["preset.effect.glitch"],
    picker: { label: "Block Displace Field", category: Atom },
    summary: "Outputs a grid of random block offsets, the displacement map behind datamosh and block-glitch looks. Feed it into Remap.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["block displace", "datamosh", "glitch blocks"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/block_displace_field_body.wgsl"),
}

impl Primitive for BlockDisplaceField {
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
        let block_size = scalar_or_param("block_size", 16.0).max(4.0);
        let speed = scalar_or_param("speed", 2.0);
        // Wire wins; else the playback clock's seconds — same value the
        // runtime injects into system.generator_input.time.
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
        let Some(hash_out) = ctx.outputs.texture_2d("hash") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Multi-output Source: the generated kernel binds uniform(0)/dst_offset
            // (1)/dst_hash(2), the body returns both in BodyOutputs, and each store
            // is gated on the injected write flag. block_displace_field.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.block_displace_field standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.block_displace_field",
            )
        });

        let uniforms = BlockDisplaceUniforms {
            amount,
            block_size,
            speed,
            time,
            write_offset: 1,
            write_hash: 1,
            _pad0: 0,
            _pad1: 0,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: hash_out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.block_displace_field",
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
    fn declares_offset_and_hash_outputs() {
        assert_eq!(BlockDisplaceField::TYPE_ID, "node.block_displace_field");
        assert_eq!(BlockDisplaceField::OUTPUTS.len(), 2);
        assert_eq!(BlockDisplaceField::OUTPUTS[0].name, "offset");
        assert_eq!(BlockDisplaceField::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(BlockDisplaceField::OUTPUTS[1].name, "hash");
        assert_eq!(BlockDisplaceField::OUTPUTS[1].ty, PortType::Texture2D);
    }

    #[test]
    fn amount_speed_time_are_optional_scalar_inputs() {
        let names: Vec<&str> = BlockDisplaceField::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["amount", "speed", "time"]);
        assert!(BlockDisplaceField::INPUTS.iter().all(|p| !p.required));
        assert!(
            BlockDisplaceField::INPUTS
                .iter()
                .all(|p| p.ty == PortType::Scalar(ScalarType::F32))
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BlockDisplaceField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.block_displace_field");
    }
}
