//! Normalized FLIP turbulence potential for markers outside the surface-emitter set.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_water_liquid::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, cell_total, grid_cells, particle_grid};

/// FLIP turbulence normalization defaults; dust uses 0.75 times the minimum.
pub(crate) const MIN_TURBULENCE: f32 = 100.0;
pub(crate) const MAX_TURBULENCE: f32 = 200.0;
pub(crate) const INSIDE_ENABLED: f32 = 1.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InsideUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: InsideTurbulencePotential,
    type_id: "node.inside_turbulence_potential",
    purpose: "Sample FLIP turbulence at p minus half a cell with out-of-range corners zero, clamp to Min/Max Turbulence and normalize. Exclude surface particles (within 1.5 cells of the surface and bordering air). Inside Enabled zero suppresses this source.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        turbulence: Array(f32) required,
        cells: Array(u32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        min_turbulence: ScalarF32 optional,
        max_turbulence: ScalarF32 optional,
        inside_enabled: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
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
        float_param!("min_turbulence", "Min Turbulence", MIN_TURBULENCE, 0.0, 1.0e5),
        float_param!("max_turbulence", "Max Turbulence", MAX_TURBULENCE, 0.0, 1.0e5),
        float_param!("inside_enabled", "Inside Enabled", INSIDE_ENABLED, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Sample turbulence_field with the same distance and shrunken material cells as wavecrest_potential; feed turbulence_emission_count alongside the mutually exclusive wavecrest potential.",
    examples: [],
    summary: "Finds submerged turbulent emitters, including near-surface particles classified inside by FLIP.",
    category: Particles3D,
    role: Filter,
    aliases: ["inside emission", "turbulence potential", "bubble source"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/inside_turbulence_potential_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for InsideTurbulencePotential {
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
                ctx.error(format!("Inside Turbulence Potential: {refusal}"));
                return;
            }
        };
        let min_turbulence = ctx.scalar_or_param("min_turbulence", MIN_TURBULENCE);
        let max_turbulence = ctx.scalar_or_param("max_turbulence", MAX_TURBULENCE);
        if !(max_turbulence > min_turbulence
            && min_turbulence.is_finite()
            && max_turbulence.is_finite())
        {
            ctx.error(format!(
                "Inside Turbulence Potential: Max Turbulence {max_turbulence} does not lie above Min Turbulence {min_turbulence}"
            ));
            return;
        }
        let inside_enabled = ctx.scalar_or_param("inside_enabled", INSIDE_ENABLED);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(distance), Some(turbulence), Some(cells)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("turbulence"),
            ctx.inputs.array("cells"),
        ) else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let total = cell_total(grid_cells(grid.nodes).expect("particle_grid checked the lattice"));
        if total * 4 > distance.size || total * 4 > turbulence.size || total * 4 > cells.size {
            ctx.error(format!(
                "Inside Turbulence Potential: a {:?}-node grid is larger than its arrays",
                grid.nodes
            ));
            return;
        }
        let count =
            (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = InsideUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            min_turbulence,
            max_turbulence,
            inside_enabled,
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
                    buffer: turbulence,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: cells,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.inside_turbulence_potential",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
