//! `node.particle_volume` — the liquid level set: one value per lattice node,
//! summed from the anisotropic kernels in the node's bins
//! (GPU_FLUID_SURFACE_DESIGN.md D8, D15, D18). A per-element gather on the
//! codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FluidBlob};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VolumeUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    threshold: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// Level-set nodes per axis: `(n − 1)·m + 1` over the solid lattice's box.
pub(crate) fn refined_nodes(solid_nodes: [f32; 3], scale: u32) -> [u32; 3] {
    solid_nodes.map(|n| (n.max(2.0) as u32 - 1) * scale + 1)
}

fn scale_param(params: &ParamValues) -> u32 {
    match params.get("resolution_scale") {
        Some(ParamValue::Float(v)) => v.round().clamp(1.0, 8.0) as u32,
        _ => 2,
    }
}

crate::primitive! {
    name: ParticleVolume,
    type_id: "node.particle_volume",
    purpose: "The liquid's level set on a lattice: at each node, threshold minus the sum of every nearby kernel's (1 − |G·(x − c)|²)³, so negative is inside. The lattice is the solid lattice (nodes_x/y/z over the center/size box) refined resolution_scale times per cell. Nodes inside a solid are never inside the liquid and the border is empty, so the surface closes.",
    inputs: {
        blobs: Array(FluidBlob) required,
        cell_ranges: Array(CellRange) required,
        solid: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        threshold: ScalarF32 optional,
    },
    outputs: {
        levelset: Array(f32),
        volume_nodes_x: ScalarF32, volume_nodes_y: ScalarF32, volume_nodes_z: ScalarF32,
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Solid Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        ParamDef {
            name: Cow::Borrowed("resolution_scale"),
            label: "Resolution Scale",
            ty: ParamType::Int,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        float_param!("threshold", "Threshold", 0.5, 0.01, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blobs from node.shape_particle_blobs, cell_ranges from the same node.sort_particles_into_cells, and the producer's solid lattice (solid_b, grid_nodes_x/y/z, grid_bounds through node.transform_components). resolution_scale sets mesh detail (2–4 per simulation cell) and the allocation (solid capacity × scale³); it is not a live wire. Raising threshold thins the liquid. volume_nodes_x/y/z carry the refined lattice to node.count_surface_triangles and node.volume_surface_mesh.",
    examples: [],
    picker: { label: "Particle Volume", category: Atom },
    summary: "Turns liquid particles into a smooth density field on a grid, the step before the surface mesh is drawn.",
    category: Particles3D,
    role: Filter,
    aliases: ["level set", "particle density", "liquid field", "scalar field"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particle_volume_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
}

impl Primitive for ParticleVolume {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port != "levelset" {
            return None;
        }
        let solid = inputs.iter().find(|(name, _)| *name == "solid").map(|&(_, n)| n)?;
        Some(solid.saturating_mul(scale_param(params).pow(3)))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let scale = scale_param(ctx.params);
        // A producer without a frame yet publishes no lattice: nothing to sum,
        // and downstream atoms see no lattice either.
        let refined = if nodes.iter().all(|&n| n >= 2.0) { refined_nodes(nodes, scale) } else { [0; 3] };
        for (port, value) in ["volume_nodes_x", "volume_nodes_y", "volume_nodes_z"].into_iter().zip(refined) {
            ctx.outputs.set_scalar(port, ParamValue::Float(value as f32));
        }
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let uniforms = VolumeUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            cell_size: ctx.scalar_or_param("cell_size", 0.0625),
            resolution_scale: scale as i32,
            threshold: ctx.scalar_or_param("threshold", 0.5),
            dispatch_count: 0,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        if refined[0] == 0 {
            return;
        }
        let (Some(blobs), Some(ranges), Some(solid), Some(levelset)) = (
            ctx.inputs.array("blobs"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("solid"),
            ctx.outputs.array("levelset"),
        ) else {
            return;
        };
        let total = refined.iter().map(|&n| u64::from(n)).product::<u64>();
        let capacity = levelset.size / 4;
        let solid_total = nodes.iter().map(|&n| n as u64).product::<u64>();
        if total > capacity || solid_total > solid.size / 4 {
            ctx.error(format!(
                "Particle Volume: a {}×{}×{} lattice needs {total} nodes; storage holds {capacity}. Wire nodes_x/y/z from the same producer as solid.",
                refined[0], refined[1], refined[2]
            ));
            return;
        }
        let uniforms = VolumeUniforms { dispatch_count: total as u32, ..uniforms };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: blobs, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: levelset, offset: 0 },
            ],
            [(total as u32).div_ceil(256), 1, 1],
            "node.particle_volume",
        );
    }
}
