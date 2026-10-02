//! Uses the neighbour-mean mesh smoothing from FLIP Fluids trianglemesh.cpp `smooth` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.relax_surface_mesh` — one umbrella relaxation pass over
//! node.volume_surface_mesh's triangle list (BUG-xwf1 (Liquid Surface mesh
//! relaxation)): each vertex moves `strength` of the way to the mean of its
//! neighbours, as FLIP Fluids' mesh smoothing does. The neighbours come from
//! the lattice the mesh was built on, so the triangle list needs no index
//! buffer. Chain nodes for more passes. A per-element gather on the codegen
//! path; with `extent` wired it dispatches only over live and last frame's
//! vertices and passes the mesh's live extent on.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use super::running_total::EXTENT_GRID_OFFSET;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::live_extent::LiveExtent;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RelaxUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    strength: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: RelaxSurfaceMesh,
    type_id: "node.relax_surface_mesh",
    purpose: "One relaxation pass over a marching-cubes triangle list from node.volume_surface_mesh: each vertex moves strength of the way toward the mean of the vertices it shares a triangle edge with. Neighbours are found through the lattice the mesh was built on (the four cells around each vertex's lattice edge), so every copy of a shared vertex moves identically and the mesh stays closed. Strength 0 copies the input; slots past the live triangles are zero. Normals pass through.",
    inputs: {
        vertices: Array(MeshVertex) required,
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        strength: ScalarF32 optional,
    },
    outputs: {
        relaxed: Array(MeshVertex),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("strength", "Strength", 0.5, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire vertices from node.volume_surface_mesh (or another relax pass), and the same levelset, scan, extent and nodes_x/y/z that mesh was built from this frame. Chain two or more for more passes, one strength value into all of them; 0 turns relaxation off. Relaxing rounds off marching-cubes facets and lattice stair-steps, and shrinks thin sheets and drops a little, more with every pass. The level-set normals pass through unchanged. Wire relaxed into node.scene_object like the mesh.",
    examples: [],
    picker: { label: "Relax Surface Mesh", category: Atom },
    summary: "Smooths a liquid's surface mesh by easing each point toward its neighbours, rounding off the small facets and steps.",
    category: Geometry3D,
    role: Filter,
    aliases: ["mesh smoothing", "laplacian smooth", "relax mesh", "smooth liquid mesh", "umbrella smoothing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/relax_surface_mesh_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [MARCHING_CUBES_COMMON],
    extra_fields: {
        // Identity of the output buffer last written; a new one is written whole.
        emit_target: usize = 0,
    },
}

impl Primitive for RelaxSurfaceMesh {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "relaxed")
            .then(|| inputs.iter().find(|(name, _)| *name == "vertices").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let strength = ctx.scalar_or_param("strength", 0.5);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(vertices), Some(levelset), Some(scan), Some(relaxed)) = (
            ctx.inputs.array("vertices"),
            ctx.inputs.array("levelset"),
            ctx.inputs.array("scan"),
            ctx.outputs.array("relaxed"),
        ) else {
            return;
        };
        let cells: u64 = nodes.iter().map(|&n| n.max(2.0) as u64 - 1).product();
        let node_total: u64 = nodes.iter().map(|&n| n.max(2.0) as u64).product();
        if nodes.iter().all(|&n| n >= 2.0) && (node_total > levelset.size / 4 || cells > scan.size / 4) {
            ctx.error("Relax Surface Mesh: the lattice is larger than its level set or running total");
            return;
        }
        let vertex = std::mem::size_of::<MeshVertex>() as u64;
        let (in_slots, out_slots) = (vertices.size / vertex, relaxed.size / vertex);
        if out_slots < in_slots {
            ctx.error(format!(
                "Relax Surface Mesh: the output holds {out_slots} vertices, the mesh {in_slots}"
            ));
            return;
        }
        let Ok(slots) = u32::try_from(in_slots) else {
            ctx.error(format!("Relax Surface Mesh: {in_slots} vertices is more than one dispatch carries"));
            return;
        };
        if slots == 0 {
            return;
        }
        let extent = ctx.inputs.array("extent");
        let fresh = relaxed.identity_key() != self.emit_target;
        self.emit_target = relaxed.identity_key();
        if let Some(live) = ctx.inputs.live_extent("vertices") {
            ctx.outputs.set_live_extent("relaxed", LiveExtent { bound: live.bound.min(slots), ..live });
        }
        let uniforms = RelaxUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            strength,
            dispatch_count: slots,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: vertices, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: levelset, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: scan, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: extent.unwrap_or(scan), offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: relaxed, offset: 0 },
        ];
        let gpu = ctx.gpu_encoder();
        match extent {
            // The running total's grid covers this frame's and last frame's
            // vertices: live ones are relaxed, the rest cleared.
            Some(extent) if !fresh => gpu.native_enc.dispatch_compute_indirect(
                pipeline,
                &bindings,
                extent,
                EXTENT_GRID_OFFSET,
                "node.relax_surface_mesh",
            ),
            _ => gpu.native_enc.dispatch_compute(
                pipeline,
                &bindings,
                [slots.div_ceil(256), 1, 1],
                "node.relax_surface_mesh",
            ),
        }
    }
}
