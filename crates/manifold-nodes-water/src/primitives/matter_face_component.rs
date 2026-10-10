//! Uses the velocity-extrapolation layer count from FLIP Fluids fluidsimulation.cpp `_extrapolateFluidVelocities` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.matter_face_component` — one axis of the matter grid as the seam's
//! face array (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.2 (Grid
//! outputs)). A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::face_sample_component::{AXES, axis_param};
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::liquid::grid::face_len;
use crate::liquid::lattice::PADDING_NODES;
use crate::matter::{MatterGridNode, grid_bytes};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

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
    nodes.iter().all(|&n| n > pad && n <= 4096).then(|| nodes.map(|n| n - pad))
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ComponentUniforms {
    axis: u32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: MatterFaceComponent,
    type_id: "node.matter_face_component",
    purpose: "Resample one axis of a matter grid onto the liquid seam's face array for that axis: each face of the authored box, (cells + 1) along the axis by cells on the other two, x fastest, reads the mean velocity of the four lattice nodes around its centre that carry mass, in m/s; 0 when none do. The lattice's three padding nodes per side are skipped.",
    inputs: {
        grid: Array(MatterGridNode) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("axis"), label: "Axis", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: AXES },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((8.0, 4096.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "After the Live Matter region, on node.matter_state's grid (the last substep's velocities; held while paused). Three of them, one per axis, feed node.matter_frame's face_u_in, face_v_in and face_w_in; the lattice nodes come from node.matter_domain. out is sized to the grid's node count, which is more than any axis's faces; the dispatch covers the faces.",
    examples: [],
    picker: { label: "Matter Face Component", category: Atom },
    summary: "Hands one direction of the simulated liquid's velocity grid to effects that follow the liquid.",
    category: Particles3D,
    role: Filter,
    aliases: ["face grid", "grid velocity", "mac faces"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_face_component_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for MatterFaceComponent {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "grid").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 71.0).round().max(0.0) as u32);
        let (Some(cells), Some(axis)) = (matter_cells(nodes), axis_param(ctx.params)) else {
            ctx.error(format!("Matter Face Component: a {nodes:?} node lattice has no cells, or the axis is not X, Y or Z"));
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(grid), Some(out)) = (ctx.inputs.array("grid"), ctx.outputs.array("out")) else {
            return;
        };
        let count = face_len(cells, axis);
        if count * 4 > out.size || grid_bytes(nodes) > grid.size {
            ctx.error(format!("Matter Face Component: a {nodes:?} node lattice is larger than its arrays"));
            return;
        }
        let uniforms = ComponentUniforms {
            axis: axis as u32,
            nodes_x: nodes[0] as i32,
            nodes_y: nodes[1] as i32,
            nodes_z: nodes[2] as i32,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.matter_face_component",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body skips the lattice's padding by a literal; it must be the
    /// lattice's.
    #[test]
    fn matter_face_component_body_skips_the_lattice_padding() {
        let body = include_str!("shaders/matter_face_component_body.wgsl");
        assert!(body.contains(&format!("let pad = {PADDING_NODES};")), "{body}");
        assert_eq!(matter_cells([71, 71, 71]), Some([64; 3]));
        assert_eq!(matter_cells([7, 71, 71]), None);
        assert_eq!(std::mem::size_of::<ComponentUniforms>(), 32);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
