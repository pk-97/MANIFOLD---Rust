//! `node.wavecrest_potential` — FLIP's wavecrest potential per liquid
//! particle on the whitewater grid's fields (`docs/GPU_WHITEWATER_DESIGN.md`
//! section 3.3). A per-element atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp and interpolation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{KnownValue, WHITEWATER_COMMON, cell_total, grid_cells, particle_grid};

/// FLIP's defaults: curvature × cell size from which a crest starts to
/// emit, where it emits fully, and the least cosine between the particle's
/// direction and the surface normal.
pub(crate) const MIN_CURVATURE: f32 = 0.4;
pub(crate) const MAX_CURVATURE: f32 = 1.0;
pub(crate) const SHARPNESS: f32 = 0.4;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WavecrestUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: WavecrestPotential,
    type_id: "node.wavecrest_potential",
    purpose: "How strongly a liquid particle sits on a breaking crest, as FLIP's whitewater measures it, 0 to 1. Only particles within 1.5 cells of the surface whose cell touches air (any of 26 neighbours) count. The surface's curvature there, times the cell size, must reach Min Curvature (it counts fully at Max Curvature), and the particle must move out through the surface: the cosine between its direction and the surface normal at least Sharpness. Distance and curvature are read trilinearly at the whitewater grid's cell centres. One f32 per particle slot; 0 for slots with radius 0.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        curvature: Array(KnownValue) required,
        cells: Array(u32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        min_curvature: ScalarF32 optional,
        max_curvature: ScalarF32 optional,
        sharpness: ScalarF32 optional,
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
        float_param!("min_curvature", "Min Curvature", MIN_CURVATURE, 0.0, 10.0),
        float_param!("max_curvature", "Max Curvature", MAX_CURVATURE, 0.0, 10.0),
        float_param!("sharpness", "Sharpness", SHARPNESS, -1.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.sample_faces_at_particles in the whitewater emitter chain. distance from node.crossing_distance, curvature from the last node.extend_lattice, cells from node.liquid_cells; center/size from node.transform_components on the frame's grid_bounds, nodes_x/y/z its grid_nodes_x/y/z. Feeds node.emission_count.",
    examples: [],
    summary: "Scores how sharply each bit of water is breaking over a wave crest, which is where foam and spray come from.",
    category: Particles3D,
    role: Filter,
    aliases: ["wave crest", "crest score", "breaking wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/wavecrest_potential_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for WavecrestPotential {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match particle_grid(ctx) {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Wavecrest Potential: {refusal}"));
                return;
            }
        };
        let min_curvature = ctx.scalar_or_param("min_curvature", MIN_CURVATURE);
        let max_curvature = ctx.scalar_or_param("max_curvature", MAX_CURVATURE);
        if !(max_curvature > min_curvature && min_curvature.is_finite() && max_curvature.is_finite()) {
            ctx.error(format!(
                "Wavecrest Potential: Max Curvature {max_curvature} does not lie above Min Curvature {min_curvature}"
            ));
            return;
        }
        let sharpness = ctx.scalar_or_param("sharpness", SHARPNESS);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else { return };
        let (Some(distance), Some(curvature), Some(cells)) =
            (ctx.inputs.array("distance"), ctx.inputs.array("curvature"), ctx.inputs.array("cells"))
        else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else { return };
        let total = cell_total(grid_cells(grid.nodes).expect("particle_grid checked the lattice"));
        if total * 4 > distance.size || total * 8 > curvature.size || total * 4 > cells.size {
            ctx.error(format!("Wavecrest Potential: a {:?}-node grid is larger than its arrays", grid.nodes));
            return;
        }
        let count = (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = WavecrestUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            min_curvature,
            max_curvature,
            sharpness,
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
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: distance, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: curvature, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: cells, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.wavecrest_potential",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
