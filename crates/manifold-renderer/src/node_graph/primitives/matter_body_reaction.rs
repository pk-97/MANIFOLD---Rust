//! `node.matter_body_reaction` — gather what the grid's collider projection
//! took from the liquid into each dynamic body's reaction words
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 5, D4).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::fields::LIQUID_FIELD;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::{MatterGridNode, REACTION_WORDS, momentum_unit_fits};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::MATTER_WALLS;
use super::matter_grid_update::FieldBinding;
use super::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BodyReactionUniforms {
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    closed_faces: i32,
    momentum_unit: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    body_count: i32,
    tick_index: i32,
    substep_in_tick: i32,
    substeps_per_tick: i32,
    dynamic_count: i32,
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
}

crate::primitive! {
    name: MatterBodyReaction,
    type_id: "node.matter_body_reaction",
    purpose: "Add the liquid's push on each dynamic body for this substep: repeat the grid's collider projection node by node and atomically add the momentum each body took from the node (and its turning moment about the body's centre of mass) to that body's reaction words, plus the same terms weighted by the substep's place in the tick.",
    inputs: {
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        reaction: Array(i32) optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        body_count: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        dynamic_count: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
    },
    outputs: {
        reaction_out: Array(i32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substeps_per_tick"), label: "Substeps per Tick", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dynamic_count"), label: "Dynamic Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_x"), label: "Field Nodes X", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_y"), label: "Field Nodes Y", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_z"), label: "Field Nodes Z", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_spacing"), label: "Field Spacing", ty: ParamType::Float, default: ParamValue::Float(0.25), range: Some((1.0e-4, 400.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("force_lattices"), label: "Force Lattices", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("impulse_tick"), label: "Impulse Tick", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.matter_grid_update (its grid_out is this grid) and before node.grid_to_matter. reaction/reaction_out alias node.matter_domain's reaction array (16 words per body, the slot the domain cleared for this tick and reads back fenced); reaction_out feeds node.grid_to_matter's reaction, whose reaction_out closes into node.matter_state's reaction_in so the region runs this node every substep; bodies from node.matter_move_bodies; shapes, atlas, the lattice, gravity, closed faces, momentum_unit, body_count, dynamic_count and substeps_per_tick from node.matter_domain; tick_index, substep_in_tick and step_dt from node.matter_state; forces, impulses and the field scalars from node.matter_domain, wired exactly as node.matter_grid_update's so both start from the same velocity. Skips its dispatch when dynamic_count is 0 or reaction is unwired.",
    examples: ["WaterFloatingBoxMatter", "WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Body Reaction", category: Atom },
    summary: "Measures how hard the liquid pushes on each floating object so the physics world can move it.",
    category: Particles3D,
    role: Filter,
    aliases: ["body reaction", "two-way coupling", "buoyancy", "fluid force"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/matter_body_reaction_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER, MATTER_WALLS, LIQUID_FIELD],
    atomic_outputs: ["reaction_out"],
}

impl Primitive for MatterBodyReaction {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "reaction_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "reaction").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let lattice = LiquidLattice::from_wires(ctx);
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as i32;
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let closed_faces = int(ctx, "closed_faces", 63.0).min(63);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let body_count = int(ctx, "body_count", 0.0).min(MAX_FLUID_ROLES as i32);
        let tick_index = int(ctx, "tick_index", 0.0);
        let substep_in_tick = int(ctx, "substep_in_tick", 0.0);
        let substeps_per_tick = int(ctx, "substeps_per_tick", 1.0).max(1);
        let dynamic_count = int(ctx, "dynamic_count", 0.0);
        let grid = ctx.inputs.array("grid");
        let colliders = (ctx.inputs.array("bodies"), ctx.inputs.array("shapes"), ctx.inputs.array("atlas"));
        let reaction = ctx.outputs.array("reaction_out");
        let field = FieldBinding::read(ctx, ctx.inputs.array("forces"), ctx.inputs.array("impulses"), "Matter Body Reaction");
        // The GPU is touched on every path so the aliased reaction array keeps
        // its place in the frame's hazard order.
        let gpu = ctx.gpu_encoder();
        let (Some(grid), (Some(bodies), Some(shapes), Some(atlas)), Some(reaction)) = (grid, colliders, reaction) else {
            return;
        };
        if dynamic_count == 0 || body_count == 0 || step_dt <= 0.0 {
            return;
        }
        let field = match field {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let nodes = lattice.node_count().min((grid.size / std::mem::size_of::<MatterGridNode>() as u64) as u32);
        let body_rows = (bodies.size / std::mem::size_of::<LiquidBody>() as u64)
            .min(reaction.size / (REACTION_WORDS as u64 * 4)) as i32;
        let body_count = body_count.min(body_rows);
        if nodes == 0 || body_count == 0 {
            return;
        }
        if !momentum_unit_fits(momentum_unit, lattice.cell_size(), step_dt) {
            ctx.error(format!(
                "Matter Body Reaction: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = BodyReactionUniforms {
            nodes_x: lattice.nodes()[0] as i32,
            nodes_y: lattice.nodes()[1] as i32,
            nodes_z: lattice.nodes()[2] as i32,
            cell_size: lattice.cell_size(),
            step_dt,
            gravity_x: gravity[0],
            gravity: gravity[1],
            gravity_z: gravity[2],
            closed_faces,
            momentum_unit,
            lattice_min_x: lattice.min()[0],
            lattice_min_y: lattice.min()[1],
            lattice_min_z: lattice.min()[2],
            body_count,
            tick_index,
            substep_in_tick,
            substeps_per_tick,
            dynamic_count,
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            force_lattices: field.force_lattices,
            impulse_tick: field.impulse_tick,
            first_tick: field.first_tick,
            dispatch_count: nodes,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: reaction, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: field.forces.unwrap_or(grid), offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: field.impulses.unwrap_or(grid), offset: 0 },
                GpuBinding::Buffer { binding: 8, buffer: reaction, offset: 0 },
            ],
            [nodes.div_ceil(256), 1, 1],
            "node.matter_body_reaction",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_body_reaction_generates_an_atomic_node_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatterBodyReaction>()
            .expect("matter_body_reaction codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("buf_reaction_out: array<atomic<i32>>"), "{wgsl}");
        assert!(wgsl.contains("    body(idx, params.dispatch_count, e_grid,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<BodyReactionUniforms>(), 112);
        assert!(wgsl.contains("first_tick: i32,\n    dispatch_count: u32,\n    _pad0: u32,\n    _pad1: u32,\n}"), "{wgsl}");
        assert!(wgsl.contains("buf_forces: array<f32>") && wgsl.contains("buf_impulses: array<f32>"), "{wgsl}");
    }
}
