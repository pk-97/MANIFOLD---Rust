//! `node.faces_to_particles` — the face grid back to the particles and one
//! RK3 move (docs/FFT_WATER_SOLVER_DESIGN.md D7, section 3 step 9): PIC/FLIP
//! blended velocity, advection through the projected field, clamp to the
//! box. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::collar_cells::cell_lattice;
use super::face_gravity::DEFAULT_STEP_DT;
use super::particles_to_faces::{face_count, lattice_box};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AdvectUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    step_dt: f32,
    flip: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: FacesToParticles,
    type_id: "node.faces_to_particles",
    purpose: "Move liquid particles one step through a face grid (node.particles_to_faces' layout). Each live particle (radius > 0) samples `faces` and `old` trilinearly per component over the faces with weight > 0; its velocity becomes flip · (v + faces(x) − old(x)) + (1 − flip) · faces(x). It then moves by third-order Runge–Kutta through `faces` for step_dt and is clamped inside the lattice box. Radius and id are kept; unused slots pass through.",
    inputs: {
        particles: Array(FluidParticle) required,
        faces: Array(FaceSample) required,
        old: Array(FaceSample) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        flip: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1.0e4, 1.0e4),
        float_param!("step_dt", "Step (s)", DEFAULT_STEP_DT, 1.0e-5, 0.1),
        float_param!("flip", "FLIP Blend", 0.95, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The last atom of an FFT water step. particles is the sort's sorted output (the order node.particles_to_faces read), faces the projected grid after node.extend_faces, old node.particles_to_faces' output extended the same way (before gravity: the FLIP change includes gravity and pressure). flip 1 keeps detail and noise, 0 is smooth and viscous; 0.95 is the usual blend.",
    examples: [],
    picker: { label: "Faces To Particles", category: Atom },
    summary: "Hands the grid's corrected motion back to the liquid particles and moves them one step.",
    category: Particles3D,
    role: Filter,
    aliases: ["grid to particle", "g2p", "advect", "flip"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/faces_to_particles_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
}

impl Primitive for FacesToParticles {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Faces To Particles: every lattice length must be 1 to 1024".to_string());
            return;
        };
        if nodes.iter().any(|&n| n < 2) {
            ctx.error("Faces To Particles: every lattice length must be at least 2".to_string());
            return;
        }
        let (min, cell_size) = lattice_box(ctx);
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let flip = ctx.scalar_or_param("flip", 0.95).clamp(0.0, 1.0);
        if !(cell_size.is_finite() && cell_size > 0.0 && step_dt.is_finite() && step_dt >= 0.0)
            || min.iter().any(|v| !v.is_finite())
        {
            ctx.error("Faces To Particles: the lattice box and step must be finite, the cell positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(particles), Some(faces), Some(old), Some(out)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("old"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let record = std::mem::size_of::<FluidParticle>() as u64;
        let count = (particles.size.min(out.size) / record) as u32;
        if face_count(nodes) * 32 > faces.size.min(old.size) {
            ctx.error(format!("Faces To Particles: a {nodes:?} lattice is larger than its face grids"));
            return;
        }
        if count == 0 {
            return;
        }
        let uniforms = AdvectUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            step_dt,
            flip,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: old, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.faces_to_particles",
        );
    }
}
