//! `node.volume_surface_mesh` — the marching-cubes triangle list of a level
//! set, one thread per output vertex (GPU_FLUID_SURFACE_DESIGN.md D16). It
//! feeds `node.scene_object.vertices` exactly as the CPU fluid mesh does. A
//! per-element gather on the codegen path. With `extent` wired it dispatches
//! only over live and last frame's vertices and publishes the live extent
//! (GPU_FLUID_SURFACE_DESIGN.md P6b).

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

const DEFAULT_CAPACITY: f32 = 1_572_864.0;
/// Ray-tracing bound granularity, in vertices.
const BOUND_GRAIN: u64 = 3 * 16_384;
/// Bound headroom over the late total: the dam break grows at most 1.42× over
/// two ticks (measured 2026-09-30).
const BOUND_HEADROOM: f64 = 1.5;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    resolution_scale: i32,
    max_capacity: i32,
    dispatch_count: u32,
}

fn capacity(params: &ParamValues) -> u32 {
    let value = match params.get("max_capacity") {
        Some(ParamValue::Float(n)) => *n,
        _ => DEFAULT_CAPACITY,
    };
    (value.clamp(3.0, 16_777_215.0) as u32 / 3) * 3
}

crate::primitive! {
    name: VolumeSurfaceMesh,
    type_id: "node.volume_surface_mesh",
    purpose: "Build the triangle-list mesh of a level set's zero crossing (marching cubes): one output vertex per thread, placed by binary search over the running total of per-cell triangle counts, with a gradient normal pointing outward. Slots past the live triangles are zero; a surface needing more than max_capacity vertices becomes an empty mesh and an error.",
    inputs: {
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        total: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
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
        ParamDef {
            name: Cow::Borrowed("resolution_scale"),
            label: "Resolution Scale",
            ty: ParamType::Int,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Mesh Capacity (vertices)",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_CAPACITY),
            range: Some((3.0, 16_777_215.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset and nodes_x/y/z from node.particle_volume, scan and total from node.running_total over node.count_surface_triangles, extent from the same running total with per_item 3, and the box from the lattice bounds through node.transform_components. resolution_scale must match the volume's; it places UVs on the authored domain (1.5 simulation cells inside the padded lattice) as the CPU fluid mesh does. Slots past the live triangles are zero. With extent wired the mesh writes only live and last frame's vertices and publishes its live extent, so node.render_scene draws only live triangles. Wire vertices to node.scene_object like the CPU mesh.",
    examples: [],
    picker: { label: "Volume Surface Mesh", category: Atom },
    summary: "Builds the triangle mesh of a liquid's surface from its density field, ready to render with any material.",
    category: Geometry3D,
    role: Filter,
    aliases: ["marching cubes", "isosurface", "polygonize", "surface mesh", "liquid mesh"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/volume_surface_mesh_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [MARCHING_CUBES_COMMON],
    extra_fields: {
        // Identity of the vertex buffer last written; a new one is cleared whole.
        emit_target: usize = 0,
        // Frames written into `emit_target`: the late total describes the
        // surface from the third.
        frames: u32 = 0,
    },
}

/// A CPU upper bound on live vertices for passes that need one (ray-tracing
/// builds): the late total with headroom, in whole grains, never above `slots`.
fn vertex_bound(late_triangles: f32, slots: u32) -> u32 {
    if !late_triangles.is_finite() {
        return slots;
    }
    let wanted = (f64::from(late_triangles.max(0.0)) * 3.0 * BOUND_HEADROOM).ceil() as u64 + BOUND_GRAIN;
    (wanted.div_ceil(BOUND_GRAIN) * BOUND_GRAIN).min(u64::from(slots)) as u32
}

impl Primitive for VolumeSurfaceMesh {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _: &[(&str, u32)]) -> Option<u32> {
        (port == "vertices").then(|| capacity(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let capacity = capacity(ctx.params);
        // The GPU emptied last frame's mesh if this total overflowed; say so.
        let total = ctx.scalar_or_param("total", 0.0);
        if total.is_finite() && total * 3.0 > capacity as f32 {
            ctx.error(format!(
                "Volume Surface Mesh: the surface needs {} vertices; Mesh Capacity is {capacity}. The mesh is empty until capacity is raised.",
                total as u64 * 3
            ));
        }
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let uniforms = MeshUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            resolution_scale: match ctx.params.get("resolution_scale") {
                Some(ParamValue::Float(n)) => n.round().clamp(1.0, 8.0) as i32,
                _ => 2,
            },
            max_capacity: capacity as i32,
            dispatch_count: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(scan), Some(vertices)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("scan"),
            ctx.outputs.array("vertices"),
        ) else {
            return;
        };
        let cells: u64 = nodes.iter().map(|&n| n.max(2.0) as u64 - 1).product();
        let node_total: u64 = nodes.iter().map(|&n| n.max(2.0) as u64).product();
        let lattice = nodes.iter().all(|&n| n >= 2.0);
        if lattice && (node_total > levelset.size / 4 || cells > scan.size / 4) {
            ctx.error("Volume Surface Mesh: the lattice is larger than its level set or running total");
            return;
        }
        let slots = (vertices.size / std::mem::size_of::<MeshVertex>() as u64).min(u64::from(capacity)) as u32;
        let extent = ctx.inputs.array("extent");
        // A new vertex buffer holds unknown bytes: write every slot once.
        let fresh = vertices.identity_key() != self.emit_target;
        if fresh {
            self.emit_target = vertices.identity_key();
            self.frames = 0;
        }
        let bound = if self.frames < 2 { slots } else { vertex_bound(total, slots) };
        self.frames = self.frames.saturating_add(1);
        if let Some(extent) = extent {
            ctx.outputs.set_live_extent(
                "vertices",
                LiveExtent { counts: extent.clone(), offset: 0, per_item: 3, bound },
            );
        }
        let uniforms = MeshUniforms { dispatch_count: slots, ..uniforms };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: levelset, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: scan, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: extent.unwrap_or(scan), offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: vertices, offset: 0 },
        ];
        let gpu = ctx.gpu_encoder();
        match extent {
            // The running total's grid covers this frame's and last frame's
            // vertices: live ones are written, the rest cleared.
            Some(extent) if !fresh => gpu.native_enc.dispatch_compute_indirect(
                pipeline,
                &bindings,
                extent,
                EXTENT_GRID_OFFSET,
                "node.volume_surface_mesh",
            ),
            _ => gpu.native_enc.dispatch_compute(
                pipeline,
                &bindings,
                [slots.div_ceil(256), 1, 1],
                "node.volume_surface_mesh",
            ),
        }
    }
}
