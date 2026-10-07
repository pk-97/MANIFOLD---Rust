//! FLIP surface-emitter velocity boost before energy and wavecrest evaluation.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, cell_total, grid_cells, particle_grid};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VelocityUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    spray_speed: f32,
    seed: f32,
    epoch: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: WhitewaterEmitterVelocity,
    type_id: "node.whitewater_emitter_velocity",
    purpose: "For surface-classified markers above minus 0.75 cell depth, scale sampled velocity uniformly between 1 and Spray Emission Speed before energy and wavecrest evaluation. Other particles pass unchanged. FLIP diffuseparticlesimulation.cpp:1699.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        spray_speed: ScalarF32 optional,
        seed: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("spray_speed", "Spray Emission Speed", 1.0, 1.0, 100.0),
        float_param!("seed", "Seed", 0.0, 0.0, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
    ],
    depth_rule: Terminal,
    composition_notes: "After sample_faces_at_particles, before energy_potential and wavecrest_potential. Keep this separate from fresh spray velocity scaling: FLIP draws a second random factor after spawn classification.",
    examples: [],
    summary: "Scales surface emitter velocity by the FLIP spray emission factor.",
    category: Particles3D,
    role: Filter,
    aliases: ["spray emitter velocity"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_emitter_velocity_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for WhitewaterEmitterVelocity {
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
                    .find(|(name, _)| *name == "particles")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match particle_grid(ctx) {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Whitewater Emitter Velocity: {refusal}"));
                return;
            }
        };
        let spray_speed = ctx.scalar_or_param("spray_speed", 1.0);
        let seed = ctx.scalar_or_param("seed", 0.0);
        let epoch = ctx.scalar_or_param("epoch", 0.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(distance), Some(cells)) =
            (ctx.inputs.array("distance"), ctx.inputs.array("cells"))
        else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let total = cell_total(grid_cells(grid.nodes).expect("particle_grid checked the lattice"));
        if total * 4 > distance.size || total * 4 > cells.size {
            ctx.error(format!(
                "Whitewater Emitter Velocity: a {:?}-node grid is larger than its arrays",
                grid.nodes
            ));
            return;
        }
        let count = (particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .min(out.size / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = VelocityUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            spray_speed,
            seed,
            epoch,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: distance,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: cells,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.whitewater_emitter_velocity",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
