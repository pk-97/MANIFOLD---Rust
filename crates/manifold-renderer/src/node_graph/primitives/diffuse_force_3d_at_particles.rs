//! `node.diffuse_force_3d_at_particles` — per-particle incoherent 3D
//! random kick added in place to an `Array<[f32; 3]>` force buffer,
//! weighted by local density.
//!
//! Bit-exact with the per-particle diffusion step of the legacy fused
//! `node.fluid_simulate_3d`:
//!
//! ```text
//! capped     = density.r / (1 + density.r) at p.position
//! diff_seed  = i * 1664525 + frame_count * 747796405
//! forces[i] += (hash_float3(diff_seed) - 0.5) * diffusion * capped
//! ```
//!
//! Incoherent (per-particle hash, reseeded each frame) where
//! `node.simplex_noise_force_3d_at_particles` is spatially coherent. The
//! density weighting concentrates the kick where particles have clumped,
//! so it doubles as an anti-clumping diffusion. Sibling on the velocity
//! field would be `node.array_diffuse_particles` (attractor sims); this
//! one adds to the force buffer so the kick is integrated through
//! `speed * dt` by `node.euler_step_particles_3d`.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiffuseUniforms {
    active_count: u32,
    frame_count: u32,
    diffusion: f32,
    _pad0: u32,
}

crate::primitive! {
    name: DiffuseForce3DAtParticles,
    type_id: "node.diffuse_force_3d_at_particles",
    purpose: "Per-particle incoherent 3D random kick added in-place to an Array<[f32; 3]> force buffer, weighted by local density. forces[i] += (hash_float3(i, frame) - 0.5) * diffusion * capped(density). Reseeds the hash each frame (Brownian, not drift); the density weighting concentrates the kick where particles clump (anti-clumping diffusion). Decomposed from the diffusion step of the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        density: Texture3D required,
        diffusion: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: "diffusion",
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0166),
            range: Some((0.0, 0.5)),
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
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place add). `diffusion` is port-shadow so a control wire drives the kick energy live. The density Texture3D weights the kick by `capped(d) = d/(1+d)` — particles in dense regions get a stronger random push, which spreads clumps. Early-outs when diffusion <= 0. Wire between node.simplex_noise_force_3d_at_particles and node.euler_step_particles_3d so the kick is integrated through speed*dt.",
    examples: ["FluidSimulation3D"],
    picker: { label: "Spread Out (3D diffuse)", category: Atom },
    summary: "Gives each 3D particle a small random kick so a tight clump slowly spreads apart in space.",
    category: Particles3D,
    role: Filter,
    aliases: ["spread out 3d", "diffuse 3d", "jitter"],
}

impl Primitive for DiffuseForce3DAtParticles {
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
        let diffusion = ctx.scalar_or_param("diffusion", 0.0166);
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

        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/diffuse_force_3d_at_particles.wgsl"),
                "cs_main",
                "node.diffuse_force_3d_at_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DiffuseUniforms {
            active_count,
            frame_count,
            diffusion,
            _pad0: 0,
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
            "node.diffuse_force_3d_at_particles",
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
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            DiffuseForce3DAtParticles::TYPE_ID,
            "node.diffuse_force_3d_at_particles"
        );
        let names: Vec<&str> = DiffuseForce3DAtParticles::INPUTS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec!["in", "particles", "density", "diffusion", "active_count"]
        );
        assert_eq!(
            DiffuseForce3DAtParticles::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(DiffuseForce3DAtParticles::INPUTS[0].required);
        assert_eq!(DiffuseForce3DAtParticles::INPUTS[2].ty, PortType::Texture3D);
        assert!(DiffuseForce3DAtParticles::INPUTS[2].required);

        assert_eq!(DiffuseForce3DAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            DiffuseForce3DAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = DiffuseForce3DAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn diffusion_port_shadows_param() {
        let has_port = DiffuseForce3DAtParticles::INPUTS
            .iter()
            .any(|p| p.name == "diffusion");
        let has_param = DiffuseForce3DAtParticles::PARAMS
            .iter()
            .any(|p| p.name == "diffusion");
        assert!(has_port);
        assert!(has_param);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DiffuseForce3DAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.diffuse_force_3d_at_particles"
        );
    }
}
