//! `node.simplex_noise_force_at_particles` — per-particle 2D simplex
//! noise force added in-place to an Array<vec2<f32>> force buffer.
//!
//! For each live particle i: evaluate 2D simplex noise at the
//! particle's position (X and Y channels offset by 100 in noise UV
//! space for decorrelation), scale by `amplitude`, optionally modulate
//! by a scalar texture sampled at the same UV (capped `m/(1+m)` × gain
//! boost — matches the legacy FluidSim density-adaptive noise formula),
//! and add to `forces[i]`. Aliased `in`/`out` Array<vec2>: a single
//! physical buffer, mutated in place.
//!
//! Why per-particle and not per-pixel? Same math, different work
//! domain. A texture-domain noise field evaluates simplex_noise_2d
//! at canvas-pixel UVs (~8.3M positions at 4K), then particles bilinear-
//! sample it. The atom evaluates at particle positions directly
//! (~2M at FluidSim defaults), then adds to the per-particle force
//! buffer. The output is mathematically equivalent; the cost is
//! independent of canvas resolution. Replaces a ~9-node texture
//! noise advection chain (simplex_field_2d × 2, pack_channels × 2,
//! scale_offset × 3, mix × 2) for any per-particle noise consumer.
//!
//! Optional `amplitude_modulator` Texture2D: when wired, samples the
//! modulator's R channel at the particle UV and applies
//! `amplitude * (1 + capped(m) * modulator_gain)`. When unwired,
//! amplitude is uniform. Both modes are bit-identical to the legacy
//! formula for the corresponding wire shape.
//!
//! Reusable for any per-particle force-augmentation pipeline that
//! wants spatially-coherent jitter without paying canvas-area cost.

use manifold_gpu::{
    GpuBinding, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseUniforms {
    active_count: u32,
    amplitude: f32,
    modulator_gain: f32,
    z: f32,
    noise_scale: f32,
    has_modulator: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SimplexNoiseForceAtParticles,
    type_id: "node.simplex_noise_force_at_particles",
    purpose: "Per-particle 2D simplex noise force added in-place to an Array<vec2<f32>> force buffer. Evaluates simplex_noise_2d at each particle's position (X/Y noise channels offset by 100 for decorrelation), scales by `amplitude`, optionally boosts by a scalar Texture2D sampled at the same UV (`amplitude * (1 + capped(m) * gain)`, capped = m/(1+m)), and adds to `forces[i]`. Aliased Array<vec2> in/out — one physical buffer, in-place mutation. Resolution-independent: work scales with particle count, not canvas area. Replaces a per-pixel texture noise chain (simplex_field × 2 + math + mix) for any per-particle noise consumer.",
    inputs: {
        in: Array([f32; 2]) required,
        particles: Array(Particle) required,
        amplitude_modulator: Texture2D optional,
        amplitude: ScalarF32 optional,
        modulator_gain: ScalarF32 optional,
        z: ScalarF32 optional,
        noise_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 2]),
    },
    params: [
        ParamDef {
            name: "amplitude",
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "modulator_gain",
            label: "Modulator Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "z",
            label: "Z (Time)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "noise_scale",
            label: "Noise Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 16.0)),
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
    composition_notes: "Aliased Array<vec2> in/out (one buffer, in-place add). `amplitude` is port-shadow so a control wire (LFO / clip-trigger envelope / outer-card slider) drives noise energy live; `z` is port-shadow so a `time × scalar` math chain animates the noise field through Z. Wire any scalar Texture2D into `amplitude_modulator` to localize the noise (canonical FluidSim use wires the density texture); leave unwired for spatially-coherent noise at uniform amplitude everywhere. Replaces ~9 canvas-sized nodes in FluidSim2D's per-pixel noise advection.",
    examples: ["FluidSimulation"],
    picker: { label: "Turbulence (simplex)", category: Atom },
    summary: "Pushes particles around with a flowing noise field, giving organic, swirling motion. The classic turbulence force.",
    category: Particles2D,
    role: Filter,
    aliases: ["turbulence", "noise force", "flow", "simplex"],
    extra_fields: {
        dummy_modulator: Option<GpuTexture> = None,
    },
}

impl Primitive for SimplexNoiseForceAtParticles {
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
        let amplitude = ctx.scalar_or_param("amplitude", 0.001);
        let modulator_gain = ctx.scalar_or_param("modulator_gain", 2.0);
        let z = ctx.scalar_or_param("z", 0.0);
        let noise_scale = ctx.scalar_or_param("noise_scale", 2.0);
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
        let modulator_wire = ctx.inputs.texture_2d("amplitude_modulator");
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 8) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }
        let has_modulator: u32 = if modulator_wire.is_some() { 1 } else { 0 };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/simplex_noise_force_at_particles.wgsl"),
                "cs_main",
                "node.simplex_noise_force_at_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // Metal requires every declared shader binding to be present
        // at dispatch even when the kernel's `if has_modulator` branch
        // skips the sample. Cache a 1×1 white texture as the unwired
        // fallback bind; allocated once per instance.
        let dummy = self.dummy_modulator.get_or_insert_with(|| {
            let tex = gpu.device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::Rgba8Unorm,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                label: "node.simplex_noise_force_at_particles dummy modulator",
                mip_levels: 1,
            });
            gpu.device.upload_texture(&tex, &[255u8, 255, 255, 255]);
            tex
        });
        let modulator_tex = modulator_wire.unwrap_or(dummy);

        let uniforms = NoiseUniforms {
            active_count,
            amplitude,
            modulator_gain,
            z,
            noise_scale,
            has_modulator,
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: in_forces,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: modulator_tex,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.simplex_noise_force_at_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec2_in_out_required_particles_and_optional_modulator() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();

        assert_eq!(
            SimplexNoiseForceAtParticles::TYPE_ID,
            "node.simplex_noise_force_at_particles"
        );
        let names: Vec<&str> = SimplexNoiseForceAtParticles::INPUTS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "in",
                "particles",
                "amplitude_modulator",
                "amplitude",
                "modulator_gain",
                "z",
                "noise_scale",
                "active_count",
            ]
        );
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[0].ty,
            PortType::Array(vec2_layout)
        );
        assert!(SimplexNoiseForceAtParticles::INPUTS[0].required);
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[1].ty,
            PortType::Array(particle_layout)
        );
        assert!(SimplexNoiseForceAtParticles::INPUTS[1].required);
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[2].ty,
            PortType::Texture2D
        );
        assert!(!SimplexNoiseForceAtParticles::INPUTS[2].required);

        assert_eq!(SimplexNoiseForceAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            SimplexNoiseForceAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec2_layout)
        );

        let prim = SimplexNoiseForceAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn amplitude_z_and_noise_scale_port_shadow_params() {
        for name in ["amplitude", "z", "noise_scale", "modulator_gain"] {
            let has_port = SimplexNoiseForceAtParticles::INPUTS
                .iter()
                .any(|p| p.name == name);
            let has_param = SimplexNoiseForceAtParticles::PARAMS
                .iter()
                .any(|p| p.name == name);
            assert!(has_port, "input port '{name}' missing");
            assert!(has_param, "param '{name}' missing");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexNoiseForceAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.simplex_noise_force_at_particles"
        );
    }
}
