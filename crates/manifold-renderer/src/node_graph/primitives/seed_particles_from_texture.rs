//! `node.seed_particles_from_texture` — spawn particles at bright
//! texels of a Texture2D mask via rejection sampling.
//!
//! Bit-exact wrap of `generators/shaders/fluid_text_seed.wgsl` via
//! include_str. Each particle does up to 64 attempts to land on a
//! bright (R > 0.1) texel of the input mask; placed particles are
//! alive (life=1), failed-rejection particles are hidden (life=0).
//!
//! ParticleText uses this with a text-bitmap mask. Any Texture2D
//! works — camera frame, procedural mask, output of a Threshold
//! primitive on an upstream image. The text-region centering math
//! (output_width / output_height) is exposed as params so the
//! caller controls how the mask maps to particle UV space.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextSeedUniforms {
    active_count: u32,
    tex_width: u32,
    tex_height: u32,
    frame_seed: u32,
    output_width: f32,
    output_height: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: SeedParticlesFromTexture,
    type_id: "node.seed_particles_from_texture",
    purpose: "Spawn particles at bright texels (R > 0.1) of a Texture2D mask via rejection sampling. Each particle attempts up to 64 hash-based positions; placed particles are alive (life=1), failed-rejection particles sit at origin with life=0. Drives ParticleText and any \"mask → particle field\" graph. Re-runs each frame; pair with node.array_feedback if you want a stable spawn that persists.",
    inputs: {
        mask: Texture2D required,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1024.0, 16_000_000.0)),
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
        ParamDef {
            name: "output_width",
            label: "Output Width",
            ty: ParamType::Float,
            default: ParamValue::Float(1920.0),
            range: Some((64.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "output_height",
            label: "Output Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1080.0),
            range: Some((64.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "frame_seed",
            label: "Frame Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "output_width / output_height set how the mask texture maps to particle UV space: the mask is centered at (0.5, 0.5) and sized (tex_width/output_width, tex_height/output_height) of the unit square. For full-frame masks: set output_width and output_height equal to the mask dimensions. For text rendered into a sub-region: match the upstream rasterizer's render box. Bright threshold is hardcoded at 0.1 (matches the legacy text-seed shader).",
    examples: [],
    picker: { label: "Seed Particles From Texture", category: Atom },
}

impl Primitive for SeedParticlesFromTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 100_000,
        };
        let output_width = match ctx.params.get("output_width") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1920.0,
        };
        let output_height = match ctx.params.get("output_height") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1080.0,
        };
        let frame_seed = match ctx.params.get("frame_seed") {
            Some(ParamValue::Float(n)) => n.round() as u32,
            _ => 0,
        };

        let Some(mask) = ctx.inputs.texture_2d("mask") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_text_seed.wgsl"),
                "main",
                "node.seed_particles_from_texture",
            )
        });

        let uniforms = TextSeedUniforms {
            active_count,
            tex_width: mask.width,
            tex_height: mask.height,
            frame_seed,
            output_width,
            output_height,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: out_buf,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: mask,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.seed_particles_from_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn seed_from_texture_declares_mask_in_and_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(
            SeedParticlesFromTexture::TYPE_ID,
            "node.seed_particles_from_texture"
        );
        assert_eq!(SeedParticlesFromTexture::INPUTS.len(), 1);
        assert_eq!(SeedParticlesFromTexture::INPUTS[0].name, "mask");
        assert_eq!(
            SeedParticlesFromTexture::INPUTS[0].ty,
            PortType::Texture2D
        );
        assert!(SeedParticlesFromTexture::INPUTS[0].required);
        assert_eq!(SeedParticlesFromTexture::OUTPUTS.len(), 1);
        assert_eq!(SeedParticlesFromTexture::OUTPUTS[0].name, "particles");
        assert_eq!(
            SeedParticlesFromTexture::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn seed_from_texture_has_full_param_surface() {
        let names: Vec<&str> = SeedParticlesFromTexture::PARAMS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "max_capacity",
                "active_count",
                "output_width",
                "output_height",
                "frame_seed",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SeedParticlesFromTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.seed_particles_from_texture");
    }
}
