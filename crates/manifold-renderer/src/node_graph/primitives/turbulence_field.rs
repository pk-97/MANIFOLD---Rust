//! FLIP cell-centre turbulence from the MAC velocity field; per-element gather.
//! Ported from FLIP Fluids turbulencefield.cpp (MIT); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TurbulenceUniforms {
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: TurbulenceField,
    type_id: "node.turbulence_field",
    purpose: "FLIP turbulence at each liquid cell: sum cell-centre MAC velocity differences over the asymmetric i-2 through i+1 window, excluding the last boundary index, weighted by direction and distance within sqrt(12) cells. Air cells are zero.",
    inputs: {
        distance: Array(f32) required,
        face_u: Array(f32) required, face_v: Array(f32) required, face_w: Array(f32) required,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("face_cells_x", "Face Cells X", 64.0, 1.0, 4096.0),
        float_param!("face_cells_y", "Face Cells Y", 64.0, 1.0, 4096.0),
        float_param!("face_cells_z", "Face Cells Z", 64.0, 1.0, 4096.0),

        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feed cell-centred liquid distance and seam MAC faces; inside_turbulence_potential samples this field for bubble emission. The field materializes before particle sampling.",
    examples: [],
    summary: "Measures local liquid agitation for whitewater emission.",
    category: Particles3D,
    role: Filter,
    aliases: ["turbulence", "liquid agitation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/turbulence_field_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: crate::node_graph::freeze::classify::FusedOutputCapacity::FromInput { input: "distance" },
    wgsl_includes: [WHITEWATER_COMMON, crate::node_graph::liquid::grid::LIQUID_FACES],
}

impl Primitive for TurbulenceField {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "distance")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!(
                "Turbulence Field: a {nodes:?} solid lattice has too few or too many nodes"
            ));
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size > 0.0 && cell_size.is_finite()) {
            ctx.error(format!(
                "Turbulence Field: cell size {cell_size} is not a length"
            ));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(out)) = (ctx.inputs.array("distance"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = cell_total(cells);
        if count * 4 > distance.size || count * 4 > out.size {
            ctx.error(format!(
                "Turbulence Field: a {nodes:?}-node grid is larger than its arrays"
            ));
            return;
        }
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"]
            .map(|name| ctx.scalar_or_param(name, 64.0).round().max(0.0) as u32);
        if let Err(error) = crate::node_graph::whitewater::face_offset(nodes, face_cells) {
            ctx.error(format!("Turbulence Field: {error}"));
            return;
        }
        let [Some(u), Some(v), Some(w)] =
            ["face_u", "face_v", "face_w"].map(|name| ctx.inputs.array(name))
        else {
            return;
        };
        for (axis, face) in [u, v, w].into_iter().enumerate() {
            if face.size < crate::node_graph::liquid::grid::face_len(face_cells, axis) * 4 {
                ctx.error("Turbulence Field: face array is shorter than its grid".to_owned());
                return;
            }
        }
        let uniforms = TurbulenceUniforms {
            face_cells_x: face_cells[0] as f32,
            face_cells_y: face_cells[1] as f32,
            face_cells_z: face_cells[2] as f32,
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            dispatch_count: count as u32,
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
                    buffer: distance,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: u,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: v,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: w,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.turbulence_field",
        );
    }
}
