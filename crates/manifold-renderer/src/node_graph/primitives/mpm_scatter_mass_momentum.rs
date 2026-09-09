//! `node.mpm_scatter_mass_momentum` — S4 solver stage: P2G mass/momentum.
//!
//! Accumulates `w*m` and `w*m*(v + C*d)` on the 27 stencil nodes of every
//! live particle as signed fixed-point Q = 2^20 via checked
//! compare/exchange atomics — a wrapped atomicAdd is never accepted as data
//! (design D6): overflow, quantisation failure and bounded-retry exhaustion
//! all stick FAULT_INTEGER_OVERFLOW and retain the last representable cell
//! value.
//!
//! Codegen gap (reported to the lead, S4): the generated buffer wrapper
//! cannot express this stage — it carries TWO atomic outputs (the i32
//! accumulator AND the u32 status word), and `generate_standalone_buffer`
//! rejects any multi-output atom containing an atomic port. This is the
//! documented escape for atomic/global-dependency stages: a hand-authored
//! standalone kernel whose math lives in the shared `water_common.wgsl`
//! include (single source with the fusable stages).
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{SEED_ACTIVE_PARTICLES, WaterParticle};

/// Hand-authored standalone kernel (see module doc): `water_common.wgsl`
/// concatenated ahead of the stage source. Composed form is validated at
/// pipeline creation and by the gpu-proofs value tests.
pub const WGSL: &str = concat!(
    include_str!("shaders/water_common.wgsl"),
    "\n",
    include_str!("shaders/mpm_scatter_mass_momentum.wgsl"),
);

/// Hand-kernel uniform layout: the live particle count; dispatch guard.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ScatterMassUniforms {
    pub active_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: MpmScatterMassMomentum,
    type_id: "node.mpm_scatter_mass_momentum",
    purpose: "MLS-MPM particle-to-grid transfer (design step 3): scatter each live WaterParticle's mass w*m and momentum w*m*(v + C*d) onto the 27 quadratic B-spline stencil nodes of the fixed-point accumulation grid (Q = 2^20, signed i32, 4 slots per cell: momentum xyz + mass). Checked compare/exchange accumulation — overflow or bounded-retry exhaustion sticks FAULT_INTEGER_OVERFLOW in the status word and keeps the last representable value; nothing ever wraps. Inactive slots (mass zero) contribute nothing. The atomic output aliases the accumulator wire; status_out aliases the status wire.",
    inputs: {
        particles: Array(WaterParticle) required,
        accumulator: Channels["water_grid_accum": I32] required,
        status: Array(u32) required,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Channels["water_grid_accum": I32],
        status_out: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Particles",
            ty: ParamType::Int,
            default: ParamValue::Float(SEED_ACTIVE_PARTICLES as f32),
            range: Some((0.0, 2_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Second stage of the repeated water region body, after node.clear_grid. `active_count` is port-shadow so the region clock / emission stage can grow the scattered set. Pair with node.mpm_scatter_stress (reads this stage's completed mass), then node.mpm_grid_velocity. Dispatch ordering — not workgroup barriers — separates the stages.",
    examples: [],
    picker: { label: "MPM Scatter (mass+momentum)", category: Atom },
    summary: "Splats each water particle's mass and momentum onto the grid of cells around it, in exact fixed-point arithmetic.",
    category: Particles3D,
    role: Filter,
    aliases: ["mpm scatter", "p2g", "particle to grid", "scatter mass momentum"],
    boundary_reason: Blocked,
}

impl Primitive for MpmScatterMassMomentum {
    /// Aliased outputs inherit their input wire's capacity (the
    /// `node.move_particles` precedent): the direct executor path binds a
    /// real buffer from this, and the chain builder aliases the output slot
    /// onto the input's instead.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let from = match port_name {
            "out" => "accumulator",
            "status_out" => "status",
            _ => return None,
        };
        input_capacities
            .iter()
            .find(|(p, _)| *p == from)
            .map(|(_, n)| *n)
    }

    /// Both atomic outputs alias their input wires: the stage accumulates
    /// into the existing accumulator/status buffers in place.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accumulator", "out"), ("status", "status_out")]
    }

    // run() dispatches `active_count` threads, not pool capacity.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("out") else {
            return;
        };
        let Some(status_out) = ctx.outputs.array("status_out") else {
            return;
        };
        let capacity = (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        let active_count = ctx
            .scalar_or_param("active_count", SEED_ACTIVE_PARTICLES as f32)
            .round()
            .max(0.0) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Hand-authored standalone kernel: the generated buffer wrapper
            // rejects this stage's two-atomic-output shape (see module doc).
            gpu.device.create_compute_pipeline(WGSL, "cs_main", "node.mpm_scatter_mass_momentum")
        });

        let uniforms = ScatterMassUniforms {
            active_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), particles(1), accumulator atomic(2), status atomic(3).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status_out,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.mpm_scatter_mass_momentum",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_mass_declares_particles_in_accumulator_status_out() {
        use crate::node_graph::ports::{ArrayType, ChannelElementType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        let u32_layout = ArrayType::of_known::<u32>();
        assert_eq!(MpmScatterMassMomentum::TYPE_ID, "node.mpm_scatter_mass_momentum");
        let particles_in = MpmScatterMassMomentum::INPUTS
            .iter()
            .find(|p| p.name == "particles")
            .expect("particles input");
        assert_eq!(particles_in.ty, PortType::Array(particle_layout));
        assert!(particles_in.required);
        let accum_in = MpmScatterMassMomentum::INPUTS
            .iter()
            .find(|p| p.name == "accumulator")
            .expect("accumulator input");
        let PortType::Array(at) = &accum_in.ty else {
            panic!("accumulator must be an array wire");
        };
        assert_eq!(at.specs[0].ty, ChannelElementType::I32);
        assert_eq!(MpmScatterMassMomentum::OUTPUTS.len(), 2);
        assert_eq!(MpmScatterMassMomentum::OUTPUTS[0].name, "out");
        assert_eq!(MpmScatterMassMomentum::OUTPUTS[1].name, "status_out");
        assert_eq!(MpmScatterMassMomentum::OUTPUTS[1].ty, PortType::Array(u32_layout));
    }

    #[test]
    fn scatter_mass_aliases_accumulator_and_status_wires() {
        let prim = MpmScatterMassMomentum::new();
        assert_eq!(
            crate::node_graph::primitive::Primitive::aliased_array_io(&prim),
            &[("accumulator", "out"), ("status", "status_out")]
        );
    }

    #[test]
    fn scatter_mass_registers_as_palette_atom() {
        let prim = MpmScatterMassMomentum::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mpm_scatter_mass_momentum");
    }
}
