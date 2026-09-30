//! `node.face_sample_component` — one axis of SWASH's face lattice as the
//! seam's face array (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.2 (Grid
//! outputs)). A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::cell_lattice;
use super::particles_to_faces::face_count;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub(super) const AXES: &[&str] = &["X (u)", "Y (v)", "Z (w)"];

/// The `axis` param: 0, 1 or 2.
pub(crate) fn axis_param(params: &ParamValues) -> Option<usize> {
    match params.get("axis") {
        Some(ParamValue::Enum(a)) => Some(*a as usize),
        None => Some(0),
        _ => None,
    }
    .filter(|&a| a < 3)
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ComponentUniforms {
    axis: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: FaceSampleComponent,
    type_id: "node.face_sample_component",
    purpose: "Copy one axis of a face grid (node.particles_to_faces' layout, one FaceSample per padded cell) into the liquid seam's face array for that axis: (cells + 1) along the axis by cells on the other two, x fastest, velocity in m/s. Faces with weight 0 read 0.",
    inputs: {
        faces: Array(FaceSample) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("axis"), label: "Axis", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: AXES },
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Three of them, one per axis, on the last water step's extended faces publish SWASH's face grid (face_u, face_v, face_w) for whitewater and any other consumer of the liquid seam. The lattice params match the water step's.",
    examples: [],
    picker: { label: "Face Grid Component", category: Atom },
    summary: "Hands one direction of the water's velocity grid to effects that follow the water.",
    category: Particles3D,
    role: Filter,
    aliases: ["face grid", "velocity component", "mac faces"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/face_sample_component_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for FaceSampleComponent {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        if port != "out" {
            return None;
        }
        let (cells, axis) = (cell_lattice(params)?, axis_param(params)?);
        u32::try_from(face_len(cells, axis)).ok()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(nodes), Some(axis)) = (cell_lattice(ctx.params), axis_param(ctx.params)) else {
            ctx.error("Face Grid Component: every lattice length must be 1 to 1024 and the axis X, Y or Z".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(out)) = (ctx.inputs.array("faces"), ctx.outputs.array("out")) else {
            return;
        };
        let count = face_len(nodes, axis);
        if count * 4 > out.size || face_count(nodes) * 32 > faces.size {
            ctx.error(format!("Face Grid Component: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ComponentUniforms {
            axis: axis as u32,
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
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
                GpuBinding::Buffer { binding: 1, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.face_sample_component",
        );
    }
}
