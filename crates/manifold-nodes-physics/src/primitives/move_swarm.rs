//! `node.move_swarm` — advance each swarm particle through the frame's fixed
//! ticks under the scene's force lattice: v += a·dt, x += v·dt per tick.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::Particle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{active_elements, standalone_pipeline};

use crate::force_lattice::{FORCE_LATTICE, ForceLatticeBinding};

/// Generated uniform layout: the params in order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MoveSwarmUniforms {
    pub(crate) active_count: i32,
    pub(crate) ticks: i32,
    pub(crate) first_tick: i32,
    pub(crate) force_lattices: i32,
    pub(crate) field_nodes_x: i32,
    pub(crate) field_nodes_y: i32,
    pub(crate) field_nodes_z: i32,
    pub(crate) field_spacing: f32,
    pub(crate) field_min_x: f32,
    pub(crate) field_min_y: f32,
    pub(crate) field_min_z: f32,
    pub(crate) dispatch_count: u32,
    pub(crate) _pad: [u32; 4],
}

const fn int(name: &'static str, label: &'static str, default: f32, max: f32) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Int, default: ParamValue::Float(default), range: Some((0.0, max)), enum_values: &[] }
}

const fn float(name: &'static str, label: &'static str, default: f32, range: (f32, f32)) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Float, default: ParamValue::Float(default), range: Some(range), enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: MoveSwarm,
    type_id: "node.move_swarm",
    purpose: "Advance each live particle through this frame's fixed ticks under the scene's force lattice: per tick, a = the trilinear read of that tick's lattice at the particle, then v += a·dt and x += v·dt (semi-implicit Euler, dt = that tick's entry in tick_seconds, so a Speed ramp lands the same at any frame rate). Each tick reads its own lattice (one per tick from first_tick, or one for every tick when force_lattices is 1; 0 or forces unwired: no force), so the swarm moves the same at any frame rate. Dead particles (life <= 0) pass through unchanged.",
    inputs: {
        in: Array(Particle) required,
        forces: Array(f32) optional,
        tick_seconds: Array(f32) optional,
        active_count: ScalarF32 optional,
        ticks: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional,
        field_nodes_y: ScalarF32 optional,
        field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        field_min_x: ScalarF32 optional,
        field_min_y: ScalarF32 optional,
        field_min_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        int("active_count", "Active Count", 100_000.0, 16_000_000.0),
        int("ticks", "Ticks", 0.0, 4096.0),
        int("first_tick", "First Tick", 0.0, 16_777_216.0),
        int("force_lattices", "Force Lattices", 0.0, 4096.0),
        int("field_nodes_x", "Field Nodes X", 2.0, 4096.0),
        int("field_nodes_y", "Field Nodes Y", 2.0, 4096.0),
        int("field_nodes_z", "Field Nodes Z", 2.0, 4096.0),
        float("field_spacing", "Field Spacing", 0.25, (1.0e-4, 400.0)),
        float("field_min_x", "Field Min X", 0.0, (-1.0e4, 1.0e4)),
        float("field_min_y", "Field Min Y", 0.0, (-1.0e4, 1.0e4)),
        float("field_min_z", "Field Min Z", 0.0, (-1.0e4, 1.0e4)),
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm integrator. Wire forces, tick_seconds and every scalar but active_count from node.swarm_forces (its ticks, first_tick, force_lattices, field_nodes_x/y/z, field_spacing and field_min_x/y/z), particles from a node.array_feedback loop (its out into in, out back into its in, the seeder into its seed, node.swarm_forces epoch into its reset_trigger), then node.swarm_to_copies from out. in/out alias one buffer, so the graph compiler keeps the loop in place. Scene forces reach it through node.swarm_forces's acceleration_field, the same force list water and rigid bodies read.",
    examples: ["Swarm"],
    picker: { label: "Move Swarm", category: Atom },
    summary: "Moves every swarm particle by the scene's forces, one fixed step per simulation tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["swarm", "move swarm", "particle forces", "force particles"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/move_swarm_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    wgsl_includes: [FORCE_LATTICE],
}

impl Primitive for MoveSwarm {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|&(_, n)| n)).flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    /// The moved particles come from the particles and the forces that move
    /// them: the scene force trace reaches node.swarm_forces through here.
    fn physical_sources(&self, output: &str) -> Option<&'static [&'static str]> {
        (output == "out").then_some(&["in", "forces"])
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // The particles are moved in place: a frame without ticks (the first
        // frame, a paused one, the in-between frame at a display rate above
        // Sim Rate) or a refused one leaves last frame's state, which is this
        // frame's. Either way the aliased buffer holds the right particles.
        ctx.mark_gpu_accessed();
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as i32;
        let requested = int(ctx, "active_count", 100_000.0) as u32;
        let ticks = int(ctx, "ticks", 0.0);
        let Some(particles) = ctx.inputs.array("in") else { return };
        let field = match ForceLatticeBinding::read(ctx, ctx.inputs.array("forces"), None, "Move Swarm") {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        // One lattice serves every tick; otherwise each tick needs its own.
        if field.force_lattices > 1 && field.force_lattices < ticks {
            ctx.error(format!("Move Swarm: {ticks} ticks but {} force lattices; wire force_lattices from node.swarm_forces", field.force_lattices));
            return;
        }
        let active = active_elements::<Particle>(particles.size, requested);
        if active == 0 || ticks == 0 {
            return;
        }
        // Each tick runs for its own length (a Speed ramp gives every tick a
        // different one), so the lengths come with the ticks or not at all.
        let Some(tick_seconds) = ctx.inputs.array("tick_seconds").filter(|lengths| lengths.size >= ticks as u64 * 4) else {
            ctx.error(format!("Move Swarm: {ticks} ticks need {ticks} tick lengths; wire tick_seconds from node.swarm_forces"));
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MoveSwarmUniforms {
            active_count: active as i32,
            ticks,
            first_tick: field.first_tick,
            force_lattices: field.force_lattices,
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            field_min_x: field.min[0],
            field_min_y: field.min[1],
            field_min_z: field.min[2],
            dispatch_count: active,
            _pad: [0; 4],
        };
        // `in`/`out` alias the particle buffer; an unwired lattice is never
        // read (force_lattices 0), so the particles stand in for it.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: field.forces.unwrap_or(particles), offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: tick_seconds, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: particles, offset: 0 },
            ],
            [active.div_ceil(256), 1, 1],
            "node.move_swarm",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_swarm_generates_a_valid_gathering_kernel() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<MoveSwarm>().expect("move_swarm codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("buf_forces: array<f32>"), "{wgsl}");
        assert!(wgsl.contains("@binding(3) var<storage, read> buf_tick_seconds: array<f32>"), "{wgsl}");
        assert!(wgsl.contains("field_min_z: f32,\n    dispatch_count: u32,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<MoveSwarmUniforms>(), 64);
    }
}
