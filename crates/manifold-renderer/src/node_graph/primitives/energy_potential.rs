//! `node.energy_potential` — FLIP's whitewater energy potential per liquid
//! particle (`docs/GPU_WHITEWATER_DESIGN.md` section 3.3). A per-element atom
//! on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// FLIP's defaults, in J/kg.
pub(crate) const MIN_ENERGY: f32 = 0.1;
pub(crate) const MAX_ENERGY: f32 = 60.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EnergyUniforms {
    min_energy: f32,
    max_energy: f32,
    dispatch_count: u32,
    _pad: u32,
}

crate::primitive! {
    name: EnergyPotential,
    type_id: "node.energy_potential",
    purpose: "How much whitewater a liquid particle's speed can make, as FLIP measures it: its kinetic energy per kilogram, ½|v|², held between Min Energy and Max Energy and scaled to 0–1 across that range. One f32 per particle slot; 0 for slots with radius 0.",
    inputs: {
        particles: Array(FluidParticle) required,
        min_energy: ScalarF32 optional,
        max_energy: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("min_energy", "Min Energy", MIN_ENERGY, 0.0, 1.0e4),
        float_param!("max_energy", "Max Energy", MAX_ENERGY, 0.0, 1.0e4),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.sample_faces_at_particles in the whitewater emitter chain; the Whitewater group's Min Energy and Max Energy drive it. Feeds node.emission_count, and the spawn's lifetimes.",
    examples: [],
    summary: "Scores how fast each bit of water is moving, from 0 to 1, because faster water throws more foam and spray.",
    category: Particles3D,
    role: Filter,
    aliases: ["kinetic energy", "speed score", "energy"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/energy_potential_body.wgsl"),
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
}

impl Primitive for EnergyPotential {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let min_energy = ctx.scalar_or_param("min_energy", MIN_ENERGY);
        let max_energy = ctx.scalar_or_param("max_energy", MAX_ENERGY);
        if !(max_energy > min_energy && min_energy.is_finite() && max_energy.is_finite()) {
            ctx.error(format!("Energy Potential: Max Energy {max_energy} does not lie above Min Energy {min_energy}"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(particles), Some(out)) = (ctx.inputs.array("particles"), ctx.outputs.array("out")) else {
            return;
        };
        let count = (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = EnergyUniforms { min_energy, max_energy, dispatch_count: count, _pad: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.energy_potential",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
