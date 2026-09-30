//! `node.matter_move_bodies` — each body's pose at the end of the current
//! substep (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 2). Prescribed
//! bodies (colliders, P2a) follow their tick's motion exactly; dynamic
//! coupled bodies add the fluid's reaction in P2b.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::matter::MatterBody;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::MATTER_POSE;
use super::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MoveBodiesUniforms {
    tick_index: i32,
    first_tick: i32,
    substep_in_tick: i32,
    step_dt: f32,
    body_count: i32,
    rows: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: MatterMoveBodies,
    type_id: "node.matter_move_bodies",
    purpose: "Pose each matter body at the end of the current substep: from the domain's row for this tick (its pose at tick start and its linear and angular velocity over the tick), move it along the linear velocity for (substep_in_tick + 1) × step_dt and turn it by the constant angular velocity, which interpolates the tick's end poses exactly (lerp and slerp). Other fields pass through. A row past `rows` comes out disabled.",
    inputs: {
        bodies: Array(MatterBody) required,
        tick_index: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        bodies_out: Array(MatterBody),
    },
    params: [
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick This Frame", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_FLUID_ROLES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, before node.matter_to_grid. bodies, first_tick, body_count and rows come from node.matter_domain (one row per body per tick of this frame); tick_index, substep_in_tick and step_dt from node.matter_state. bodies_out feeds node.matter_grid_update's collider projection and node.matter_solid_distance.",
    examples: ["WaterDamBreakMatter"],
    picker: { label: "Matter Move Bodies", category: Atom },
    summary: "Moves the solid objects in a liquid to where they are at this instant of the simulation.",
    category: Particles3D,
    role: Filter,
    aliases: ["move colliders", "body poses", "prescribed motion"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_move_bodies_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [MATTER_POSE],
}

impl Primitive for MatterMoveBodies {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
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
        let bodies = ctx.inputs.array("bodies");
        let out = ctx.outputs.array("bodies_out");
        let gpu = ctx.gpu_encoder();
        let (Some(bodies), Some(out)) = (bodies, out) else {
            return;
        };
        let stride = std::mem::size_of::<MatterBody>() as u64;
        let rows = rows.min((bodies.size / stride).min(i32::MAX as u64) as i32);
        let count = (body_count as u32).min((out.size / stride) as u32);
        if count == 0 {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MoveBodiesUniforms {
            tick_index,
            first_tick,
            substep_in_tick,
            step_dt,
            body_count,
            rows,
            dispatch_count: count,
            _pad0: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
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
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatterMoveBodies>()
            .expect("matter_move_bodies codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("var<storage, read> buf_bodies: array<Element>"), "{wgsl}");
        assert!(wgsl.contains("buf_bodies_out[idx] = body(idx, params.dispatch_count,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<MoveBodiesUniforms>(), 32);
    }
}
