//! `node.clamp_liquid_to_solids` — the last step before a liquid surface is
//! meshed: nodes inside a solid are never liquid, including border samples.
//! node.particle_volume applies the same production FLIP rule before smoothing;
//! smoothing can pull liquid values back into walls and bodies.
//! A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::liquid_bricks;
use super::sort_particles_into_cells::float_param;
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::freeze::classify::FusedOutputCapacity;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ClampUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    solid_nodes_x: f32,
    solid_nodes_y: f32,
    solid_nodes_z: f32,
    cell_size: f32,
    brick_pass: u32,
    dispatch_count: u32,
    _pad: u32,
}

crate::primitive! {
    name: ClampLiquidToSolids,
    type_id: "node.clamp_liquid_to_solids",
    purpose: "Clamp every liquid field sample, including borders, against its solid lattice: where solid distance is negative return max(value, 0), otherwise preserve the value. This ports ScalarField::getScalarFieldValue after sign conversion; the preview mesher border replacement does not apply to production surfaces.",
    inputs: {
        levelset: Array(f32) required,
        solid: Array(f32) required,
        bricks: Array(u32) optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        solid_nodes_x: ScalarF32 optional, solid_nodes_y: ScalarF32 optional, solid_nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        clamped: Array(f32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_x", "Solid Nodes X", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_y", "Solid Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_z", "Solid Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The last node before node.count_surface_triangles and node.volume_surface_mesh, after the node.smooth_lattice chain. Wire levelset from the last smoothing pass, nodes_x/y/z from node.particle_volume's volume_nodes_x/y/z, and solid, solid_nodes_x/y/z, center/size and cell_size from the same wires node.particle_volume takes. Without it, high Smoothing or Resolution Scale can pull the surface into walls and floating bodies and open it at the lattice edge.",
    examples: ["WaterDamBreakGpuFlip", "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"],
    picker: { label: "Clamp Liquid To Solids", category: Atom },
    summary: "Keeps a liquid surface out of walls and solid bodies and closed at the edge of its grid, after smoothing.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid clamp", "wall clamp", "level set clamp", "close liquid surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/clamp_liquid_to_solids_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    // The solid lattice is coarser than the level set: the count is the level set's alone.
    output_capacity: FusedOutputCapacity::FromInput { input: "levelset" },
    derived_uniforms: ["brick_pass:u32"],
    wgsl_includes: [liquid_bricks::COMMON, include_str!("shaders/clamp_liquid_to_solids_element.wgsl")],
    buffer_index: "liquid_brick_index",
    dense_buffer_fusion: crate::exec::effect_node::DenseBufferFusion {
        body_fragments: &[
            include_str!("shaders/clamp_liquid_to_solids_element.wgsl"),
            include_str!("shaders/clamp_liquid_to_solids_dense_body.wgsl"),
        ],
        schedule_inputs: &["bricks"],
    },
}

impl Primitive for ClampLiquidToSolids {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "clamped")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let solid_nodes = ["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0).round());
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] =
            ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(solid), Some(clamped)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("solid"),
            ctx.outputs.array("clamped"),
        ) else {
            return;
        };
        let product = |n: [f32; 3]| n.iter().map(|&v| v.max(0.0) as u64).product::<u64>();
        if nodes.iter().all(|&n| n >= 2.0) && product(nodes) > levelset.size / 4 {
            ctx.error(format!(
                "Clamp Liquid To Solids: a {}×{}×{} lattice is larger than its level set",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        if solid_nodes.iter().all(|&n| n >= 2.0) && product(solid_nodes) > solid.size / 4 {
            ctx.error(format!(
                "Clamp Liquid To Solids: a {}×{}×{} solid lattice is larger than its solid storage. Wire solid_nodes_x/y/z from the same producer as solid.",
                solid_nodes[0], solid_nodes[1], solid_nodes[2]
            ));
            return;
        }
        let count = (levelset.size.min(clamped.size) / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = ClampUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            solid_nodes_x: solid_nodes[0],
            solid_nodes_y: solid_nodes[1],
            solid_nodes_z: solid_nodes[2],
            cell_size,
            brick_pass: 0,
            dispatch_count: count,
            _pad: 0,
        };
        let bricks = ctx.inputs.array("bricks");
        if bricks
            .is_some_and(|b| !liquid_bricks::valid_schedule(b, nodes.map(|n| n.max(0.0) as u32)))
        {
            ctx.error("Liquid lattice: brick schedule does not match the lattice dimensions");
            return;
        }
        let gpu = ctx.gpu_encoder();
        for pass in 0..if bricks.is_some() { 2 } else { 1 } {
            let uniforms = ClampUniforms {
                brick_pass: if bricks.is_some() { 2 - pass } else { 0 },
                ..uniforms
            };
            liquid_bricks::dispatch(
                gpu.native_enc,
                pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: levelset,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: solid,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: bricks.unwrap_or(levelset),
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: clamped,
                        offset: 0,
                    },
                ],
                bricks,
                uniforms.brick_pass,
                count,
                "node.clamp_liquid_to_solids",
            );
        }
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
