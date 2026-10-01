//! `node.jitter_particles` — FLIP's emitter jitter on liquid particles
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.3). A per-element atom on the
//! codegen path.
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
use crate::node_graph::whitewater::WHITEWATER_COMMON;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct JitterUniforms {
    cell_size: f32,
    seed: f32,
    epoch: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: JitterParticles,
    type_id: "node.jitter_particles",
    purpose: "Moves each live liquid particle by a uniform random offset of up to a quarter cell on each axis (0.25 × (1 − 0.001) × cell size), as FLIP's whitewater does before it looks for emitters, so emitters don't sit on the solver's particle lattice. The offset comes from a hash of the slot, the seed and the epoch: the same inputs give the same jitter. Velocity, radius and id pass through; slots with radius 0 pass whole.",
    inputs: {
        particles: Array(FluidParticle) required,
        cell_size: ScalarF32 optional,
        seed: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("seed", "Seed", 0.0, -1.0e9, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
    ],
    depth_rule: Terminal,
    composition_notes: "The first atom of the whitewater emitter chain: particles from a liquid frame's particles_b, cell_size the domain's, seed the domain's simulation time so each frame jitters afresh, epoch its reset count. Feed node.sample_faces_at_particles next.",
    examples: [],
    picker: { label: "Jitter Particles", category: Atom },
    summary: "Nudges each liquid particle by a small random amount, so foam doesn't line up on the simulation's grid.",
    category: Particles3D,
    role: Filter,
    aliases: ["jitter", "scatter particles", "random offset"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/jitter_particles_body.wgsl"),
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for JitterParticles {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size > 0.0 && cell_size.is_finite()) {
            ctx.error(format!("Jitter Particles: cell size {cell_size} is not a length"));
            return;
        }
        let uniforms = JitterUniforms {
            cell_size,
            seed: ctx.scalar_or_param("seed", 0.0),
            epoch: ctx.scalar_or_param("epoch", 0.0),
            dispatch_count: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(particles), Some(out)) = (ctx.inputs.array("particles"), ctx.outputs.array("out")) else {
            return;
        };
        let count = (particles.size.min(out.size) / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = JitterUniforms { dispatch_count: count, ..uniforms };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.jitter_particles",
        );
    }
}
