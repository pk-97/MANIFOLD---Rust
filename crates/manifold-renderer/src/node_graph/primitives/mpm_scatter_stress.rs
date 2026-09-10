//! `node.mpm_scatter_stress` — S4 solver stage: density + stress momentum.
//!
//! Reads the COMPLETED grid mass (dispatch ordering separates the stages),
//! reconstructs `rho_p = sum(w*m_i)/h^3` per particle, writes it to a
//! separate candidate particle record — never an in-place cross-thread
//! mutation — and adds the stress momentum `-4*dt*V_p/h^2 * w * sigma*d`
//! with `V_p = m_p/rho_p`, `sigma = -p*I + mu*(C + C^T)` to the momentum
//! cells. Mass cells are never modified by this stage. Momentum accumulation
//! is the same checked compare/exchange fixed-point path as the
//! mass/momentum scatter.
//!
//! Codegen gap (reported to the lead, S4): mixed outputs from one
//! invocation — an atomic grid output plus a coincident particle copy —
//! exceed what the generated buffer wrapper expresses (multi-output atoms
//! are supported, but an atomic port among them is rejected outright). This
//! is the documented escape for atomic/global-dependency stages; the math
//! lives in the shared `water_common.wgsl` include either way.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{
    DEFAULT_STEP_DT, SEED_ACTIVE_PARTICLES, WaterParticle,
};

/// Hand-authored standalone kernel (see module doc): `water_common.wgsl`
/// concatenated ahead of the stage source. Composed form is validated at
/// pipeline creation and by the gpu-proofs value tests.
pub const WGSL: &str = concat!(
    include_str!("shaders/water_common.wgsl"),
    "\n",
    include_str!("shaders/mpm_scatter_stress.wgsl"),
);

/// Hand-kernel uniform layout: substep dt, live particle count, pad.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ScatterStressUniforms {
    pub step_dt: f32,
    pub active_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

crate::primitive! {
    name: MpmScatterStress,
    type_id: "node.mpm_scatter_stress",
    purpose: "MLS-MPM density/stress stage (design step 4): after mpm_scatter_mass_momentum has completed the grid mass, reconstruct each particle's density rho_p = sum(w*m_i)/h^3 from the accumulation wire, write it to a separate candidate particle record (density flows stress -> gather -> candidate; no in-place cross-thread mutation), and add the stress momentum -4*dt*V_p/h^2 * w * sigma*d to the grid momentum cells, V_p = m_p/rho_p, sigma = -p*I + mu*(C + C^T), p the weakly-compressible EOS with zero tension below rest density (the explicit free-surface approximation). Mass cells are never modified. Stress momentum uses the same checked Q=2^20 compare/exchange accumulation — overflow sticks FAULT_INTEGER_OVERFLOW, nothing wraps. rho_p nonfinite or nonpositive sticks FAULT_INVALID_DENSITY.",
    inputs: {
        particles: Array(WaterParticle) required,
        accumulator: Channels["water_grid_accum": I32] required,
        status: Array(u32) required,
        step_dt: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Channels["water_grid_accum": I32],
        particles_out: Array(WaterParticle),
        status_out: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("step_dt"),
            label: "Substep dt",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_STEP_DT),
            range: Some((1.0e-5, 1.0e-2)),
            enum_values: &[],
        },
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
    composition_notes: "Third stage of the repeated water region body, between node.mpm_scatter_mass_momentum and node.mpm_grid_velocity. Must run AFTER the mass scatter completes (sequential graph ordering) because density is reconstructed from the completed grid mass. `step_dt` is port-shadow so the region clock drives the fixed substep.",
    examples: [],
    picker: { label: "MPM Scatter (stress)", category: Atom },
    summary: "Recomputes each water particle's density from the grid and pushes the pressure-and-viscosity stress back onto the grid.",
    category: Particles3D,
    role: Filter,
    aliases: ["mpm stress", "water stress", "p2g stress", "density stress"],
    boundary_reason: Blocked,
}

impl Primitive for MpmScatterStress {
    /// Aliased outputs inherit their input wire's capacity (see
    /// `node.move_particles`); `particles_out` inherits the particle wire.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let from = match port_name {
            "out" => "accumulator",
            "status_out" => "status",
            "particles_out" => "particles",
            _ => return None,
        };
        input_capacities
            .iter()
            .find(|(p, _)| *p == from)
            .map(|(_, n)| *n)
    }

    /// Atomic outputs alias their input wires; `particles_out` is a fresh
    /// candidate copy (density-carrying records for the gather stage).
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accumulator", "out"), ("status", "status_out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let Some(particles) = ctx.inputs.array("particles") else {
            // Aliased outputs share their input wires' buffers, so a
            // no-dispatch path leaves the wire coherent; mark GPU access for
            // the executor's stale-data debug_assert.
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(accum) = ctx.outputs.array("out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(particles_out) = ctx.outputs.array("particles_out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(status_out) = ctx.outputs.array("status_out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let capacity = (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        let active_count = ctx
            .scalar_or_param("active_count", SEED_ACTIVE_PARTICLES as f32)
            .round()
            .max(0.0) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Hand-authored standalone kernel: the generated buffer wrapper
            // rejects this stage's mixed atomic+coincident output shape (see
            // module doc).
            gpu.device.create_compute_pipeline(WGSL, "cs_main", "node.mpm_scatter_stress")
        });

        let uniforms = ScatterStressUniforms {
            step_dt,
            active_count,
            _pad0: 0,
            _pad1: 0,
        };

        // uniform(0), particles(1), accumulator atomic(2), status atomic(3),
        // particles_out(4).
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: particles_out,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.mpm_scatter_stress",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_stress_declares_three_outputs() {
        use crate::node_graph::ports::{ArrayType, ChannelElementType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        assert_eq!(MpmScatterStress::TYPE_ID, "node.mpm_scatter_stress");
        let names: Vec<&str> = MpmScatterStress::OUTPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["out", "particles_out", "status_out"]);
        assert_eq!(MpmScatterStress::OUTPUTS[1].ty, PortType::Array(particle_layout));
        let PortType::Array(at) = &MpmScatterStress::OUTPUTS[0].ty else {
            panic!("out must be an array wire");
        };
        assert_eq!(at.specs[0].ty, ChannelElementType::I32);
    }

    #[test]
    fn scatter_stress_registers_as_palette_atom() {
        let prim = MpmScatterStress::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mpm_scatter_stress");
    }
}
