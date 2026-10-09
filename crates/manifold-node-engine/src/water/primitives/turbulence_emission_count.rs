//! `node.turbulence_emission_count` — how many whitewater particles each liquid
//! particle emits this frame, FLIP's count per tick times the frame's ticks
//! (`docs/GPU_WHITEWATER_DESIGN.md` D5, section 3.3). A per-element atom on
//! the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use crate::float_param;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::particles::FluidParticle;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

/// FLIP's wavecrest emission rate, particles per second at full potential.
pub(crate) const WAVECREST_RATE: f32 = 175.0;

crate::primitive! {
    name: TurbulenceEmissionCount,
    type_id: "node.turbulence_emission_count",
    purpose: "FLIP normal whitewater count per particle: round energy times (wavecrest rate times wavecrest potential plus turbulence rate times turbulence potential) times Duration times 8/points per cell, then multiply by ticks. Reject empty slots, low energy and speed below 1 mm/s.",
    inputs: {
        particles: Array(FluidParticle) required,
        energy: Array(f32) required,
        wavecrest: Array(f32) required,
        turbulence: Array(f32) required,
        influence: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        turbulence_rate: ScalarF32 optional,
        generation_rate: ScalarF32 optional, seed: ScalarF32 optional, epoch: ScalarF32 optional,
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
        float_param!("turbulence_rate", "Turbulence Emission", 175.0, 0.0, 1.0e5),
        float_param!("generation_rate", "Emitter Generation", 1.0, 0.0, 1.0),
        float_param!("seed", "Seed", 0.0, 0.0, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
        float_param!("points_per_cell", "Points per Cell", 8.0, 0.001, 1.0e3),
        float_param!("ticks", "Ticks", 1.0, 0.0, 1.0e3),
        float_param!("live_count", "Live Count", 1.0e9, 0.0, 1.0e9),
        float_param!("dt", "Duration (s)", 1.0 / 60.0, 0.0, 1.0e3),
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Grid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Grid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Grid Nodes Z", 71.0, 3.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Combine wavecrest_potential and inside_turbulence_potential, then running_total and spawn_whitewater. FLIP classifies markers into mutually exclusive surface and inside sources before counting.",
    examples: [],
    summary: "Works out how many foam, spray and bubble particles each bit of breaking water throws off this frame.",
    category: Particles3D,
    role: Filter,
    aliases: ["emission count", "whitewater count", "emit"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/turbulence_emission_count_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident, Coincident, BufferGather],
    output_capacity: crate::freeze::classify::FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [crate::water::whitewater::WHITEWATER_COMMON],
}

impl Primitive for TurbulenceEmissionCount {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(p, _)| *p == "particles")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        super::whitewater_emitter_dispatch::run::<Self>(
            ctx,
            &mut self.pipeline,
            &[
                "particles",
                "energy",
                "wavecrest",
                "turbulence",
                "influence",
            ],
            32,
            4,
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
