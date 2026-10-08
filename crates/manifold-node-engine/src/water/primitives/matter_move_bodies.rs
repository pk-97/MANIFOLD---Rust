//! `node.matter_move_bodies` — each body's pose at the end of the current
//! substep (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 2). Prescribed
//! bodies follow their tick's motion exactly; dynamic coupled bodies step
//! from their tick-start state with their accelerations and the liquid's
//! reaction so far.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::exec::effect_node::EffectNodeContext;
use crate::water::fluid_role::MAX_FLUID_ROLES;
use crate::water::liquid::bodies::{LIQUID_POSE, LiquidBody};
use crate::water::matter::REACTION_WORDS;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;
use crate::primitives::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MoveBodiesUniforms {
    tick_index: i32,
    first_tick: i32,
    substep_in_tick: i32,
    step_dt: f32,
    body_count: i32,
    rows: i32,
    substeps_per_tick: i32,
    momentum_unit: f32,
    cell_size: f32,
    dynamic_count: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: MatterMoveBodies,
    type_id: "node.matter_move_bodies",
    purpose: "Pose each matter body at the end of the current substep from the domain's row for this tick. A prescribed body moves along its linear velocity for (substep_in_tick + 1) × step_dt and turns by its constant angular velocity, which interpolates the tick's end poses exactly (lerp and slerp). A dynamic coupled body (inverse mass above 0) steps from its tick-start state with its external accelerations and the liquid's reaction from the substeps before this one, and carries its current velocities out. Other fields pass through. A row past `rows` comes out disabled.",
    inputs: {
        bodies: Array(LiquidBody) required,
        reaction: Array(i32) optional,
        tick_index: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        dynamic_count: ScalarF32 optional,
    },
    outputs: {
        bodies_out: Array(LiquidBody),
    },
    params: [
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick This Frame", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_FLUID_ROLES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substeps_per_tick"), label: "Substeps per Tick", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dynamic_count"), label: "Dynamic Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_FLUID_ROLES as f32)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, before node.matter_to_grid. bodies, first_tick, body_count, rows, substeps_per_tick, momentum_unit, cell_size, dynamic_count and reaction come from node.matter_domain (one row per body per tick of this frame; reaction is the tick's slot that node.matter_body_reaction adds to later in each substep); tick_index, substep_in_tick and step_dt from node.matter_state. bodies_out feeds node.matter_grid_update's collider projection, node.matter_body_reaction and node.liquid_solid_distance. reaction is read only when dynamic_count is above 0.",
    examples: ["WaterDamBreakMatter", "WaterFloatingBoxMatter"],
    picker: { label: "Matter Move Bodies", category: Atom },
    summary: "Moves the solid objects in a liquid to where they are at this instant of the simulation.",
    category: Particles3D,
    role: Filter,
    aliases: ["move colliders", "body poses", "prescribed motion"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_move_bodies_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE],
}

impl Primitive for MatterMoveBodies {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "bodies_out").then_some(MAX_FLUID_ROLES as u32)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str| ctx.scalar_or_param(name, 0.0).round().max(0.0) as i32;
        let tick_index = int(ctx, "tick_index");
        let first_tick = int(ctx, "first_tick");
        let substep_in_tick = int(ctx, "substep_in_tick");
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let body_count = int(ctx, "body_count").min(MAX_FLUID_ROLES as i32);
        let rows = int(ctx, "rows");
        let substeps_per_tick = int(ctx, "substeps_per_tick").max(1);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let mut dynamic_count = int(ctx, "dynamic_count");
        let bodies = ctx.inputs.array("bodies");
        let reaction = ctx.inputs.array("reaction");
        let out = ctx.outputs.array("bodies_out");
        let gpu = ctx.gpu_encoder();
        let (Some(bodies), Some(out)) = (bodies, out) else {
            return;
        };
        let stride = std::mem::size_of::<LiquidBody>() as u64;
        let rows = rows.min((bodies.size / stride).min(i32::MAX as u64) as i32);
        let count = (body_count as u32).min((out.size / stride) as u32);
        if count == 0 {
            return;
        }
        // Without a reaction slot covering every body the dynamic path runs
        // on its accelerations alone; the binding then stands in with bodies.
        let covered = reaction.is_some_and(|r| r.size >= u64::from(count) * u64::from(REACTION_WORDS) * 4);
        if !covered {
            dynamic_count = 0;
        }
        let reaction = reaction.filter(|_| covered).unwrap_or(bodies);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MoveBodiesUniforms {
            tick_index,
            first_tick,
            substep_in_tick,
            step_dt,
            body_count,
            rows,
            substeps_per_tick,
            momentum_unit,
            cell_size,
            dynamic_count,
            dispatch_count: count,
            _pad0: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: reaction, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.matter_move_bodies",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_move_bodies_generates_a_gathering_body_kernel() {
        let wgsl = crate::freeze::codegen::standalone_for_spec::<MatterMoveBodies>()
            .expect("matter_move_bodies codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("var<storage, read> buf_bodies: array<Element>"), "{wgsl}");
        assert!(wgsl.contains("buf_bodies_out[idx] = body(idx, params.dispatch_count,"), "{wgsl}");
        assert!(wgsl.contains("var<storage, read> buf_reaction: array<i32>"), "{wgsl}");
        assert_eq!(std::mem::size_of::<MoveBodiesUniforms>(), 48);
        assert!(wgsl.contains("dynamic_count: i32,\n    dispatch_count: u32,\n    _pad0: u32,"), "{wgsl}");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
