//! `node.age_whitewater` — FLIP's per-tick lifetime decay by type
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L2). A per-element
//! atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use crate::whitewater::WhitewaterParticle;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AgeUniforms {
    dt: f32,
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// Lifetime scales per kind; the fused lifecycle kernel reads the same values.
pub(crate) const BUBBLE_LIFETIME_MODIFIER: f32 = 0.333;
pub(crate) const FOAM_LIFETIME_MODIFIER: f32 = 1.0;
pub(crate) const SPRAY_LIFETIME_MODIFIER: f32 = 2.0;

manifold_node_engine::primitive! {
    name: AgeWhitewater,
    type_id: "node.age_whitewater",
    purpose: "Shortens each live whitewater particle's lifetime by its type's modifier times the tick, as FLIP does: by default spray ages twice as fast as foam and bubbles a third as fast. Empty slots (kind 3) pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [
        float_param!("dt", "Tick", 1.0 / 60.0, 0.0001, 1.0),
        float_param!("bubble_lifetime_modifier", "Bubble Lifetime Modifier", BUBBLE_LIFETIME_MODIFIER, 0.0, 100.0),
        float_param!("foam_lifetime_modifier", "Foam Lifetime Modifier", FOAM_LIFETIME_MODIFIER, 0.0, 100.0),
        float_param!("spray_lifetime_modifier", "Spray Lifetime Modifier", SPRAY_LIFETIME_MODIFIER, 0.0, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.retype_whitewater in the GPU whitewater tick, so a particle ages at its new type's rate; removal follows.",
    examples: [],
    summary: "Counts down each whitewater particle's life, spray fastest and bubbles slowest.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater lifetime", "age foam", "diffuse particle lifetime"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/age_whitewater_body.wgsl"),
    input_access: [Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [],
}

impl Primitive for AgeWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(out)) = (ctx.inputs.array("pool"), ctx.outputs.array("out")) else { return };
        let count = (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = AgeUniforms {
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
            bubble_lifetime_modifier: ctx.scalar_or_param("bubble_lifetime_modifier", 0.333),
            foam_lifetime_modifier: ctx.scalar_or_param("foam_lifetime_modifier", 1.0),
            spray_lifetime_modifier: ctx.scalar_or_param("spray_lifetime_modifier", 2.0),
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.age_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
