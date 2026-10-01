//! `node.volume_surface_mesh` — the marching-cubes triangle list of a level
//! set, one thread per output vertex (GPU_FLUID_SURFACE_DESIGN.md D16). It
//! feeds `node.scene_object.vertices` exactly as the CPU fluid mesh does. A
//! per-element gather on the codegen path. With `extent` wired it dispatches
//! only over live and last frame's vertices and publishes the live extent
//! (GPU_FLUID_SURFACE_DESIGN.md P6b).
//!
//! The node owns its vertex buffer and grows it from the late triangle total
//! (BUG-j34u (surface mesh sized to worst case)): every copy downstream sizes
//! from it, so the mesh costs what the surface needs, not the lattice's worst
//! case. Growth is the only allocation; steady state allocates nothing.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use super::running_total::EXTENT_GRID_OFFSET;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::live_extent::LiveExtent;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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
    resolution_scale: i32,
    max_capacity: i32,
    dispatch_count: u32,
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
    Some((wanted.div_ceil(BOUND_GRAIN) * BOUND_GRAIN).min(INDEXABLE_VERTICES).max(u64::from(slots)))
}

crate::primitive! {
    name: VolumeSurfaceMesh,
    type_id: "node.volume_surface_mesh",
    purpose: "Build the triangle-list mesh of a level set's zero crossing (marching cubes): one output vertex per thread, placed by binary search over the running total of per-cell triangle counts, with a gradient normal pointing outward. Slots past the live triangles are zero. The vertex buffer starts at max_capacity (0: the lattice's box surface) and grows from the late triangle total; a frame whose surface outruns the buffer is an empty mesh and a named error, and the buffer grows.",
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
            label: "Starting Mesh Capacity (vertices)",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_215.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset and nodes_x/y/z from node.particle_volume, scan and total from node.running_total over node.count_surface_triangles, extent from the same running total with per_item 3, and the box from the lattice bounds through node.transform_components. resolution_scale must match the volume's; it places UVs on the authored domain (1.5 simulation cells inside the padded lattice) as the CPU fluid mesh does. Slots past the live triangles are zero. Wire total: the vertex buffer grows from it; unwired it holds only its start. Leave Starting Mesh Capacity at 0 so the start follows the lattice. With extent wired the mesh writes only live and last frame's vertices and publishes its live extent, so node.render_scene draws only live triangles. Wire vertices to node.scene_object like the CPU mesh.",
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
        // The vertex buffer this node owns and publishes.
        mesh: Option<GpuBuffer> = None,
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

impl VolumeSurfaceMesh {
    /// Grow the buffer to the start capacity (it rises with the lattice) or
    /// what the late total asks for. Grow-only; a refused allocation keeps
    /// the current buffer.
    fn ensure_capacity(&mut self, ctx: &mut EffectNodeContext<'_, '_>, total: f32, nodes: [f32; 3]) {
        let current = self.mesh.as_ref().map_or(0, |mesh| emit_slots(mesh.size));
        let wanted = grown_capacity(total, current).unwrap_or(0).max(start_capacity(ctx.params, nodes));
        if wanted <= u64::from(current) {
            return;
        }
        let target = (wanted.div_ceil(3) * 3).min(INDEXABLE_VERTICES);
        let device = ctx.gpu_encoder().device;
        match device.try_create_buffer_shared(target * VERTEX_BYTES) {
            Ok(buffer) => {
                buffer.zero_fill();
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
        port == "vertices"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "vertices").then_some(self.mesh.as_ref()).flatten()
    }

    /// Provided storage: a one-triangle hint, sized from the surface at run time.
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, _: &[(&str, u32)]) -> Option<u32> {
        (port == "vertices").then_some(3)
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
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        self.ensure_capacity(ctx, total, nodes);
        let resolution_scale = match ctx.params.get("resolution_scale") {
            Some(ParamValue::Float(n)) => n.round().clamp(1.0, 8.0) as i32,
            _ => 2,
        };
        let (Some(levelset), Some(scan), Some(vertices)) =
            (ctx.inputs.array("levelset"), ctx.inputs.array("scan"), self.mesh.as_ref())
        else {
            return;
        };
        let cells: u64 = nodes.iter().map(|&n| n.max(2.0) as u64 - 1).product();
        let node_total: u64 = nodes.iter().map(|&n| n.max(2.0) as u64).product();
        let lattice = nodes.iter().all(|&n| n >= 2.0);
        if lattice && (node_total > levelset.size / 4 || cells > scan.size / 4) {
            ctx.error("Volume Surface Mesh: the lattice is larger than its level set or running total");
            return;
        }
        // The kernel writes vertex idx only below this; it is the buffer's own size.
        let slots = emit_slots(vertices.size);
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
            resolution_scale,
            max_capacity: slots as i32,
            dispatch_count: slots,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: levelset, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: scan, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: extent.unwrap_or(scan), offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: vertices, offset: 0 },
        ];
        let pipeline = self.pipeline.as_ref().expect("pipeline built above");
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
            let mut slots = emit_slots(start_capacity(&ParamValues::default(), lattice) * VERTEX_BYTES);
            let mut steps = 0;
            let mut late = 1.0f32;
            while late <= worst {
                if let Some(target) = grown_capacity(late, slots) {
                    assert!(target > u64::from(slots) || target == INDEXABLE_VERTICES, "grid {resolution}: grows");
                    let bytes = target * VERTEX_BYTES;
                    slots = emit_slots(bytes);
                    assert!(u64::from(slots) * VERTEX_BYTES <= bytes, "grid {resolution}: slots past the buffer");
                    assert!(slots as i32 >= 0 && slots.is_multiple_of(3), "grid {resolution}: {slots}");
                    steps += 1;
                }
                // The kernel's last thread: idx < dispatch_count = slots.
                let grid_threads = u64::from(slots).div_ceil(256) * 256;
                assert!(grid_threads - 1 < u64::from(u32::MAX), "grid {resolution}: thread index wraps");
                if f64::from(late) * 3.0 <= f64::from(slots) * GROW_AT {
                    assert!(f64::from(late) * 3.0 <= f64::from(slots), "grid {resolution}: fits without growing");
                }
                late *= 1.5;
            }
            assert!(steps > 0, "grid {resolution}: the worst case grows the buffer");
            println!("grid {resolution}: worst {worst} triangles, {steps} growths, final {slots} vertices");
        }
    }

    #[test]
    fn growth_is_none_while_the_surface_fits() {
        assert_eq!(grown_capacity(0.0, 300), None);
        assert_eq!(grown_capacity(f32::NAN, 300), None);
        assert_eq!(grown_capacity(66.0, 300), None, "198 of 300 vertices is under two thirds");
        let grown = grown_capacity(67.0, 300).expect("201 of 300 crowds it");
        assert_eq!(grown % BOUND_GRAIN, 0);
        assert!(grown >= 2 * 201);
    }

    #[test]
    fn emit_slots_never_pass_the_buffer() {
        for bytes in [0u64, 79, 80, 239, 240, 241, 80 * 1000 + 5] {
            let slots = u64::from(emit_slots(bytes));
            assert!(slots * VERTEX_BYTES <= bytes && slots.is_multiple_of(3),"{bytes} bytes: {slots}");
        }
    }
}
