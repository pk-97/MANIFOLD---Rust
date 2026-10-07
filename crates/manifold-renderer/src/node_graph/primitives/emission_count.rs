//! `node.emission_count` — how many whitewater particles each liquid
//! particle emits this frame, FLIP's count per tick times the frame's ticks
//! (`docs/GPU_WHITEWATER_DESIGN.md` D5, section 3.3). A per-element atom on
//! the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// FLIP's wavecrest emission rate, particles per second at full potential.
pub(crate) const WAVECREST_RATE: f32 = 175.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CountUniforms {
    rate: f32,
    points_per_cell: f32,
    ticks: f32,
    live_count: f32,
    dt: f32,
    dispatch_count: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: EmissionCount,
    type_id: "node.emission_count",
    purpose: "How many whitewater particles each liquid particle emits this frame: FLIP's count per tick, Wavecrest Emission × energy potential × wavecrest potential × Duration × 8 / Points per Cell, rounded to a whole number on its own each tick and multiplied by the frame's Ticks. FLIP's rate is set for 8 particles per cell; the 8 / Points per Cell factor keeps the amount the same for any solver's sampling. 0 for slots at or past Live Count, slots with radius 0, and particles slower than 1 mm/s. One u32 per particle slot.",
    inputs: {
        particles: Array(FluidParticle) required,
        energy: Array(f32) required,
        wavecrest: Array(f32) required,
        rate: ScalarF32 optional,
        points_per_cell: ScalarF32 optional,
        ticks: ScalarF32 optional,
        live_count: ScalarF32 optional,
        dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("rate", "Wavecrest Emission", WAVECREST_RATE, 0.0, 1.0e5),
        float_param!("points_per_cell", "Points per Cell", 8.0, 0.001, 1.0e3),
        float_param!("ticks", "Ticks", 1.0, 0.0, 1.0e3),
        float_param!("live_count", "Live Count", 1.0e9, 0.0, 1.0e9),
        float_param!("dt", "Duration (s)", 1.0 / 60.0, 0.0, 1.0e3),
    ],
    depth_rule: Terminal,
    composition_notes: "Ends the whitewater emitter chain: particles from node.sample_faces_at_particles, energy from node.energy_potential, wavecrest from node.wavecrest_potential. ticks and points_per_cell come from the liquid domain, live_count from the frame's particle count. node.running_total over the counts gives each emitter's first spawn slot.",
    examples: [],
    summary: "Works out how many foam, spray and bubble particles each bit of breaking water throws off this frame.",
    category: Particles3D,
    role: Filter,
    aliases: ["emission count", "whitewater count", "emit"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/emission_count_body.wgsl"),
}

impl Primitive for EmissionCount {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().map(|&(_, n)| n).min()).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let points_per_cell = ctx.scalar_or_param("points_per_cell", 8.0);
        if !(points_per_cell > 0.0 && points_per_cell.is_finite()) {
            ctx.error(format!("Emission Count: {points_per_cell} points per cell is not a sampling density"));
            return;
        }
        let uniforms = CountUniforms {
            rate: ctx.scalar_or_param("rate", WAVECREST_RATE),
            points_per_cell,
            ticks: ctx.scalar_or_param("ticks", 1.0),
            live_count: ctx.scalar_or_param("live_count", 1.0e9),
            dispatch_count: 0,
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(particles), Some(energy), Some(wavecrest)) =
            (ctx.inputs.array("particles"), ctx.inputs.array("energy"), ctx.inputs.array("wavecrest"))
        else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else { return };
        let count = (particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .min(energy.size / 4)
            .min(wavecrest.size / 4)
            .min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = CountUniforms { dispatch_count: count, ..uniforms };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: energy, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: wavecrest, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.emission_count",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) mod extent;
