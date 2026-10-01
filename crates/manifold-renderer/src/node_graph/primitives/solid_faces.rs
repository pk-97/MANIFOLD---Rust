//! Ported from FLIP Fluids levelsetutils.cpp and meshlevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.solid_faces` — each face's open fraction from the solid distance
//! lattice (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water)):
//! the weights of the water's weighted pressure operator, FLIP Fluids' face
//! weights ported line by line. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_lattice};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use super::particles_to_faces::{face_capacity, face_count};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidFacesUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    box_offset: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SolidFaces,
    type_id: "node.solid_faces",
    purpose: "The open fraction of every face of a face grid (node.particles_to_faces' layout, nodes_x/y/z cells) from a solid distance lattice on the cell corners (solid: (nodes + 1) per axis from the box's lowest corner, negative inside a solid). Each face's weight is 1 − the fraction of it inside the solid, from its four corners (FLIP Fluids' fractionInside; a face on the interface at all four corners, within 8 f32 epsilons of cell_size · the longest side + box_offset, is half open), clamped to 0 to 1. Box wall faces and faces past the lattice are 0. Weight w of a cell's record is the cell's open volume, 1 − the fraction of it inside the solid from its eight corners (FLIP Fluids' volumeFraction; 0 past the lattice). Velocity is 0.",
    inputs: {
        solid: Array(f32) required,
    },
    outputs: {
        out: Array(FaceSample),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("box_offset", "Box Offset", 2.0, 0.0, 1.0e4),
    ],
    depth_rule: Terminal,
    composition_notes: "Once per GPU FLIP water step, from node.liquid_solid_distance on the box's corner lattice (lattice_min the box's lowest corner, nodes the cells + 1, closed_faces 0: the box walls are this atom's). Feeds node.pressure_smooth, node.pressure_residual, node.coarse_inverse, node.face_divergence and node.subtract_pressure on the finest level; node.coarsen_solid_faces makes each coarser level's.",
    examples: [],
    picker: { label: "Solid Faces", category: Atom },
    summary: "Works out how much of each grid face is blocked by solid objects, so water flows around them.",
    category: Particles3D,
    role: Filter,
    aliases: ["face weights", "open fraction", "solid fraction", "cut cells"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/solid_faces_body.wgsl"),
    input_access: [BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
}

impl Primitive for SolidFaces {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Solid Faces: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let box_offset = ctx.scalar_or_param("box_offset", 2.0);
        if !(cell_size.is_finite() && cell_size > 0.0 && box_offset.is_finite() && box_offset >= 0.0) {
            ctx.error("Solid Faces: cell_size must be positive and box_offset finite".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(solid), Some(out)) = (ctx.inputs.array("solid"), ctx.outputs.array("out")) else {
            return;
        };
        let faces = face_count(nodes);
        if faces * 4 > solid.size || faces * size_of::<FaceSample>() as u64 > out.size {
            ctx.error(format!("Solid Faces: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = SolidFacesUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            box_offset,
            dispatch_count: faces as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(faces as u32).div_ceil(256), 1, 1],
            "node.solid_faces",
        );
    }
}
