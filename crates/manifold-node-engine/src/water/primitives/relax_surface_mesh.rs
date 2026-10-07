//! Uses the neighbour-mean mesh smoothing from FLIP Fluids trianglemesh.cpp `smooth` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.relax_surface_mesh` — one umbrella relaxation pass over
//! node.volume_surface_mesh's triangle list (BUG-xwf1 (Liquid Surface mesh
//! relaxation)): each vertex moves `strength` of the way to the mean of its
//! neighbours, as FLIP Fluids' mesh smoothing does. The neighbours come from
//! the lattice the mesh was built on, so the triangle list needs no index
//! buffer. Use smooth_surface_mesh for a param-driven iteration loop. A per-element gather on the codegen
//! path; each cell owns its scan interval, while a pass-2 invocation on the
//! same generated kernel clears retired vertex slots before emission. With
//! `extent` wired it dispatches only over live and last frame's vertices and
//! passes the mesh's live extent on.

use std::borrow::Cow;

pub const SURFACE_MESH_ADJACENCY_WGSL: &str = include_str!("shaders/surface_mesh_adjacency.wgsl");

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use super::liquid_bricks;
use crate::float_param;
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::mesh::MeshVertex;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::scene::live_extent::LiveExtent;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

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
    composition_notes: "When volume_surface_mesh uses edge_scan, wire the same scan here to relax its compact shared vertices; keep its indices unchanged. Wire vertices from node.volume_surface_mesh (or another relax pass), and the same levelset, scan, extent and nodes_x/y/z that mesh was built from this frame. Use smooth_surface_mesh to repeat this pass with a param-driven loop; strength 0 turns relaxation off. Relaxing rounds off marching-cubes facets and lattice stair-steps, and shrinks thin sheets and drops a little, more with every pass. The level-set normals pass through unchanged. Wire relaxed into node.scene_object like the mesh.",
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
    wgsl_includes: [MARCHING_CUBES_COMMON, liquid_bricks::COMMON, include_str!("shaders/surface_edge_ownership.wgsl"), include_str!("shaders/surface_edge_index.wgsl"), SURFACE_MESH_ADJACENCY_WGSL],
    owned_outputs: ["relaxed"],
    buffer_index: "liquid_cell_brick_index",
    extra_fields: {
        pass: SurfaceMeshPass = SurfaceMeshPass::default(),
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
        let strength = ctx.scalar_or_param("strength", 0.5);
        self.pass.run::<Self>(ctx, strength, 1, "relaxed");
    }
}

/// Shared cell-owned pass execution. Scratch storage grows only with the
/// incoming mesh capacity; iterations never change the graph or its extent.
#[derive(Default)]
pub struct SurfaceMeshPass {
    pipeline: Option<GpuComputePipeline>,
    scratch: Option<GpuBuffer>,
    emit_target: usize,
}

impl SurfaceMeshPass {
    pub fn run<P: Primitive>(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        strength: f32,
        iterations: u32,
        output: &str,
    ) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<P>(&mut self.pipeline, gpu.device);
        let (Some(vertices), Some(levelset), Some(scan), Some(relaxed)) = (
            ctx.inputs.array("vertices"),
            ctx.inputs.array("levelset"),
            ctx.inputs.array("scan"),
            ctx.outputs.array(output),
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
                output,
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
        let scratch_fresh = iterations > 1
            && self
                .scratch
                .as_ref()
                .is_none_or(|b| b.size != vertices.size);
        if scratch_fresh {
            self.scratch = Some(gpu.device.create_buffer_shared(vertices.size));
        }
        // Zero iterations copies without moving; every positive iteration is
        // Jacobi, reading the complete previous pass, never in-place.
        let steps = iterations.max(1);
        for step in 0..steps {
            let destination_is_output = (steps - step) % 2 == 1;
            let destination = if destination_is_output {
                relaxed
            } else {
                self.scratch.as_ref().expect("smoothing scratch")
            };
            let source = if step == 0 {
                vertices
            } else if destination_is_output {
                self.scratch.as_ref().expect("smoothing scratch")
            } else {
                relaxed
            };
            let uniforms = RelaxUniforms {
                strength: if iterations == 0 { 0.0 } else { strength },
                ..uniforms
            };
            let clear_uniforms = RelaxUniforms {
                brick_pass: 2,
                dispatch_count: slots,
                ..uniforms
            };
            let buffers = [
                source,
                levelset,
                scan,
                extent.unwrap_or(scan),
                bricks.unwrap_or(scan),
                edge_scan.unwrap_or(scan),
                destination,
            ];
            // Scratch may have skipped frames when iterations was 0 or 1.
            // The upstream previous-frame extent cannot describe its retired tail.
            let target_fresh = if destination_is_output { fresh } else { true };
            if let Some(extent) = extent.filter(|_| !target_fresh) {
                gpu.native_enc.dispatch_compute_indirect(
                    pipeline,
                    &mesh_bindings(&clear_uniforms, buffers),
                    extent,
                    super::running_total::EXTENT_GRID_OFFSET,
                    "surface_mesh.clear_tail",
                );
            } else {
                liquid_bricks::dispatch(
                    gpu.native_enc,
                    pipeline,
                    &mesh_bindings(&clear_uniforms, buffers),
                    bricks,
                    2,
                    slots,
                    "surface_mesh.clear",
                );
            }
            liquid_bricks::dispatch(
                gpu.native_enc,
                pipeline,
                &mesh_bindings(&uniforms, buffers),
                bricks,
                brick_pass,
                dispatch_cells,
                P::TYPE_ID,
            );
        }
    }
}

fn mesh_bindings<'a>(
    uniforms: &'a RelaxUniforms,
    buffers: [&'a GpuBuffer; 7],
) -> [GpuBinding<'a>; 8] {
    std::array::from_fn(|i| {
        if i == 0 {
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(uniforms),
            }
        } else {
            GpuBinding::Buffer {
                binding: i as u32,
                buffer: buffers[i - 1],
                offset: 0,
            }
        }
    })
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
        let source = crate::freeze::codegen::standalone_for_spec::<RelaxSurfaceMesh>()
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

#[cfg(test)]
mod shared_shader_tests {
    #[test]
    fn surface_mesh_adjacency_bytes_unchanged() {
        use sha2::{Digest, Sha256};
        assert_eq!(format!("{:x}", Sha256::digest(super::SURFACE_MESH_ADJACENCY_WGSL.as_bytes())),
            "32f3b339a2e09d8229b860138cb66127d2d963b8318576b4d79b2fd6cd7ca302");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
