//! Ported from FLIP Fluids polygonizer3d.cpp `_vertexInterp` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.volume_surface_mesh` — the marching-cubes triangle list of a level
//! set, one cell-owned thread per lattice cell. It feeds
//! `node.scene_object.vertices` exactly as the CPU fluid mesh does. A
//! per-element gather on the codegen path. With `extent` wired it dispatches
//! only over live and last frame's cells and publishes the live extent
//! (GPU_FLUID_SURFACE_DESIGN.md P6b).
//!
//! The node owns its vertex buffer and grows it from the late triangle total
//! (BUG-j34u (surface mesh sized to worst case)): every copy downstream sizes
//! from it, so the mesh costs what the surface needs, not the lattice's worst
//! case. Growth is the only allocation; steady state allocates nothing.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use super::liquid_bricks;
use crate::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::scene::live_extent::LiveExtent;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

/// Ray-tracing bound and growth granularity, in vertices.
const BOUND_GRAIN: u64 = 3 * 16_384;
/// Bound headroom over the late total. The dam break grows at most 1.42× over two
/// ticks (measured 2026-09-30) and the total is read a tick late; a splash impact
/// can beat 1.5×, and zero margin triangles are cheap.
const BOUND_HEADROOM: f64 = 2.0;
/// The buffer grows once the late total needs more than this share of it, to
/// `BOUND_HEADROOM` times the late total: the same lag margin as the bound.
const GROW_AT: f64 = 2.0 / 3.0;
/// Largest vertex count the kernel can index: `max_capacity` is an i32 uniform.
const INDEXABLE_VERTICES: u64 = (i32::MAX as u64 / 3) * 3;
const VERTEX_BYTES: u64 = std::mem::size_of::<MeshVertex>() as u64;

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
    solid_nodes_x: f32,
    solid_nodes_y: f32,
    solid_nodes_z: f32,
    resolution_scale: i32,
    max_capacity: i32,
    brick_pass: u32,
    indexed: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// The first buffer's capacity in vertices, whole triangles: Starting Mesh
/// Capacity when set, else the lattice's box surface.
pub(crate) fn start_capacity(params: &ParamValues, nodes: [f32; 3]) -> u64 {
    match params.get("max_capacity") {
        Some(ParamValue::Float(n)) if *n >= 3.0 => u64::from(n.min(16_777_215.0) as u32 / 3 * 3),
        _ => box_surface_vertices(nodes).max(3),
    }
}

/// Vertex slots the kernel may write in a buffer of `bytes`: whole triangles,
/// never past the buffer. Every dispatch is guarded on this count.
pub(crate) fn emit_slots(bytes: u64) -> u32 {
    ((bytes / VERTEX_BYTES).min(INDEXABLE_VERTICES) / 3 * 3) as u32
}

/// The vertices of a surface as large as the lattice's own box: two triangles
/// per cell face on all six sides. The default start, so a liquid with no
/// more surface than its container never waits on the late total; a
/// splashier one grows from it.
pub(crate) fn box_surface_vertices(nodes: [f32; 3]) -> u64 {
    if nodes.iter().any(|&n| !n.is_finite() || n < 2.0) {
        return 0;
    }
    let [x, y, z] = nodes.map(|n| n as u64 - 1);
    (2 * (x * y + y * z + z * x) * 2 * 3).min(INDEXABLE_VERTICES)
}

/// The capacity, in vertices, to grow to when `late_triangles` crowds `slots`:
/// `None` while it fits. Whole grains, so a slowly rising surface regrows
/// rarely.
pub(crate) fn grown_capacity(late_triangles: f32, slots: u32) -> Option<u64> {
    if !late_triangles.is_finite() || late_triangles <= 0.0 {
        return None;
    }
    let needed = f64::from(late_triangles) * 3.0;
    if needed <= f64::from(slots) * GROW_AT {
        return None;
    }
    let wanted = (needed * BOUND_HEADROOM).ceil() as u64;
    Some(
        (wanted.div_ceil(BOUND_GRAIN) * BOUND_GRAIN)
            .min(INDEXABLE_VERTICES)
            .max(u64::from(slots)),
    )
}

manifold_node_engine::primitive! {
    name: VolumeSurfaceMesh,
    type_id: "node.volume_surface_mesh",
    purpose: "Build the triangle-list mesh of a level set's zero crossing (marching cubes): each lattice cell owns its inclusive scan interval, caches its twelve edge vertices, and emits the existing triangle table order with gradient normals pointing outward. Optional solid-lattice samples constrain each edge crossing to the open side of the solid root using FLIP Fluids _vertexInterp (epsilon 1e-10). Slots past the live triangles are zero. The vertex buffer starts at max_capacity (0: the lattice's box surface) and grows from the late triangle total; a frame whose surface outruns the buffer is an empty mesh and a named error, and the buffer grows.",
    inputs: {
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        bricks: Array(u32) optional,
        edge_scan: Array(u32) optional,
        solid: Array(f32) optional,
        total: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        solid_nodes_x: ScalarF32 optional, solid_nodes_y: ScalarF32 optional, solid_nodes_z: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
        indices: Array(u32),
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
            label: "Starting Mesh Capacity (vertices)",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_215.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset and nodes_x/y/z from node.particle_volume, scan and total from node.running_total over node.count_surface_triangles, extent from the same running total with per_item 3, and the box from the lattice bounds through node.transform_components. Optionally wire solid and solid_nodes_x/y/z from the same solid lattice over the same box; the FLIP endpoint is clipped between the solid crossings. Without solid, no solid constraint is applied. resolution_scale must match the volume's; it places UVs on the authored domain (1.5 simulation cells inside the padded lattice) as the CPU fluid mesh does. Slots past the live triangles are zero. Wire total: the vertex buffer grows from it; unwired it holds only its start. Leave Starting Mesh Capacity at 0 so the start follows the lattice. With extent wired the mesh writes only live and last frame's vertices and publishes its live extent, so node.render_scene draws only live triangles. For indexed output, wire the inclusive node.count_surface_edges scan to edge_scan and wire both vertices and indices to node.scene_object. No edge_scan retains triangle-list output. Capacity and overflow rules are identical.",
    examples: [],
    picker: { label: "Volume Surface Mesh", category: Atom },
    summary: "Builds the triangle mesh of a liquid's surface from its density field, ready to render with any material.",
    category: Geometry3D,
    role: Filter,
    aliases: ["marching cubes", "isosurface", "polygonize", "surface mesh", "liquid mesh"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/volume_surface_mesh_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    derived_uniforms: ["brick_pass:u32", "indexed:u32"],
    wgsl_includes: [MARCHING_CUBES_COMMON, liquid_bricks::COMMON, include_str!("shaders/surface_edge_ownership.wgsl"), include_str!("shaders/surface_edge_index.wgsl"), include_str!("shaders/clamp_liquid_to_solids_element.wgsl")],
    owned_outputs: ["vertices", "indices"],
    buffer_index: "liquid_cell_brick_index",
    extra_fields: {
        // The vertex buffer this node owns and publishes.
        mesh: Option<GpuBuffer> = None,
        indices: Option<GpuBuffer> = None,
        index_stub: Option<GpuBuffer> = None,
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
    let wanted =
        (f64::from(late_triangles.max(0.0)) * 3.0 * BOUND_HEADROOM).ceil() as u64 + BOUND_GRAIN;
    (wanted.div_ceil(BOUND_GRAIN) * BOUND_GRAIN).min(u64::from(slots)) as u32
}

impl VolumeSurfaceMesh {
    /// Grow the buffer to the start capacity (it rises with the lattice) or
    /// what the late total asks for. Grow-only; a refused allocation keeps
    /// the current buffer.
    fn ensure_capacity(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        total: f32,
        nodes: [f32; 3],
        indexed: bool,
    ) {
        let current = self.mesh.as_ref().map_or(0, |mesh| emit_slots(mesh.size));
        let wanted = grown_capacity(total, current)
            .unwrap_or(0)
            .max(start_capacity(ctx.params, nodes));
        if wanted <= u64::from(current) && (!indexed || self.indices.is_some()) {
            return;
        }
        let target = (wanted.max(u64::from(current)).div_ceil(3) * 3).min(INDEXABLE_VERTICES);
        let device = ctx.gpu_encoder().device;
        match device.try_create_buffer_shared(target * VERTEX_BYTES) {
            Ok(buffer) => {
                let indices = if indexed {
                    match device.try_create_buffer_shared(target * 4) {
                        Ok(indices) => { indices.zero_fill(); Some(indices) }
                        Err(error) => {
                            ctx.error(format!("Volume Surface Mesh: index allocation failed ({error})"));
                            return;
                        }
                    }
                } else { None };
                buffer.zero_fill();
                self.indices = indices;
                if current > 0 {
                    log::info!("[volume_surface_mesh] vertex buffer grown from {current} to {target} vertices");
                }
                self.mesh = Some(buffer);
            }
            Err(error) => ctx.error(format!(
                "Volume Surface Mesh: the surface needs a {:.0} MB mesh and the device refused it ({error}). Lower Resolution.",
                (target * VERTEX_BYTES) as f64 / 1e6
            )),
        }
    }
}

impl Primitive for VolumeSurfaceMesh {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "vertices" | "indices")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port { "vertices" => self.mesh.as_ref(), "indices" => self.indices.as_ref(), _ => None }
    }

    /// Provided storage: a one-triangle hint, sized from the surface at run time.
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        matches!(port, "vertices" | "indices").then_some(3)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let total = ctx.scalar_or_param("total", 0.0);
        let held = self.mesh.as_ref().map_or(0, |mesh| emit_slots(mesh.size));
        // The GPU emptied last frame's mesh if this total overflowed; say so,
        // and grow below.
        if self.mesh.is_some() && total.is_finite() && f64::from(total) * 3.0 > f64::from(held) {
            ctx.error(format!(
                "Volume Surface Mesh: the surface needs {} vertices; Mesh Capacity is {held}. The buffer grows; the mesh is empty until it has.",
                total as u64 * 3
            ));
        }
        {
            let gpu = ctx.gpu_encoder();
            standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        }
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] =
            ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let solid_wired = ctx.inputs.slot_of("solid").is_some();
        let solid_nodes_input = ["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0));
        let solid_nodes = if solid_wired {
            let Some(solid) = ctx.inputs.array("solid") else {
                ctx.error("Volume Surface Mesh: solid is wired but its array is unavailable");
                return;
            };
            if solid_nodes_input
                .iter()
                .any(|&n| !n.is_finite() || n < 2.0)
            {
                ctx.error(format!(
                    "Volume Surface Mesh: invalid solid lattice dimensions {}×{}×{}",
                    solid_nodes_input[0], solid_nodes_input[1], solid_nodes_input[2]
                ));
                return;
            }
            let rounded = solid_nodes_input.map(f32::round);
            if rounded.iter().any(|&n| n >= u32::MAX as f32) {
                ctx.error("Volume Surface Mesh: solid lattice dimensions exceed u32");
                return;
            }
            let Some(total) = rounded.iter().try_fold(1u64, |total, &n| {
                total.checked_mul(n as u64)
            }) else {
                ctx.error("Volume Surface Mesh: solid lattice dimensions overflow");
                return;
            };
            if !solid.size.is_multiple_of(4) || total > solid.size / 4 {
                ctx.error(format!(
                    "Volume Surface Mesh: solid has {} f32 values; solid lattice needs {total}",
                    solid.size / 4
                ));
                return;
            }
            rounded
        } else {
            [0.0; 3]
        };
        let indexed = ctx.inputs.slot_of("edge_scan").is_some();
        if indexed && ctx.inputs.array("edge_scan").is_none() {
            ctx.error("Volume Surface Mesh: wired edge scan is unavailable");
            return;
        }
        self.ensure_capacity(ctx, total, nodes, indexed);
        if indexed && self.indices.is_none() { return; }
        if self.index_stub.is_none() {
            self.index_stub = Some(ctx.gpu_encoder().device.create_buffer_shared(4));
        }
        let resolution_scale = match ctx.params.get("resolution_scale") {
            Some(ParamValue::Float(n)) => n.round().clamp(1.0, 8.0) as i32,
            _ => 2,
        };
        let (Some(levelset), Some(scan), Some(vertices)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("scan"),
            self.mesh.as_ref(),
        ) else {
            return;
        };
        let dimensions = nodes.map(|n| n.max(2.0) as u64);
        let Some(node_total) = dimensions
            .into_iter()
            .try_fold(1u64, |total, n| total.checked_mul(n))
        else {
            ctx.error("Volume Surface Mesh: lattice node count overflows the dispatch index");
            return;
        };
        let Some(cells) = nodes
            .map(|n| n.max(2.0) as u64 - 1)
            .into_iter()
            .try_fold(1u64, |total, n| total.checked_mul(n))
        else {
            ctx.error("Volume Surface Mesh: lattice cell count overflows the dispatch index");
            return;
        };
        let lattice = nodes.iter().all(|&n| n >= 2.0);
        if lattice && (node_total > levelset.size / 4 || cells > scan.size / 4) {
            ctx.error(
                "Volume Surface Mesh: the lattice is larger than its level set or running total",
            );
            return;
        }
        // The kernel writes vertex idx only below this; it is the buffer's own size.
        let slots = emit_slots(vertices.size);
        let Ok(dispatch_cells) = u32::try_from(cells) else {
            ctx.error(format!(
                "Volume Surface Mesh: {cells} cells is more than one dispatch carries"
            ));
            return;
        };
        let edge_scan = ctx.inputs.array("edge_scan");
        if edge_scan.is_some_and(|scan| scan.size / 4 < node_total) {
            ctx.error("Volume Surface Mesh: edge scan is shorter than the lattice");
            return;
        }
        let extent = ctx.inputs.array("extent");
        // A new vertex buffer holds unknown bytes: write every slot once.
        let fresh = vertices.identity_key() != self.emit_target;
        if fresh {
            self.emit_target = vertices.identity_key();
            self.frames = 0;
        }
        let bound = if self.frames < 2 {
            slots
        } else {
            vertex_bound(total, slots)
        };
        self.frames = self.frames.saturating_add(1);
        if let Some(extent) = extent {
            ctx.outputs.set_live_extent(
                if indexed { "indices" } else { "vertices" },
                LiveExtent {
                    counts: extent.clone(),
                    offset: 0,
                    per_item: 3,
                    bound,
                },
            );
        }
        if let Some(edge_scan) = edge_scan {
            ctx.outputs.set_live_extent("vertices", LiveExtent {
                counts: edge_scan.clone(), offset: (node_total - 1) * 4, per_item: 1, bound: slots,
            });
        }
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
            solid_nodes_x: solid_nodes[0],
            solid_nodes_y: solid_nodes[1],
            solid_nodes_z: solid_nodes[2],
            resolution_scale,
            max_capacity: slots as i32,
            brick_pass: 0,
            indexed: u32::from(indexed),
            dispatch_count: dispatch_cells,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let bricks = ctx.inputs.array("bricks");
        if bricks
            .is_some_and(|b| !liquid_bricks::valid_schedule(b, nodes.map(|n| n.max(2.0) as u32)))
        {
            ctx.error("Liquid surface mesh: brick schedule does not match the lattice");
            return;
        }
        let solid_buffer = ctx.inputs.array("solid").unwrap_or(scan);
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.as_ref().expect("pipeline built above");
        let clear_uniforms = MeshUniforms {
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
                buffer: levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: scan,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: extent.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: bricks.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: edge_scan.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: solid_buffer,
                offset: 0,
            },
            GpuBinding::Buffer { binding: 7, buffer: vertices, offset: 0 },
            GpuBinding::Buffer {
                binding: 8, buffer: self.indices.as_ref().unwrap_or(self.index_stub.as_ref().expect("stub prepared")), offset: 0,
            },
        ];
        if let Some(extent) = extent.filter(|_| !fresh) {
            gpu.native_enc.dispatch_compute_indirect(
                pipeline,
                &clear_bindings,
                extent,
                super::running_total::EXTENT_GRID_OFFSET,
                "node.volume_surface_mesh.clear_tail",
            );
        } else {
            liquid_bricks::dispatch(
                gpu.native_enc,
                pipeline,
                &clear_bindings,
                bricks,
                2,
                slots,
                "node.volume_surface_mesh.clear",
            );
        }
        let brick_pass = u32::from(bricks.is_some());
        let uniforms = MeshUniforms {
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
                buffer: levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: scan,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: extent.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: bricks.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: edge_scan.unwrap_or(scan),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: solid_buffer,
                offset: 0,
            },
            GpuBinding::Buffer { binding: 7, buffer: vertices, offset: 0 },
            GpuBinding::Buffer {
                binding: 8, buffer: self.indices.as_ref().unwrap_or(self.index_stub.as_ref().expect("stub prepared")), offset: 0,
            },
        ];
        liquid_bricks::dispatch(
            gpu.native_enc,
            pipeline,
            &bindings,
            bricks,
            brick_pass,
            dispatch_cells,
            "node.volume_surface_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CPU extent proof for the grown buffer at Resolution 32, 64 and 128
    /// (Resolution Scale 2, the dam break's lattice plus padding): at the
    /// lattice's worst case, every growth step's buffer covers the slots the
    /// kernel is told it may write, and no count overflows its integer type.
    #[test]
    fn grown_buffers_cover_their_dispatch_at_every_grid() {
        for resolution in [32u64, 64, 128] {
            let nodes = (resolution + 3) * 2 + 1;
            let cells = (nodes - 1).pow(3);
            // Marching cubes places at most five triangles per cell.
            let worst = (cells * 5) as f32;
            let lattice = [nodes as f32; 3];
            let mut slots =
                emit_slots(start_capacity(&ParamValues::default(), lattice) * VERTEX_BYTES);
            let mut steps = 0;
            let mut late = 1.0f32;
            while late <= worst {
                if let Some(target) = grown_capacity(late, slots) {
                    assert!(
                        target > u64::from(slots) || target == INDEXABLE_VERTICES,
                        "grid {resolution}: grows"
                    );
                    let bytes = target * VERTEX_BYTES;
                    slots = emit_slots(bytes);
                    assert!(
                        u64::from(slots) * VERTEX_BYTES <= bytes,
                        "grid {resolution}: slots past the buffer"
                    );
                    assert!(
                        slots as i32 >= 0 && slots.is_multiple_of(3),
                        "grid {resolution}: {slots}"
                    );
                    steps += 1;
                }
                // The kernel's last thread: idx < dispatch_count = slots.
                let grid_threads = u64::from(slots).div_ceil(256) * 256;
                assert!(
                    grid_threads - 1 < u64::from(u32::MAX),
                    "grid {resolution}: thread index wraps"
                );
                if f64::from(late) * 3.0 <= f64::from(slots) * GROW_AT {
                    assert!(
                        f64::from(late) * 3.0 <= f64::from(slots),
                        "grid {resolution}: fits without growing"
                    );
                }
                late *= 1.5;
            }
            assert!(
                steps > 0,
                "grid {resolution}: the worst case grows the buffer"
            );
            println!(
                "grid {resolution}: worst {worst} triangles, {steps} growths, final {slots} vertices"
            );
        }
    }

    #[test]
    fn growth_is_none_while_the_surface_fits() {
        assert_eq!(grown_capacity(0.0, 300), None);
        assert_eq!(grown_capacity(f32::NAN, 300), None);
        assert_eq!(
            grown_capacity(66.0, 300),
            None,
            "198 of 300 vertices is under two thirds"
        );
        let grown = grown_capacity(67.0, 300).expect("201 of 300 crowds it");
        assert_eq!(grown % BOUND_GRAIN, 0);
        assert!(grown >= 2 * 201);
    }

    #[test]
    fn emit_slots_never_pass_the_buffer() {
        for bytes in [0u64, 79, 80, 239, 240, 241, 80 * 1000 + 5] {
            let slots = u64::from(emit_slots(bytes));
            assert!(
                slots * VERTEX_BYTES <= bytes && slots.is_multiple_of(3),
                "{bytes} bytes: {slots}"
            );
        }
    }

    #[test]
    fn cell_owned_scan_intervals_are_disjoint_and_preserve_triangle_order() {
        let counts = [0u32, 2, 1, 4, 0, 3];
        let mut cursor = 0u32;
        let mut intervals = Vec::new();
        for count in counts {
            let start = cursor;
            cursor += count * 3;
            intervals.push((start, cursor));
        }
        for pair in intervals.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
        assert_eq!(cursor, 30);
        assert_eq!(intervals[1], (0, 6));
        assert_eq!(intervals[3], (9, 21));
        assert_eq!(intervals[5], (21, 30));
    }

    #[test]
    fn cell_owned_mesh_codegen_has_no_vertex_binary_search() {
        assert_eq!(
            std::mem::size_of::<MeshUniforms>(),
            20 * std::mem::size_of::<u32>()
        );
        let source = manifold_node_engine::freeze::codegen::standalone_for_spec::<VolumeSurfaceMesh>()
            .expect("cell-owned mesh standalone codegen");
        let module =
            naga::front::wgsl::parse_str(&source).expect("generated mesh kernel must parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("generated mesh kernel must validate");
        assert!(!source.contains("var lo"));
        assert!(source.contains("buf_vertices[(first + t) * 3u + corner]"));
        assert!(source.contains("liquid_cell_brick_index(gid.x)"));
        assert!(source.contains("if idx == 0xffffffffu"));
        assert!(source.contains("if brick_pass == 2u"));
        assert!(source.contains("buf_vertices[idx] = zero"));
        assert!(!source.contains("buf_vertices[idx] = body"));
        assert!(source.contains("solid_nodes_x"));
        assert!(source.contains("clamp_liquid_solid_at"));
        assert!(source.contains("min_mu = max(min_mu, eps)"));
    }

    pub(super) fn flip_vertex_interp(
        p1: [f64; 3],
        p2: [f64; 3],
        valp1: f64,
        valp2: f64,
        solid: Option<(f64, f64)>,
    ) -> [f64; 3] {
        let eps = 1e-10;
        let (mut min_mu, mut max_mu) = (0.0, 1.0);
        if let Some((s1, s2)) = solid
            && ((s1 < 0.0 && s2 >= 0.0) || (s2 < 0.0 && s1 >= 0.0))
        {
            let diff = s2 - s1;
            if diff.abs() > eps {
                let su = -s1 / diff;
                if s1 < 0.0 {
                    min_mu = su;
                } else {
                    max_mu = su;
                }
            } else {
                max_mu = min_mu;
            }
        }
        min_mu = min_mu.max(eps);
        max_mu = max_mu.min(1.0 - eps);
        let mut mu = -valp1 / (valp2 - valp1);
        if mu < min_mu {
            mu = min_mu;
        }
        if mu > max_mu {
            mu = max_mu;
        }
        std::array::from_fn(|axis| p1[axis] + mu * (p2[axis] - p1[axis]))
    }

    pub(super) struct ContactFixture {
        pub field: Vec<f32>,
        pub solid: Vec<f32>,
        pub scan: Vec<u32>,
        pub edge_scan: Vec<u32>,
        pub points: Vec<[f64; 3]>,
        pub indices: Vec<usize>,
    }

    /// Eight nodes per axis, unit spacing. The plate spans 0.625 cells in x;
    /// both faces cross edges. Values are binary fractions to isolate the
    /// endpoint constraint from sampling roundoff. Read topology from the
    /// engine's table, independently of the GPU table and scan.
    pub(super) fn contact_fixture(plate: bool) -> ContactFixture {
        use super::super::surface_mesh_parity::{CORNERS, EDGES, triangle_table};
        let n = 8usize;
        let flat = |p: [usize; 3]| p[0] + n * (p[1] + n * p[2]);
        let mut solid = Vec::new();
        let mut field = Vec::new();
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let wall = x as f32 + 0.5 * y as f32 + 0.25 * z as f32 - 4.125;
                    let distance = if plate { wall.abs() - 0.3125 } else { wall };
                    solid.push(distance);
                    let liquid = y as f32 - 4.25;
                    field.push(if distance < 0.0 { liquid.max(0.0) } else { liquid });
                }
            }
        }
        let mut points = Vec::new();
        let mut edge_scan = Vec::new();
        let mut edge_ids = std::collections::HashMap::new();
        let mut contacts = [0usize; 2];
        for i in 0..n.pow(3) {
            let a = [i % n, (i / n) % n, i / (n * n)];
            for axis in 0..3 {
                let mut b = a;
                b[axis] += 1;
                if b[axis] == n || (field[i] < 0.0) == (field[flat(b)] < 0.0) {
                    continue;
                }
                let (s1, s2) = (f64::from(solid[i]), f64::from(solid[flat(b)]));
                let point = flip_vertex_interp(a.map(|v| v as f64), b.map(|v| v as f64),
                    f64::from(field[i]), f64::from(field[flat(b)]), Some((s1, s2)));
                if (s1 < 0.0) != (s2 < 0.0) {
                    let root = -s1 / (s2 - s1);
                    let mu = point[axis] - a[axis] as f64;
                    if s1 < 0.0 {
                        assert!(mu >= root - 1e-14, "inside-to-open: {mu} < {root}");
                        contacts[0] += 1;
                    } else {
                        assert!(mu <= root + 1e-14, "open-to-inside: {mu} > {root}");
                        contacts[1] += 1;
                    }
                }
                edge_ids.insert((a, axis), points.len());
                points.push(point);
            }
            edge_scan.push(points.len() as u32);
        }
        assert!(contacts[0] > 0, "fixture must exercise contact");
        if plate { assert!(contacts[1] > 0, "both plate faces must be exercised"); }
        let mut scan = Vec::new();
        let mut indices = Vec::new();
        let table = triangle_table();
        for z in 0..n - 1 {
            for y in 0..n - 1 {
                for x in 0..n - 1 {
                    let cell = [x, y, z];
                    let corners = CORNERS.map(|o| std::array::from_fn(|d| cell[d] + o[d]));
                    let case = corners.iter().enumerate().fold(0usize, |bits, (j, p)|
                        bits | (usize::from(field[flat(*p)] < 0.0) << j));
                    for &e in table[case].iter().take_while(|&&e| e >= 0) {
                        let (a, b) = EDGES[e as usize];
                        let (a, b) = (corners[a], corners[b]);
                        let low = std::array::from_fn(|d| a[d].min(b[d]));
                        let axis = (0..3).find(|&d| a[d] != b[d]).unwrap();
                        indices.push(edge_ids[&(low, axis)]);
                    }
                    scan.push((indices.len() / 3) as u32);
                }
            }
        }
        ContactFixture { field, solid, scan, edge_scan, points, indices }
    }

    #[test]
    pub(super) fn mesh_contact_oblique_wall_and_thin_plate_cpu_reference_and_extents() {
        for plate in [false, true] {
            let f = contact_fixture(plate);
            // liquid/extent.rs volume_surface_mesh rules: node gathers cover
            // 8^3 nodes; scans cover 7^3 cells; outputs cover every scanned
            // triangle and edge. Rounded-up workgroups are idx-guarded.
            assert_eq!(f.field.len(), 8usize.pow(3));
            assert_eq!(f.solid.len(), f.field.len());
            assert_eq!(f.edge_scan.len(), f.field.len());
            assert_eq!(f.scan.len(), 7usize.pow(3));
            assert_eq!(f.scan.last().copied().unwrap() as usize * 3, f.indices.len());
            assert_eq!(f.edge_scan.last().copied().unwrap() as usize, f.points.len());
            assert!(f.points.len() <= f.indices.len());
            assert!(f.indices.iter().all(|&i| i < f.points.len()));
            let slots = emit_slots((f.indices.len() as u64 + 3) * VERTEX_BYTES);
            assert!(slots as usize >= f.indices.len());
            assert!(7u32.pow(3).div_ceil(256) * 256 < u32::MAX);
        }
    }

    #[test]
    fn flip_vertex_interp_clips_solid_crossings_and_endpoints() {
        let p1 = [0.0, 2.0, -1.0];
        let p2 = [4.0, 6.0, 3.0];
        let cases = [
            (0.1, None, 0.1),
            (0.1, Some((-0.25, 0.75)), 0.25),
            (0.9, Some((0.75, -0.25)), 0.75),
            (0.1, Some((-1.0e-12, 1.0e-12)), 0.0),
            (0.9, Some((1.0e-12, -1.0e-12)), 0.0),
            (0.1, Some((-0.25, 0.0)), 1.0 - 1e-10),
            (0.9, Some((0.0, -0.25)), 0.0),
            (0.4, Some((0.2, 0.3)), 0.4),
            (0.4, Some((-0.2, -0.3)), 0.4),
            (0.0, None, 1.0e-10),
            (1.0, None, 1.0 - 1.0e-10),
        ];
        for (surface_mu, solid, expected_mu) in cases {
            let got = flip_vertex_interp(p1, p2, -surface_mu, 1.0 - surface_mu, solid);
            let want: [f64; 3] = std::array::from_fn(|axis| p1[axis] + expected_mu * (p2[axis] - p1[axis]));
            for (actual, expected) in got.into_iter().zip(want) {
                assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
            }
        }
    }
}

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
mod gpu_tests {
    use super::tests::flip_vertex_interp;
    use super::VolumeSurfaceMesh;
    use manifold_node_engine::mesh::MeshVertex;
    use manifold_node_engine::testkit::array_harness::{params, read, Harness};
    use manifold_node_engine::parameters::ParamValue;

    #[test]
    fn mesh_contact_oblique_wall_and_thin_plate_match_cpu_reference() {
        // CPU extent proof runs before device acquisition, at this exact size.
        super::tests::mesh_contact_oblique_wall_and_thin_plate_cpu_reference_and_extents();
        let mut h = Harness::new();
        for plate in [false, true] {
            let f = super::tests::contact_fixture(plate);
            let (field, _) = h.array(&f.field, f.field.len());
            let (solid, _) = h.array(&f.solid, f.solid.len());
            let (scan, _) = h.array(&f.scan, f.scan.len());
            let (edges, _) = h.array(&f.edge_scan, f.edge_scan.len());
            for indexed in [false, true] {
                let capacity = f.indices.len() + 3;
                let (vertices, _) = h.array::<MeshVertex>(&[], capacity);
                let (indices, _) = h.array::<u32>(&[], capacity);
                let settings = params(&[
                    ("center_x", 3.5), ("center_y", 3.5), ("center_z", 3.5),
                    ("size_x", 7.0), ("size_y", 7.0), ("size_z", 7.0),
                    ("nodes_x", 8.0), ("nodes_y", 8.0), ("nodes_z", 8.0),
                    ("solid_nodes_x", 8.0), ("solid_nodes_y", 8.0), ("solid_nodes_z", 8.0),
                    ("max_capacity", capacity as f32), ("total", (f.indices.len() / 3) as f32),
                ]);
                let mut inputs = vec![("levelset", field), ("solid", solid), ("scan", scan)];
                if indexed { inputs.push(("edge_scan", edges)); }
                let (_, errors) = h.run(&mut VolumeSurfaceMesh::new(), &inputs,
                    &[("vertices", vertices), ("indices", indices)], &settings);
                assert!(errors.is_empty(), "{errors:?}");
                let got = read::<MeshVertex>(&h.buffer(vertices), capacity);
                let actual_indices = if indexed {
                    read::<u32>(&h.buffer(indices), f.indices.len())
                } else { (0..f.indices.len() as u32).collect() };
                for (i, (&actual, &expected)) in actual_indices.iter().zip(&f.indices).enumerate() {
                    for axis in 0..3 {
                        assert!((f64::from(got[actual as usize].position[axis]) - f.points[expected][axis]).abs() < 1e-6,
                            "plate={plate}, indexed={indexed}, vertex={i}, axis={axis}");
                    }
                    if indexed { assert_eq!(actual as usize, expected); }
                }
                let live = if indexed { f.points.len() } else { f.indices.len() };
                assert!(got[live..].iter().all(|v| v.position == [0.0; 3] && v.color == [0.0; 4]));
            }
        }
    }

    #[test]
    fn solid_endpoint_clip_matches_nested_f64_reference() {
        let mut harness = Harness::new();
        let levelset = vec![-0.1_f32, 0.9, 0.9, 0.9, 0.9, 0.9, 0.9, 0.9];
        let solid = vec![-0.25_f32, 0.75, 0.75, 0.75, 0.75, 0.75, 0.75, 0.75];
        let scan = vec![1_u32; 8];
        let (levelset_slot, _) = harness.array(&levelset, levelset.len());
        let (scan_slot, _) = harness.array(&scan, scan.len());
        let (solid_slot, _) = harness.array(&solid, solid.len());
        let (out_slot, _) = harness.array::<MeshVertex>(&[], 3);
        let base = [
            ("center_x", 0.0),
            ("center_y", 0.0),
            ("center_z", 0.0),
            ("size_x", 2.0),
            ("size_y", 2.0),
            ("size_z", 2.0),
            ("nodes_x", 2.0),
            ("nodes_y", 2.0),
            ("nodes_z", 2.0),
            ("total", 1.0),
        ];
        let mut run = |solid_input| {
            let mut params = params(&base);
            if solid_input {
                params.insert("solid_nodes_x".into(), ParamValue::Float(2.0));
                params.insert("solid_nodes_y".into(), ParamValue::Float(2.0));
                params.insert("solid_nodes_z".into(), ParamValue::Float(2.0));
            }
            let mut mesh = VolumeSurfaceMesh::new();
            let inputs = if solid_input {
                vec![("levelset", levelset_slot), ("scan", scan_slot), ("solid", solid_slot)]
            } else {
                vec![("levelset", levelset_slot), ("scan", scan_slot)]
            };
            let (_, errors) = harness.run(&mut mesh, &inputs, &[("vertices", out_slot)], &params);
            assert!(errors.is_empty(), "{errors:?}");
            let buffer = harness.buffer(out_slot);
            read::<MeshVertex>(&buffer, 1)[0].position
        };
        let unconstrained = run(false);
        let constrained = run(true);
        let expected_unconstrained = flip_vertex_interp(
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            -0.1,
            0.9,
            None,
        );
        let expected_constrained = flip_vertex_interp(
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            -0.1,
            0.9,
            Some((-0.25, 0.75)),
        );
        assert!((f64::from(unconstrained[0]) - expected_unconstrained[0]).abs() < 1e-6);
        assert!((f64::from(constrained[0]) - expected_constrained[0]).abs() < 1e-6);
        assert!(constrained[0] > unconstrained[0]);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
