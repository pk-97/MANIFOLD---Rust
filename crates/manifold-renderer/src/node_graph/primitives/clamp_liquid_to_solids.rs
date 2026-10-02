//! `node.clamp_liquid_to_solids` — the last step before a liquid surface is
//! meshed: nodes inside a solid are never liquid and the lattice border is
//! outside, whatever smoothing did to them. node.particle_volume applies the
//! same rule before smoothing; smoothing can pull liquid values back into
//! walls, bodies and the border, so the mesher reads this atom's output.
//! A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: ClampLiquidToSolids,
    type_id: "node.clamp_liquid_to_solids",
    purpose: "Clamp a liquid level set (nodes_x/y/z nodes over the center/size box, node (i, j, k) at i + nx·(j + ny·k)) against its solid lattice (solid_nodes_x/y/z over the same box, sampled trilinearly): border nodes read a tenth of a bin outside, nodes where the solid is negative read max(value, 0), every other node passes through. Nodes past the lattice, and every node while there is no lattice, pass through.",
    inputs: {
        levelset: Array(f32) required,
        solid: Array(f32) required,
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
    examples: ["WaterDamBreakGpu", "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"],
    picker: { label: "Clamp Liquid To Solids", category: Atom },
    summary: "Keeps a liquid surface out of walls and solid bodies and closed at the edge of its grid, after smoothing.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid clamp", "wall clamp", "level set clamp", "close liquid surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/clamp_liquid_to_solids_body.wgsl"),
    input_access: [Coincident, BufferGather],
    // The solid lattice is coarser than the level set: the count is the level set's alone.
    output_capacity: FusedOutputCapacity::FromInput { input: "levelset" },
}

impl Primitive for ClampLiquidToSolids {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "clamped")
            .then(|| inputs.iter().find(|(name, _)| *name == "levelset").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let solid_nodes =
            ["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(solid), Some(clamped)) =
            (ctx.inputs.array("levelset"), ctx.inputs.array("solid"), ctx.outputs.array("clamped"))
        else {
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
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: levelset, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: clamped, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.clamp_liquid_to_solids",
        );
    }
}
