//! `node.integrate_particles` — advect particles by sampling a 2D
//! velocity field.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. Reads particle positions from
//! an Array input, samples a velocity texture at each particle's
//! UV, integrates one Euler step, and writes back. Particles with
//! `life <= 0` pass through unchanged.
//!
//! The Array port is read/write — Integrate operates on the
//! producer's buffer directly via atomic in-place updates. The
//! `out` port shares the producer's slot so downstream primitives
//! (Scatter, Resolve) see the integrated positions on the same
//! frame.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct IntegrateUniforms {
    active_count: u32,
    speed: f32,
    dt: f32,
    _pad: u32,
}

crate::primitive! {
    name: IntegrateParticles,
    type_id: "node.integrate_particles",
    purpose: "Advect each live particle one Euler step by sampling the wired 2D velocity field at the particle's current UV. `speed` scales the field magnitude; `dt` scales the integration step. Toroidal wrap keeps particles in [0,1]² regardless of step size. Dead particles (life <= 0) pass through unchanged.",
    inputs: {
        in: Array(Particle) required,
        velocity: Texture2D required,
        speed: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "speed",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "dt",
            label: "Timestep",
            ty: ParamType::Float,
            default: ParamValue::Float(0.016_666_67),
            range: Some((0.001, 0.1)),
            enum_values: &[],
        },
    ],
    composition_notes: "The velocity texture is sampled with bilinear filtering at the particle's UV — bilinear is what makes per-pixel velocity fields produce smooth advection. `speed` accepts a control wire for audio/LFO-driven advection energy.",
    examples: [],
    picker: { label: "Integrate Particles", category: Atom },
}

impl Primitive for IntegrateParticles {
    /// Output `out` is sized to match the input `in` — the in-place
    /// integration writes through the producer's buffer; the chain
    /// build aliases `in` and `out` to the same slot.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 100_000,
        };
        let speed = match ctx.inputs.scalar("speed") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("speed") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        let dt = match ctx.params.get("dt") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.016_666_67,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(velocity) = ctx.inputs.texture_2d("velocity") else {
            return;
        };
        // Output shares the input's slot for in-place mutation. If the
        // chain build pre-bound `in` and `out` to the same buffer
        // (typical), `out_buf` is the same handle as `in_buf`.
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        // If the runtime ever wires `out` to a different buffer than
        // `in`, the in-place semantics break. For V1 the chain build
        // is responsible for aliasing the two slots; the dispatch
        // below writes through `in_buf`.
        let _ = out_buf;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/integrate_particles.wgsl"),
                "cs_main",
                "node.integrate_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = IntegrateUniforms {
            active_count,
            speed,
            dt,
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
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: velocity,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.integrate_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn integrate_particles_declares_array_in_texture_in_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(IntegrateParticles::TYPE_ID, "node.integrate_particles");
        assert_eq!(IntegrateParticles::INPUTS.len(), 3);
        assert_eq!(IntegrateParticles::INPUTS[0].name, "in");
        assert_eq!(
            IntegrateParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(IntegrateParticles::INPUTS[1].name, "velocity");
        assert_eq!(IntegrateParticles::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(IntegrateParticles::INPUTS[2].name, "speed");
        assert!(!IntegrateParticles::INPUTS[2].required);

        assert_eq!(IntegrateParticles::OUTPUTS.len(), 1);
        assert_eq!(
            IntegrateParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn integrate_particles_speed_port_shadows_param() {
        // Standard port-shadow convention: scalar input port with same
        // name as a ParamDef. Both must be declared.
        let speed_port_present = IntegrateParticles::INPUTS
            .iter()
            .any(|p| p.name == "speed");
        let speed_param_present = IntegrateParticles::PARAMS
            .iter()
            .any(|p| p.name == "speed");
        assert!(speed_port_present);
        assert!(speed_param_present);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = IntegrateParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.integrate_particles");
    }
}
