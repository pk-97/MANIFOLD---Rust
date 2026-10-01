//! `node.face_gravity` — the body forces and the box walls on the face grid
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)), before the pressure
//! solve: gravity, the scene's forces and impulses (seam P8), then the walls.
//! A per-element atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_lattice};
use super::particles_to_faces::{face_capacity, face_count, lattice_box};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::fields::{FieldBinding, LIQUID_FIELD};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// The step length every GPU FLIP step atom defaults to: two steps per 60 fps
/// frame.
pub(super) const DEFAULT_STEP_DT: f32 = 1.0 / 120.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
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
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    tick_index: i32,
    substep_in_tick: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: FaceGravity,
    type_id: "node.face_gravity",
    purpose: "Add the body forces to a face grid (node.particles_to_faces' layout) for one step: every face gains (gravity + the scene's acceleration field) × step_dt along its normal, the field read from a coarse force lattice at the face's centre; on the first step of the impulse tick it also gains the scene's impulses, from a coarse impulse lattice. A box wall face (the first and last along each axis) then keeps only the part leaving the wall, so water may leave a wall and never enter it. Weights pass through.",
    inputs: {
        faces: Array(FaceSample) required,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
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
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1.0e4, 1.0e4),
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Step in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_x"), label: "Field Nodes X", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_y"), label: "Field Nodes Y", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_z"), label: "Field Nodes Z", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_spacing"), label: "Field Spacing", ty: ParamType::Float, default: ParamValue::Float(0.25), range: Some((1.0e-4, 400.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("force_lattices"), label: "Force Lattices", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("impulse_tick"), label: "Impulse Tick", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "After node.particles_to_faces, before node.face_divergence, on the same lattice box (cell_size, lattice_min). Use the same step_dt as node.faces_to_particles in the same step. Gravity, forces, impulses and the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick) come from node.gpu_flip_domain, whose field lattices start at the box minimum; tick_index from node.liquid_state, substep_in_tick is the step's index in the tick. Every step adds the force lattice of its tick: one lattice per tick from first_tick, or one for all ticks when force_lattices is 1 (0: no forces); impulses apply once, on step 0 of tick impulse_tick (−1: none). With forces or impulses unwired neither is read.",
    examples: [],
    picker: { label: "Face Gravity", category: Atom },
    summary: "Pulls the liquid with gravity and the scene's forces for one step and stops it going through the tank walls.",
    category: Particles3D,
    role: Filter,
    aliases: ["gravity", "body force", "walls", "forces", "impulses"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/face_gravity_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    // The face grid of its own lattice, like every other face atom of the
    // step, so a fused projection or constraint counts the same faces.
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
    wgsl_includes: [LIQUID_FIELD],
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
        let (min, cell_size) = lattice_box(ctx);
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let substep_in_tick = ctx.scalar_or_param("substep_in_tick", 0.0).round().max(0.0) as i32;
        let field = FieldBinding::read(ctx, ctx.inputs.array("forces"), ctx.inputs.array("impulses"), "Face Gravity");
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(out)) = (ctx.inputs.array("faces"), ctx.outputs.array("out")) else {
            return;
        };
        let field = match field {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let reads_field = field.force_lattices > 0 || field.impulse_tick >= 0;
        if reads_field && (!(cell_size.is_finite() && cell_size > 0.0) || min.iter().any(|v| !v.is_finite())) {
            ctx.error("Face Gravity: the lattice box must be finite with a positive cell".to_string());
            return;
        }
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
            cell_size,
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            tick_index,
            substep_in_tick,
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            force_lattices: field.force_lattices,
            impulse_tick: field.impulse_tick,
            first_tick: field.first_tick,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        // An unwired lattice is never read (force_lattices 0, impulse_tick −1).
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: field.forces.unwrap_or(faces), offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: field.impulses.unwrap_or(faces), offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.face_gravity",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_gravity_generates_a_field_reading_face_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<FaceGravity>()
            .expect("face_gravity codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert_eq!(std::mem::size_of::<GravityUniforms>(), 96);
        assert!(wgsl.contains("first_tick: i32,\n    dispatch_count: u32,\n    _pad0: u32,"), "{wgsl}");
        for binding in ["buf_forces: array<f32>", "buf_impulses: array<f32>"] {
            assert!(wgsl.contains(binding), "{binding}: {wgsl}");
        }
    }
}
