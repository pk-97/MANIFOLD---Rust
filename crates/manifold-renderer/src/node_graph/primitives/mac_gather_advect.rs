//! Trilinear staggered MAC APIC gather and third-order particle advection.
//! Invalid samples emit a nonfinite candidate for water_validate to reject.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;

/// Default fixed substep for the incompressible MAC solver.
pub const DEFAULT_STEP_DT: f32 = 1.0 / 120.0;

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacGatherAdvect>()
        .expect("node.mac_gather_advect codegen")
}

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
    name: MacGatherAdvect,
    type_id: "node.mac_gather_advect",
    purpose: "Gather velocity and APIC affine rows from eight trilinear staggered faces per component, then advect particles with RK3 (stages 1/2 and 3/4; weights 2/9, 3/9, 4/9). Preserve mass and density, record the old position, and copy inactive slots exactly. Invalid positions, face validity, velocities, or timesteps produce a NaN candidate position for validation to reject.",
    inputs: {
        particles: Array(WaterParticle) required,
        grid: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] required,
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
    composition_notes: "Consumes the projected, extrapolated MAC field: 64-cubed cells in a padded 65-cubed allocation, 32 bytes per entry. Each component has 65 normal faces and 64 tangential faces at half-cell offsets. APIC rows are sum(gradient(weight) * face_velocity), without MPM moment scaling. Collision, validation, and commit remain separate downstream atoms.",
    examples: [],
    picker: { label: "MAC Gather + Advect", category: Atom },
    summary: "Pulls the grid velocities back onto each water particle and moves it one substep, carrying its density forward.",
    category: Particles3D,
    role: Filter,
    aliases: ["mac gather", "g2p", "grid to particle", "advect water"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mac_gather_advect_body.wgsl"),
    input_access: [Coincident, BufferGather],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
    extra_fields: { source: String = shader_source(), },
}

impl Primitive for MacGatherAdvect {
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
        if grid.size < 65 * 65 * 65 * 32 {
            ctx.error("node.mac_gather_advect requires the full padded 65-cubed MAC grid");
            return;
        }
        let particle_size = std::mem::size_of::<WaterParticle>() as u64;
        // Full capacity: every slot is written (live slots advected, inactive
        // slots passed through) so downstream buffers never carry stale data.
        // The grid wire does not constrain the particle count — each
        // particle gathers staggered faces; only the particle buffers size the dispatch.
        let capacity = (particles.size.min(out_buf.size) / particle_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mac_gather_advect",
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
            "node.mac_gather_advect",
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
        assert_eq!(MacGatherAdvect::TYPE_ID, "node.mac_gather_advect");
        assert_eq!(MacGatherAdvect::INPUTS[0].name, "particles");
        assert_eq!(
            MacGatherAdvect::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(MacGatherAdvect::INPUTS[1].name, "grid");
        let PortType::Array(grid_layout) = &MacGatherAdvect::INPUTS[1].ty else {
            panic!("grid must be channels");
        };
        assert_eq!(grid_layout.item_size, 32);
        assert_eq!(grid_layout.specs.len(), 2);
        assert_eq!(MacGatherAdvect::OUTPUTS.len(), 1);
        assert_eq!(
            MacGatherAdvect::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn gather_advect_registers_as_palette_atom() {
        let prim = MacGatherAdvect::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mac_gather_advect");
    }

    #[test]
    fn mac_gather_generated_shader_validates() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("valid MAC gather shader");
    }

    #[test]
    fn gather_advect_codegen_binds_grid_gather() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MacGatherAdvect>()
            .expect("node.mac_gather_advect standalone codegen");
        assert!(wgsl.contains("var<storage, read> buf_grid: array<"));
        assert!(wgsl.contains("mac_valid: vec4<f32>"));
        assert!(wgsl.contains("struct Element"));
    }
}
