//! `node.mpm_gather_advect` — S4 solver stage: G2P + advection.
//!
//! Gathers the resolved grid velocities over each particle's 27-node
//! stencil (`v_p = sum(w*v_i)`, `C_p = 4/h^2 * sum(w*outer(v_i,d))`),
//! advects `x_next = x + step_dt*v_p`, stores the previous accepted
//! position, and passes the reconstructed density through untouched —
//! density reaches the candidate through the stress copy, never through a
//! parallel copy of pre-stress records (design step 6).
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{DEFAULT_STEP_DT, WaterGridCell, WaterParticle};

/// Generated-codegen uniform layout: the `step_dt` param, then the
/// codegen-injected `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GatherAdvectUniforms {
    pub step_dt: f32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

crate::primitive! {
    name: MpmGatherAdvect,
    type_id: "node.mpm_gather_advect",
    purpose: "MLS-MPM grid-to-particle transfer + advection (design step 6): gather the resolved grid velocities over each live particle's 27-node quadratic B-spline stencil — v_p = sum(w*v_i), C_p = 4/h^2 * sum(w*outer(v_i,d)) — advect x_next = x + step_dt*v_p, and store the previous accepted position. The reconstructed density written by node.mpm_scatter_stress flows through to the candidate untouched. Dispatches over the full wire capacity: inactive slots pass through unchanged so the candidate buffer is fully written every substep. A stencil that leaves the grid cannot fault here (no status output) — the particle passes through without advancing and water_validate flags it; with the static basin inside the guard shell this path is unreachable.",
    inputs: {
        particles: Array(WaterParticle) required,
        grid: Array(WaterGridCell) required,
        step_dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterParticle),
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
    ],
    depth_rule: Terminal,
    composition_notes: "Fifth stage of the repeated water region body, after node.mpm_grid_velocity. Wire `particles` from mpm_scatter_stress's particles_out (the density-carrying candidate copy), NOT from the pre-stress records — losing the density through the wrong wire is the bug class the port order exists to prevent. Particle-level collision projection is node.water_collide_box (S5); this stage closes with node.water_validate + node.water_commit.",
    examples: [],
    picker: { label: "MPM Gather + Advect", category: Atom },
    summary: "Pulls the grid velocities back onto each water particle and moves it one substep, carrying its density forward.",
    category: Particles3D,
    role: Filter,
    aliases: ["mpm gather", "g2p", "grid to particle", "advect water"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mpm_gather_advect_body.wgsl"),
    input_access: [Coincident, BufferGather],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for MpmGatherAdvect {
    /// Output capacity inherits the particle wire.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "particles")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let step_dt = ctx.scalar_or_param("step_dt", DEFAULT_STEP_DT);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(grid) = ctx.inputs.array("grid") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let particle_size = std::mem::size_of::<WaterParticle>() as u64;
        // Full capacity: every slot is written (live slots advected, inactive
        // slots passed through) so downstream buffers never carry stale data.
        // The grid wire does not constrain the particle count — each
        // particle reads 27 cells; only the particle buffers size the dispatch.
        let capacity = (particles.size.min(out_buf.size) / particle_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body`; the BufferGather grid input
            // keeps the atom a fusion boundary in practice.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.mpm_gather_advect standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mpm_gather_advect",
            )
        });

        let uniforms = GatherAdvectUniforms {
            step_dt,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
        };

        // uniform(0), particles(1), grid(2), particles out(3).
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
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.mpm_gather_advect",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn gather_advect_declares_particles_grid_in_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        let grid_layout = ArrayType::of_known::<WaterGridCell>();
        assert_eq!(MpmGatherAdvect::TYPE_ID, "node.mpm_gather_advect");
        assert_eq!(MpmGatherAdvect::INPUTS[0].name, "particles");
        assert_eq!(MpmGatherAdvect::INPUTS[0].ty, PortType::Array(particle_layout));
        assert_eq!(MpmGatherAdvect::INPUTS[1].name, "grid");
        assert_eq!(MpmGatherAdvect::INPUTS[1].ty, PortType::Array(grid_layout));
        assert_eq!(MpmGatherAdvect::OUTPUTS.len(), 1);
        assert_eq!(MpmGatherAdvect::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn gather_advect_registers_as_palette_atom() {
        let prim = MpmGatherAdvect::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mpm_gather_advect");
    }

    #[test]
    fn gather_advect_codegen_binds_grid_gather() {
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<MpmGatherAdvect>()
                .expect("node.mpm_gather_advect standalone codegen");
        assert!(wgsl.contains("var<storage, read> buf_grid: array<vec4<f32>>"));
        assert!(wgsl.contains("struct Element"));
    }
}
