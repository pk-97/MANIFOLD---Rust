//! `node.smooth_lattice` — binomial smoothing of a scalar lattice held in an
//! `Array(f32)` (GPU_FLUID_SURFACE_DESIGN.md P6c): the liquid level set before
//! marching cubes, as the CPU mesher's smoothing iterations did. One axis per
//! atom; chained over x, y and z it is the full 3D binomial blur at 3·(2p + 1)
//! taps per node instead of (2p + 1)³. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    passes: f32,
    axis: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SmoothLattice,
    type_id: "node.smooth_lattice",
    purpose: "Smooth a scalar lattice held in an Array<f32> (nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k)) along one axis: `passes` rounds of the [1, 2, 1] / 4 filter, applied as one (2·passes + 1)-tap binomial gather with edge-clamped indices. Chained over axes 0, 1 and 2 it is the full 3D binomial blur. 0 passes, or no lattice, copies the input; values past the lattice pass through.",
    inputs: {
        levelset: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        passes: ScalarF32 optional,
    },
    outputs: {
        smoothed: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("passes", "Smoothing Passes", 2.0, 0.0, 3.0),
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Chain three, axes 0, 1 and 2, between node.particle_volume and node.count_surface_triangles / node.volume_surface_mesh, with nodes_x/y/z from the volume's volume_nodes_x/y/z and one passes value wired into all three. More passes round the liquid surface and thin sheets further. Live: changing passes reshapes the next frame without touching the simulation.",
    examples: [],
    picker: { label: "Smooth Lattice", category: Atom },
    summary: "Softens a liquid's density field so its surface comes out smooth instead of lumpy.",
    category: Particles3D,
    role: Filter,
    aliases: ["blur volume", "smooth level set", "binomial blur", "lattice blur"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/smooth_lattice_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for SmoothLattice {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "smoothed")
            .then(|| inputs.iter().find(|(name, _)| *name == "levelset").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ctx.inputs.any_pending() {
            ctx.mark_outputs_pending();
            return;
        }
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let passes = ctx.scalar_or_param("passes", 2.0).round().clamp(0.0, 3.0);
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Float(n)) => n.round().clamp(0.0, 2.0) as i32,
            _ => 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(smoothed)) = (ctx.inputs.array("levelset"), ctx.outputs.array("smoothed")) else {
            return;
        };
        let node_total: u64 = nodes.iter().map(|&n| n.max(0.0) as u64).product();
        if nodes.iter().all(|&n| n >= 2.0) && node_total > levelset.size / 4 {
            ctx.error(format!(
                "Smooth Lattice: a {}×{}×{} lattice is larger than its level set",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let count = (levelset.size.min(smoothed.size) / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = SmoothUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            passes,
            axis,
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
                GpuBinding::Buffer { binding: 2, buffer: smoothed, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.smooth_lattice",
        );
    }
}
