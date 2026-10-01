//! `node.sample_faces_at_particles` — liquid velocity at each particle from
//! the seam's face grid, by FLIP's MAC trilinear
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.3). A per-element atom on the
//! codegen path.
//!
//! Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::grid::{LIQUID_FACES, face_len};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, face_offset, particle_grid};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleUniforms {
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

const FACE_PORTS: [&str; 3] = ["face_u", "face_v", "face_w"];

crate::primitive! {
    name: SampleFacesAtParticles,
    type_id: "node.sample_faces_at_particles",
    purpose: "Sets each live liquid particle's velocity from a liquid's face grid, as FLIP's whitewater reads its velocity field: each component is the trilinear mix of the eight faces around the particle on that component's staggered stencil, a face past the face grid reads 0, and a particle outside the whitewater grid (the box center ± size/2 over nodes − 1 cells) gets 0. The face grid sits centred in that grid by a whole number of cells. Position, radius and id pass through; slots with radius 0 pass whole.",
    inputs: {
        particles: Array(FluidParticle) required,
        face_u: Array(f32) required,
        face_v: Array(f32) required,
        face_w: Array(f32) required,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("face_cells_x", "Face Cells X", 64.0, 1.0, 4096.0),
        float_param!("face_cells_y", "Face Cells Y", 64.0, 1.0, 4096.0),
        float_param!("face_cells_z", "Face Cells Z", 64.0, 1.0, 4096.0),
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.jitter_particles in the whitewater emitter chain. face_u/v/w and face_cells_x/y/z from a liquid frame's face grid; center/size from node.transform_components on the frame's grid_bounds (position and scale), nodes_x/y/z its grid_nodes_x/y/z. Feeds node.energy_potential, node.wavecrest_potential and node.emission_count.",
    examples: [],
    picker: { label: "Sample Faces at Particles", category: Atom },
    summary: "Reads the liquid's flow at each particle, so whitewater knows how fast and which way the water there is moving.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid velocity at particles", "sample velocity", "MAC trilinear"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_faces_at_particles_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for SampleFacesAtParticles {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| ctx.scalar_or_param(name, 64.0).round().max(0.0) as u32);
        let placed = particle_grid(ctx).and_then(|grid| face_offset(grid.nodes, face_cells).map(|_| grid));
        let grid = match placed {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Sample Faces at Particles: {refusal}"));
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else { return };
        let Some(out) = ctx.outputs.array("out") else { return };
        let faces = FACE_PORTS.map(|port| ctx.inputs.array(port));
        let [Some(u), Some(v), Some(w)] = faces else { return };
        for (axis, buffer) in [u, v, w].into_iter().enumerate() {
            if buffer.size < face_len(face_cells, axis) * 4 {
                ctx.error(format!(
                    "Sample Faces at Particles: {} holds fewer than the {face_cells:?}-cell grid's {} faces",
                    FACE_PORTS[axis],
                    face_len(face_cells, axis)
                ));
                return;
            }
        }
        let count = (particles.size.min(out.size) / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let [face_cells_x, face_cells_y, face_cells_z] = face_cells.map(|n| n as f32);
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = SampleUniforms {
            face_cells_x,
            face_cells_y,
            face_cells_z,
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: u, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: v, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: w, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.sample_faces_at_particles",
        );
    }
}
