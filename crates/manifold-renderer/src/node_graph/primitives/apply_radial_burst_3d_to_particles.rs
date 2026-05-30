//! `node.apply_radial_burst_3d_to_particles` — per-particle 3D injection
//! burst around one of four hardcoded tetrahedron-vertex zones.
//!
//! The 3D sibling of `node.apply_radial_burst_to_particles`. Where the
//! 2D atom pushes around a free `(point_x, point_y)`, the 3D burst
//! selects one of four fixed tetrahedron-vertex zones via `inject_index`
//! (-1 = off) and applies a noise-perturbed radial push plus a
//! vortex-ring tangent directly to `position.xyz`. Bit-exact with the
//! injection step of the legacy fused `node.fluid_simulate_3d`.
//!
//! `dt = delta * 60` is baked in (frame-rate-normalised like
//! `node.euler_step_particles_3d`); time drives the noise-perturbation
//! phase. Place last in the per-particle position chain.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Burst3DUniforms {
    active_count: u32,
    inject_index: i32,
    inject_force: f32,
    inject_phase: f32,
    time2: f32,
    dt_scaled: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: ApplyRadialBurst3DToParticles,
    type_id: "node.apply_radial_burst_3d_to_particles",
    purpose: "Per-particle 3D injection burst around one of four hardcoded tetrahedron-vertex zones. inject_index < 0 disables it; 0..3 selects a zone. Applies a noise-perturbed radial push + vortex-ring tangent (within radius 0.25, quartic falloff, attack/decay envelope from inject_phase) directly to position.xyz. The 3D sibling of node.apply_radial_burst_to_particles; decomposed from the injection step of the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array(Particle) required,
        inject_index: ScalarF32 optional,
        inject_force: ScalarF32 optional,
        inject_phase: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "inject_index",
            label: "Inject Zone",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_force",
            label: "Inject Force",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_phase",
            label: "Inject Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `inject_index = -1` disables the burst (the kernel early-outs); the four zones (0..3) are hardcoded tetrahedron-vertex positions. Typical wiring: a clip-trigger cycle picks the zone, gated by node.inject_burst's `active` (mux to -1 when idle); `inject_force` from the gated force slider; `inject_phase` from inject_burst's phase. `dt = delta * 60` baked in; time uses ctx.time.seconds for the noise-perturbation phase. When inject_index < 0 or inject_force * envelope is tiny the kernel early-outs (cheap when idle).",
    examples: ["FluidSimulation3D"],
    picker: { label: "Apply Radial Burst 3D (Particles)", category: Atom },
}

impl Primitive for ApplyRadialBurst3DToParticles {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let inject_index = ctx.scalar_or_param("inject_index", -1.0).round() as i32;
        let inject_force = ctx.scalar_or_param("inject_force", 0.0);
        let inject_phase = ctx.scalar_or_param("inject_phase", 0.0);
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let time2 = ctx.time.seconds.0 as f32;
        let dt_scaled = ctx.time.delta.0 as f32 * 60.0;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/apply_radial_burst_3d_to_particles.wgsl"),
                "cs_main",
                "node.apply_radial_burst_3d_to_particles",
            )
        });

        let uniforms = Burst3DUniforms {
            active_count,
            inject_index,
            inject_force,
            inject_phase,
            time2,
            dt_scaled,
            _pad0: 0,
            _pad1: 0,
        };

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
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.apply_radial_burst_3d_to_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out_and_port_shadow_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(
            ApplyRadialBurst3DToParticles::TYPE_ID,
            "node.apply_radial_burst_3d_to_particles"
        );
        let names: Vec<&str> = ApplyRadialBurst3DToParticles::INPUTS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec!["in", "inject_index", "inject_force", "inject_phase", "active_count"]
        );
        assert_eq!(
            ApplyRadialBurst3DToParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ApplyRadialBurst3DToParticles::INPUTS[0].required);
        for input in &ApplyRadialBurst3DToParticles::INPUTS[1..] {
            assert!(!input.required, "{} should be optional", input.name);
        }

        assert_eq!(ApplyRadialBurst3DToParticles::OUTPUTS.len(), 1);
        assert_eq!(
            ApplyRadialBurst3DToParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = ApplyRadialBurst3DToParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<Burst3DUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ApplyRadialBurst3DToParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.apply_radial_burst_3d_to_particles"
        );
    }
}
