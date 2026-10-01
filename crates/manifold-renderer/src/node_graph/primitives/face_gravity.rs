//! `node.face_gravity` — the body force and the box walls on the face grid
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)), before the pressure
//! solve. A per-element atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::cell_lattice;
use super::particles_to_faces::{face_capacity, face_count};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// The step length every GPU FLIP step atom defaults to: two steps per 60 fps
/// frame.
pub(super) const DEFAULT_STEP_DT: f32 = 1.0 / 120.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GravityUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    step_dt: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: FaceGravity,
    type_id: "node.face_gravity",
    purpose: "Add gravity to a face grid (node.particles_to_faces' layout) for one step: every face gains gravity × step_dt along its normal; a box wall face (the first and last along each axis) then keeps only the part leaving the wall, so water may leave a wall and never enter it. Weights pass through.",
    inputs: {
        faces: Array(FaceSample) required,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        step_dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(FaceSample),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("gravity_x", "Gravity X", 0.0, -100.0, 100.0),
        float_param!("gravity_y", "Gravity Y", -9.81, -100.0, 100.0),
        float_param!("gravity_z", "Gravity Z", 0.0, -100.0, 100.0),
        float_param!("step_dt", "Step (s)", DEFAULT_STEP_DT, 1.0e-5, 0.1),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.particles_to_faces, before node.face_divergence. Use the same step_dt as node.faces_to_particles in the same step.",
    examples: [],
    picker: { label: "Face Gravity", category: Atom },
    summary: "Pulls the liquid down for one step and stops it going through the tank walls.",
    category: Particles3D,
    role: Filter,
    aliases: ["gravity", "body force", "walls"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/face_gravity_body.wgsl"),
    input_access: [Coincident],
}

impl Primitive for FaceGravity {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Face Gravity: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)]
            .map(|(name, default)| ctx.scalar_or_param(name, default));
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(out)) = (ctx.inputs.array("faces"), ctx.outputs.array("out")) else {
            return;
        };
        let count = face_count(nodes);
        if count * 32 > faces.size.min(out.size) {
            ctx.error(format!("Face Gravity: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = GravityUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            gravity_x: gravity[0],
            gravity_y: gravity[1],
            gravity_z: gravity[2],
            step_dt,
            dispatch_count: count as u32,
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
            "node.face_gravity",
        );
    }
}
