//! `node.anti_clump_particles` — density-weighted Brownian kick on
//! each live particle's `position.xy`.
//!
//! For each particle: sample `density` at `position.xy`, compute
//! `capped_density = d / (1 + d)`, and add
//! `(hash3(i, frame) − 0.5) * strength * capped_density` to
//! `position.xy`. The density weighting concentrates the kick where
//! particles are clumped — the density texture is bright in pixels
//! where many particles overlap — so the noise preferentially shoves
//! clumps apart instead of being a uniform jitter everywhere.
//!
//! Sibling to [`super::array_diffuse_particles`] which kicks
//! `velocity` (ODE-state diffusion for attractor sims). Two distinct
//! atoms rather than one with a mode enum because the math, the
//! state field, and the density-weighting are different — splitting
//! avoids the dead-state-param anti-pattern.
//!
//! Reusable for any density-displacement particle pipeline: fluid
//! sims, sparks, particle-text, crowd / flock simulations.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AntiClumpUniforms {
    active_count: u32,
    frame_count: u32,
    strength: f32,
    _pad: u32,
}

crate::primitive! {
    name: AntiClumpParticles,
    type_id: "node.anti_clump_particles",
    purpose: "Density-weighted Brownian kick on each live particle's position.xy. Samples a density texture at the particle's UV, applies `kick = (hash3(i, frame) − 0.5) * strength * capped_density` where `capped_density = d / (1 + d)`. Concentrates the noise where particles are clumped, gently shoving accumulated clusters apart — the textbook 'anti-clumping' force. Sibling to node.array_diffuse_particles (which kicks velocity, un-weighted) — these are two atoms because the math and weighting differ, not one with a mode enum.",
    inputs: {
        in: Array(Particle) required,
        density: Texture2D required,
        strength: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "strength",
            label: "Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.1)),
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
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `strength` is port-shadow so an LFO / audio band / outer-card slider can modulate the anti-clump energy live. The density texture is typically the same one driving the gradient/rotate force-field path; sampling it here ensures the kick activates exactly where particles have accumulated. Frame seed (frame_count) reseeds the hash each frame so adjacent frames produce decorrelated kicks rather than a slow drift.",
    examples: [],
    picker: { label: "Anti-Clump Particles", category: Atom },
}

impl Primitive for AntiClumpParticles {
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
        let strength = ctx.scalar_or_param("strength", 0.0);
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let Some(density) = ctx.inputs.texture_2d("density") else {
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

        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/anti_clump_particles.wgsl"),
                "cs_main",
                "node.anti_clump_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = AntiClumpUniforms {
            active_count,
            frame_count,
            strength,
            _pad: 0,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: density,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.anti_clump_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out_and_required_density_texture() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(AntiClumpParticles::TYPE_ID, "node.anti_clump_particles");
        let names: Vec<&str> = AntiClumpParticles::INPUTS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["in", "density", "strength", "active_count"]);
        assert_eq!(
            AntiClumpParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(AntiClumpParticles::INPUTS[0].required);
        assert_eq!(AntiClumpParticles::INPUTS[1].ty, PortType::Texture2D);
        assert!(AntiClumpParticles::INPUTS[1].required);

        assert_eq!(AntiClumpParticles::OUTPUTS.len(), 1);
        assert_eq!(
            AntiClumpParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = AntiClumpParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn strength_port_shadows_param() {
        let has_port = AntiClumpParticles::INPUTS.iter().any(|p| p.name == "strength");
        let has_param = AntiClumpParticles::PARAMS.iter().any(|p| p.name == "strength");
        assert!(has_port);
        assert!(has_param);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = AntiClumpParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.anti_clump_particles");
    }
}
