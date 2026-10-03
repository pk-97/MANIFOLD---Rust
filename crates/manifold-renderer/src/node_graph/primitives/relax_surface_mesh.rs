//! Uses the neighbour-mean mesh smoothing from FLIP Fluids trianglemesh.cpp `smooth` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.relax_surface_mesh` — one umbrella relaxation pass over
//! node.volume_surface_mesh's triangle list (BUG-xwf1 (Liquid Surface mesh
//! relaxation)): each vertex moves `strength` of the way to the mean of its
//! neighbours, as FLIP Fluids' mesh smoothing does. The neighbours come from
//! the lattice the mesh was built on, so the triangle list needs no index
//! buffer. Chain nodes for more passes. A per-element gather on the codegen
//! path; each cell owns its scan interval, while a pass-2 invocation on the
//! same generated kernel clears retired vertex slots before emission. With
//! `extent` wired it dispatches only over live and last frame's vertices and
//! passes the mesh's live extent on.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use super::liquid_bricks;
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
    max_capacity: u32,
    brick_pass: u32,
    indexed: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: RelaxSurfaceMesh,
    type_id: "node.relax_surface_mesh",
    purpose: "One relaxation pass over a marching-cubes triangle list from node.volume_surface_mesh: each lattice cell owns its scan interval and caches each shared-edge neighbour sum, then every vertex moves strength of the way toward the mean of the vertices it shares a triangle edge with. Every copy of a shared vertex moves identically and the mesh stays closed. Strength 0 copies the input; slots past the live triangles are zero. Normals pass through.",
    inputs: {
        vertices: Array(MeshVertex) required,
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        bricks: Array(u32) optional,
        edge_scan: Array(u32) optional,
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
    composition_notes: "When volume_surface_mesh uses edge_scan, wire the same scan here to relax its compact shared vertices; keep its indices unchanged. Wire vertices from node.volume_surface_mesh (or another relax pass), and the same levelset, scan, extent and nodes_x/y/z that mesh was built from this frame. Chain two or more for more passes, one strength value into all of them; 0 turns relaxation off. Relaxing rounds off marching-cubes facets and lattice stair-steps, and shrinks thin sheets and drops a little, more with every pass. The level-set normals pass through unchanged. Wire relaxed into node.scene_object like the mesh.",
    examples: [],
    picker: { label: "Relax Surface Mesh", category: Atom },
    summary: "Smooths a liquid's surface mesh by easing each point toward its neighbours, rounding off the small facets and steps.",
    category: Geometry3D,
    role: Filter,
    aliases: ["mesh smoothing", "laplacian smooth", "relax mesh", "smooth liquid mesh", "umbrella smoothing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/relax_surface_mesh_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    derived_uniforms: ["max_capacity:u32", "brick_pass:u32", "indexed:u32"],
    wgsl_includes: [MARCHING_CUBES_COMMON, liquid_bricks::COMMON, include_str!("shaders/surface_edge_ownership.wgsl"), include_str!("shaders/surface_edge_index.wgsl")],
    owned_outputs: ["relaxed"],
    buffer_index: "liquid_cell_brick_index",
    extra_fields: {
        // Identity of the output buffer last written; a new one is written whole.
        emit_target: usize = 0,
    },
}

impl Primitive for RelaxSurfaceMesh {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "relaxed")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "vertices")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
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
        let dimensions = nodes.map(|n| n.max(2.0) as u64);
        let Some(node_total) = dimensions
            .into_iter()
            .try_fold(1u64, |total, n| total.checked_mul(n))
        else {
            ctx.error("Relax Surface Mesh: lattice node count overflows the dispatch index");
            return;
        };
        let Some(cells) = nodes
            .map(|n| n.max(2.0) as u64 - 1)
            .into_iter()
            .try_fold(1u64, |total, n| total.checked_mul(n))
        else {
            ctx.error("Relax Surface Mesh: lattice cell count overflows the dispatch index");
            return;
        };
        if nodes.iter().all(|&n| n >= 2.0)
            && (node_total > levelset.size / 4 || cells > scan.size / 4)
        {
            ctx.error(
                "Relax Surface Mesh: the lattice is larger than its level set or running total",
            );
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
            ctx.error(format!(
                "Relax Surface Mesh: {in_slots} vertices is more than one dispatch carries"
            ));
            return;
        };
        let Ok(dispatch_cells) = u32::try_from(cells) else {
            ctx.error(format!(
                "Relax Surface Mesh: {cells} cells is more than one dispatch carries"
            ));
            return;
        };
        if slots == 0 || dispatch_cells == 0 {
            return;
        }
        let max_capacity = slots;
        let edge_scan = ctx.inputs.array("edge_scan");
        if ctx.inputs.slot_of("edge_scan").is_some() && edge_scan.is_none() {
            ctx.error("Relax Surface Mesh: wired edge scan is unavailable");
            return;
        }
        if edge_scan.is_some_and(|scan| scan.size / 4 < node_total) {
            ctx.error("Relax Surface Mesh: edge scan is shorter than the lattice");
            return;
        }
        let extent = ctx.inputs.array("extent");
        let fresh = relaxed.identity_key() != self.emit_target;
        self.emit_target = relaxed.identity_key();
        if let Some(live) = ctx.inputs.live_extent("vertices") {
            ctx.outputs.set_live_extent(
                "relaxed",
                LiveExtent {
                    bound: live.bound.min(slots),
                    ..live
                },
            );
        }
        let uniforms = RelaxUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            strength,
            max_capacity,
            brick_pass: 0,
            indexed: u32::from(edge_scan.is_some()),
            dispatch_count: dispatch_cells,
        };
        let bricks = ctx.inputs.array("bricks");
        if bricks
            .is_some_and(|b| !liquid_bricks::valid_schedule(b, nodes.map(|n| n.max(2.0) as u32)))
        {
            ctx.error("Relax Surface Mesh: brick schedule does not match the lattice");
            return;
        }
        let brick_pass = u32::from(bricks.is_some());
        let uniforms = RelaxUniforms {
            brick_pass,
            ..uniforms
        };
        let gpu = ctx.gpu_encoder();
        let clear_uniforms = RelaxUniforms {
            brick_pass: 2,
            dispatch_count: slots,
            ..uniforms
        };
        let clear_bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&clear_uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: vertices,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: scan,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: extent.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: bricks.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: edge_scan.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer { binding: 7, buffer: relaxed, offset: 0 },
        ];
        if let Some(extent) = extent.filter(|_| !fresh) {
            gpu.native_enc.dispatch_compute_indirect(
                pipeline,
                &clear_bindings,
                extent,
                super::running_total::EXTENT_GRID_OFFSET,
                "node.relax_surface_mesh.clear_tail",
            );
        } else {
            liquid_bricks::dispatch(
                gpu.native_enc,
                pipeline,
                &clear_bindings,
                bricks,
                2,
                slots,
                "node.relax_surface_mesh.clear",
            );
        }
        let brick_pass = u32::from(bricks.is_some());
        let uniforms = RelaxUniforms {
            brick_pass,
            dispatch_count: dispatch_cells,
            ..uniforms
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: vertices,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: scan,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: extent.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: bricks.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: edge_scan.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer { binding: 7, buffer: relaxed, offset: 0 },
        ];
        liquid_bricks::dispatch(
            gpu.native_enc,
            pipeline,
            &bindings,
            bricks,
            brick_pass,
            dispatch_cells,
            "node.relax_surface_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::RelaxSurfaceMesh;

    #[test]
    fn cell_owned_relax_codegen_caches_neighbour_sums() {
        assert_eq!(
            std::mem::size_of::<super::RelaxUniforms>(),
            8 * std::mem::size_of::<u32>()
        );
        let source = crate::node_graph::freeze::codegen::standalone_for_spec::<RelaxSurfaceMesh>()
            .expect("cell-owned relax standalone codegen");
        let module =
            naga::front::wgsl::parse_str(&source).expect("generated relax kernel must parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("generated relax kernel must validate");
        assert!(!source.contains("var lo"));
        assert!(source.contains("edge_cache[edge] = rsm_neighbour_sum"));
        assert!(source.contains("if !edge_ready[edge]"));
        assert!(source.contains("edge_cache[edge]"));
        assert!(source.contains("buf_relaxed[slot]"));
        assert!(source.contains("if idx == 0xffffffffu"));
        assert!(source.contains("if brick_pass == 2u"));
        assert!(source.contains("buf_relaxed[idx] = zero"));
        assert!(!source.contains("buf_relaxed[idx] = body"));
    }
}
