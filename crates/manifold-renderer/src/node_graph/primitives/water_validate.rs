//! `node.water_validate` — S4 solver stage: global fault OR.
//!
//! Every live candidate slot is checked for finiteness, stencil containment
//! and the proof kinematic bounds (`|v| <= 4 m/s`, Frobenius `|C| <= 64/s`,
//! `0 < rho <= 4*rho0`). Finite velocity and affine excess are warnings
//! recorded in the status sideband; invalid density still ORs into the sticky
//! fault word. Bounds are never clamped (design step 7). The output aliases the input status wire,
//! and the incoming bits are OR'd through so a pre-existing sticky fault
//! survives validation.
//!
//! Codegen gap (reported to the lead, S4): a single-global-word reduction —
//! all threads contributing to one atomic word — is not a per-element body
//! shape the generated buffer wrapper expresses. Documented escape for
//! atomic/global-dependency stages.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{
    AFFINE_BOUND, DENSITY_MAX_MULTIPLE, PARTICLE_CAPACITY, REST_DENSITY, STATUS_BYTES,
    VELOCITY_BOUND, WaterParticle,
};

/// Hand-authored standalone kernel (see module doc): `water_common.wgsl`
/// concatenated ahead of the stage source. Composed form is validated at
/// pipeline creation and by the gpu-proofs value tests.
pub const WGSL: &str = concat!(
    include_str!("shaders/water_common.wgsl"),
    "\n",
    include_str!("shaders/water_validate.wgsl"),
);

/// Hand-kernel uniform layout: the validated slot count and the proof
/// kinematic bounds (f32). 4 words, no padding.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ValidateUniforms {
    pub validate_count: u32,
    pub velocity_bound: f32,
    pub affine_bound: f32,
    pub density_max: f32,
    /// 1 when the status output has the fixed diagnostic sideband; 0 keeps
    /// direct/legacy one-word status buffers strictly in-bounds.
    pub diagnostics_enabled: u32,
}

crate::primitive! {
    name: WaterValidate,
    type_id: "node.water_validate",
    purpose: "Validate the Live Water candidate state (design step 7): every live slot (mass != 0) is checked for finiteness, full 27-node stencil containment inside the guard shell, and the proof kinematic bounds |v| <= 4 m/s, Frobenius |C| <= 64/s, 0 < rho <= 4*rho0. Finite velocity and affine excess are diagnostic-only; the other invalid states OR into the single sticky status word (bit 1 nonfinite, 2 integer overflow, 4 outside domain, 8 reserved unsupported kinematics, 16 invalid density). Bounds are never clamped. The incoming status is OR'd through so a pre-existing sticky fault survives. Inactive slots (mass exactly zero) carry no checks. water_commit copies candidate to accepted only while the status word is clean.",
    inputs: {
        particles: Array(WaterParticle) required,
        status: Array(u32) required,
    },
    outputs: {
        status_out: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("validate_count"),
            label: "Validated Slots",
            ty: ParamType::Int,
            default: ParamValue::Float(PARTICLE_CAPACITY as f32),
            range: Some((1.0, 2_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("velocity_bound"),
            label: "Velocity Bound",
            ty: ParamType::Float,
            default: ParamValue::Float(VELOCITY_BOUND),
            range: Some((0.1, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("affine_bound"),
            label: "Affine Bound",
            ty: ParamType::Float,
            default: ParamValue::Float(AFFINE_BOUND),
            range: Some((1.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("density_max"),
            label: "Max Density",
            ty: ParamType::Float,
            default: ParamValue::Float(DENSITY_MAX_MULTIPLE * REST_DENSITY),
            range: Some((100.0, 10000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Sixth stage of the repeated water region body, wired to mpm_gather_advect's candidate output. Dispatches over the full wire capacity (a corrupt tail slot with nonzero mass must be caught, not skipped). The status wire is the same sticky word the scatter stages fault into — one word for the whole region, aliased end to end.",
    examples: [],
    picker: { label: "Water Validate", category: Atom },
    summary: "Checks every live water particle for invalid numerical state and records finite kinematic excess as a warning.",
    category: Particles3D,
    role: Filter,
    aliases: ["water validate", "validate water", "water fault check"],
    boundary_reason: Blocked,
}

impl Primitive for WaterValidate {
    /// The sticky word inherits the status wire's capacity.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "status_out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "status")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    /// The sticky word is updated in place.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("status", "status_out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("validate_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            // Aliased status_out shares the status input's buffer, so a
            // no-dispatch path leaves the wire coherent; mark GPU access for
            // the executor's stale-data debug_assert.
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(status_in) = ctx.inputs.array("status") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(status_out) = ctx.outputs.array("status_out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let capacity = (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        let validate_count = ctx
            .scalar_or_param("validate_count", PARTICLE_CAPACITY as f32)
            .round()
            .max(0.0) as u32;
        let validate_count = validate_count.min(capacity);
        if validate_count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }
        let read_param = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Hand-authored standalone kernel: a global-word reduction is not
            // a per-element body shape (see module doc).
            gpu.device.create_compute_pipeline(WGSL, "cs_main", "node.water_validate")
        });

        let uniforms = ValidateUniforms {
            validate_count,
            velocity_bound: read_param("velocity_bound", VELOCITY_BOUND),
            affine_bound: read_param("affine_bound", AFFINE_BOUND),
            density_max: read_param("density_max", DENSITY_MAX_MULTIPLE * REST_DENSITY),
            diagnostics_enabled: u32::from(status_out.size >= STATUS_BYTES),
        };

        // uniform(0), particles(1), status in(2), status out atomic(3).
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
                    buffer: status_in,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status_out,
                    offset: 0,
                },
            ],
            [validate_count.div_ceil(256), 1, 1],
            "node.water_validate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn validate_declares_particles_status_in_status_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        let u32_layout = ArrayType::of_known::<u32>();
        assert_eq!(WaterValidate::TYPE_ID, "node.water_validate");
        assert_eq!(WaterValidate::INPUTS[0].name, "particles");
        assert_eq!(WaterValidate::INPUTS[0].ty, PortType::Array(particle_layout));
        assert_eq!(WaterValidate::INPUTS[1].name, "status");
        assert_eq!(WaterValidate::OUTPUTS.len(), 1);
        assert_eq!(WaterValidate::OUTPUTS[0].name, "status_out");
        assert_eq!(WaterValidate::OUTPUTS[0].ty, PortType::Array(u32_layout));
    }

    #[test]
    fn validate_aliases_status_wire() {
        let prim = WaterValidate::new();
        assert_eq!(
            crate::node_graph::primitive::Primitive::aliased_array_io(&prim),
            &[("status", "status_out")]
        );
    }

    #[test]
    fn validate_registers_as_palette_atom() {
        let prim = WaterValidate::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_validate");
    }
}
