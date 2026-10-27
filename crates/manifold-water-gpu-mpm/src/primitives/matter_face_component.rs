//! Uses the velocity-extrapolation layer count from FLIP Fluids fluidsimulation.cpp `_extrapolateFluidVelocities` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.matter_face_component` — one axis of the matter grid as the seam's
//! face array. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::matter::{MatterGridNode, grid_bytes};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_words};
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::lattice::PADDING_NODES;
use manifold_water_liquid::primitives::face_sample_component::{AXES, axis_param};

/// Face layers past the liquid whose velocity is the grid's own, counted as
/// FLIP's extrapolation counts them: a layer holds when every face in it
/// carries velocity. A point's quadratic stencil puts mass on every corner
/// node of its cell, so a liquid cell's own faces and the faces sharing an
/// edge with them always read grid velocity. The face one cell out along
/// its own axis reads it only when a point sits in the half of the cell
/// next to it: 67.7% of those faces in Dam Break at 64 after 45 ticks
/// (measured 2026-10 by a seam demo now in git history). So no layer holds.
pub const MATTER_FACE_VALID_LAYERS: u32 = 0;

/// Authored cells per axis of a matter lattice with `nodes` nodes.
pub fn matter_cells(nodes: [u32; 3]) -> Option<[u32; 3]> {
    let pad = 1 + 2 * PADDING_NODES;
    nodes
        .iter()
        .all(|&n| n > pad && n <= 4096)
        .then(|| nodes.map(|n| n - pad))
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ComponentUniforms {
    axis: u32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    pool_tiles: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: MatterFaceComponent,
    type_id: "node.matter_face_component",
    purpose: "Resample one axis of a matter grid onto the liquid seam's face array for that axis: each face of the authored box, (cells + 1) along the axis by cells on the other two, x fastest, reads the mean velocity of the four lattice nodes around its centre that carry mass, in m/s; 0 when none do. The lattice's three padding nodes per side are skipped.",
    inputs: {
        grid: Array(MatterGridNode) required,
        tiles: Array(u32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("axis"), label: "Axis", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: AXES },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "After the Live Matter region, on node.matter_state's grid (the last substep's velocities; held while paused). Three of them, one per axis, feed node.matter_frame's face_u_in, face_v_in and face_w_in; the lattice nodes come from node.matter_domain. Wire tiles and pool_tiles from the same step as grid, and lattice_min_x/y/z and cell_size from the domain: lattice node q is the world node round(lattice_min / cell_size) + q. out is sized to the axis's faces.",
    examples: [],
    picker: { label: "Matter Face Component", category: Atom },
    summary: "Hands one direction of the simulated liquid's velocity grid to effects that follow the liquid.",
    category: Particles3D,
    role: Filter,
    aliases: ["face grid", "grid velocity", "mac faces"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_face_component_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    output_capacity: manifold_node_engine::freeze::classify::FusedOutputCapacity::FromInput { input: "grid" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for MatterFaceComponent {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // One float per node of the pooled grid: the pool holds every node
        // of the lattice the wired node counts name, so every face fits.
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "grid")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 71.0).round().max(0.0) as u32);
        let (Some(cells), Some(axis)) = (matter_cells(nodes), axis_param(ctx.params)) else {
            ctx.error(format!("Matter Face Component: a {nodes:?} node lattice has no cells, or the axis is not X, Y or Z"));
            return;
        };
        let lattice_min = ["lattice_min_x", "lattice_min_y", "lattice_min_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if cell_size <= 0.0 {
            ctx.error(format!(
                "Matter Face Component: cell_size is {cell_size}; it must be above 0"
            ));
            return;
        }
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(grid), Some(tiles), Some(out)) = (
            ctx.inputs.array("grid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = face_len(cells, axis);
        if count * 4 > out.size
            || grid_bytes(pool_tiles) > grid.size
            || table_words(pool_tiles) * 4 > tiles.size
        {
            ctx.error(format!(
                "Matter Face Component: a {nodes:?} node lattice with {pool_tiles} pool tiles is larger than its arrays"
            ));
            return;
        }
        let uniforms = ComponentUniforms {
            axis: axis as u32,
            nodes_x: nodes[0] as i32,
            nodes_y: nodes[1] as i32,
            nodes_z: nodes[2] as i32,
            lattice_min_x: lattice_min[0],
            lattice_min_y: lattice_min[1],
            lattice_min_z: lattice_min[2],
            cell_size,
            pool_tiles: pool_tiles as i32,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.matter_face_component",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_face_component_generates_a_pooled_gather_kernel() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterFaceComponent>()
                .expect("matter_face_component codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }
}
