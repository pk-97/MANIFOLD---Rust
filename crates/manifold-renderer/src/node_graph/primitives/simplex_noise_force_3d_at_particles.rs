//! `node.simplex_noise_force_3d_at_particles` — per-particle 3D simplex
//! noise advection added in place to an `Array<[f32; 3]>` force buffer.
//!
//! The 3D sibling of `node.simplex_noise_force_at_particles`. 3D noise
//! is built from `SimplexNoise2D` evaluated on three orthogonal planes
//! (yz / xz / xy), with density-adaptive amplitude
//! (`turbulence * (1 + capped(density) * anti_clump)`). Bit-exact with
//! the noise-advection step of the legacy fused `node.fluid_simulate_3d`.
//!
//! Aliased `in`/`out` `Array<[f32; 3]>`: a single physical force buffer,
//! mutated in place (the noise is *added* to whatever upstream force
//! atoms already deposited). Pair downstream of
//! `node.sample_texture_3d_at_particles`.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseUniforms {
    active_count: u32,
    turbulence: f32,
    anti_clump: f32,
    time2: f32,
}

crate::primitive! {
    name: SimplexNoiseForce3DAtParticles,
    type_id: "node.simplex_noise_force_3d_at_particles",
    purpose: "Per-particle 3D simplex noise advection added in-place to an Array<[f32; 3]> force buffer. 3D noise from SimplexNoise2D on three orthogonal planes (yz/xz/xy), density-adaptive amplitude (turbulence * (1 + capped(density) * anti_clump), capped = d/(1+d)). Samples a density Texture3D at the particle's position. Aliased force in/out — one physical buffer, in-place add. The 3D sibling of node.simplex_noise_force_at_particles, decomposed from the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        density: Texture3D required,
        turbulence: ScalarF32 optional,
        anti_clump: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: "turbulence",
            label: "Turbulence",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "anti_clump",
            label: "Anti-Clump",
            ty: ParamType::Float,
            default: ParamValue::Float(20.0),
            range: Some((0.0, 60.0)),
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
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place add). `turbulence` and `anti_clump` are port-shadow so an LFO / clip-trigger envelope / outer-card slider drives the noise energy and the density-adaptive boost live. The density Texture3D modulates amplitude: where particles have accumulated (high density), the noise amplitude rises by `1 + capped(d) * anti_clump`, which spreads clumps apart. Time animates the noise field through `time2 * 0.1`. Wire downstream of node.sample_texture_3d_at_particles, upstream of node.euler_step_particles_3d.",
    examples: ["FluidSimulation3D"],
    picker: { label: "Turbulence (3D, simplex)", category: Atom },
    summary: "Pushes 3D particles around with a flowing 3D noise field for organic, swirling motion through space.",
    category: Particles3D,
    role: Filter,
    aliases: ["turbulence 3d", "noise force 3d", "flow", "simplex"],
}

impl Primitive for SimplexNoiseForce3DAtParticles {
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
        let turbulence = ctx.scalar_or_param("turbulence", 0.001);
        let anti_clump = ctx.scalar_or_param("anti_clump", 20.0);
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;

        let Some(in_forces) = ctx.inputs.array("in") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(density) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 12) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }

        let time2 = ctx.time.seconds.0 as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/simplex_noise_force_3d_at_particles.wgsl"),
                "cs_main",
                "node.simplex_noise_force_3d_at_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = NoiseUniforms {
            active_count,
            turbulence,
            anti_clump,
            time2,
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: in_forces,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: density,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.simplex_noise_force_3d_at_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec3_in_out_required_particles_and_density() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            SimplexNoiseForce3DAtParticles::TYPE_ID,
            "node.simplex_noise_force_3d_at_particles"
        );
        let names: Vec<&str> = SimplexNoiseForce3DAtParticles::INPUTS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec!["in", "particles", "density", "turbulence", "anti_clump", "active_count"]
        );
        assert_eq!(
            SimplexNoiseForce3DAtParticles::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(SimplexNoiseForce3DAtParticles::INPUTS[0].required);
        assert_eq!(
            SimplexNoiseForce3DAtParticles::INPUTS[1].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(SimplexNoiseForce3DAtParticles::INPUTS[2].ty, PortType::Texture3D);
        assert!(SimplexNoiseForce3DAtParticles::INPUTS[2].required);

        assert_eq!(SimplexNoiseForce3DAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            SimplexNoiseForce3DAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = SimplexNoiseForce3DAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn turbulence_and_anti_clump_port_shadow_params() {
        for name in ["turbulence", "anti_clump"] {
            let has_port = SimplexNoiseForce3DAtParticles::INPUTS
                .iter()
                .any(|p| p.name == name);
            let has_param = SimplexNoiseForce3DAtParticles::PARAMS
                .iter()
                .any(|p| p.name == name);
            assert!(has_port, "input port '{name}' missing");
            assert!(has_param, "param '{name}' missing");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexNoiseForce3DAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.simplex_noise_force_3d_at_particles"
        );
    }
}
