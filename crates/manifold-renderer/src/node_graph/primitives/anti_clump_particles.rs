//! `node.anti_clump_particles` — modulator-weighted Brownian kick on
//! each live particle's `position.xy`.
//!
//! For each particle: optionally sample a `strength_modulator`
//! texture at `position.xy`, compute `capped = m / (1 + m)`, and add
//! `(hash3(i, frame) − 0.5) * strength * weight` to `position.xy`
//! where `weight = capped` if a modulator is wired, otherwise `1`.
//! Concentrates the kick where the modulator is bright — the
//! canonical FluidSim use wires the density texture so the kick
//! activates where particles have accumulated (textbook
//! "anti-clumping"), but any scalar texture works: an audio
//! amplitude band, a mask, a depth slice, etc. Without a modulator
//! wired the atom is a plain Brownian position jitter at uniform
//! strength everywhere.
//!
//! Sibling to [`super::array_diffuse_particles`] which kicks
//! `velocity` (ODE-state diffusion for attractor sims). Two distinct
//! atoms rather than one with a mode enum because the math, the
//! state field, and the modulator weighting are different —
//! splitting avoids the dead-state-param anti-pattern.
//!
//! Reusable for any per-particle Brownian-kick pipeline: fluid sims,
//! sparks, particle-text, crowd / flock simulations, audio-reactive
//! particle jitter, mask-driven turbulence.

use manifold_gpu::{
    GpuBinding, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`strength`
/// f32, `active_count` Int → i32), then the derived `frame_count` (u32), then the
/// injected optional-texture flag `use_strength_modulator` (u32, run() packs
/// is_some()), then the codegen-injected `dispatch_count`, padded to a 16-byte
/// multiple. 5 words + 3 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AntiClumpUniforms {
    strength: f32,
    active_count: i32,
    frame_count: u32,
    use_strength_modulator: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: AntiClumpParticles,
    type_id: "node.anti_clump_particles",
    purpose: "Modulator-weighted Brownian kick on each live particle's position.xy. With a `strength_modulator` texture wired, samples it at the particle's UV and applies `kick = (hash3(i, frame) − 0.5) * strength * capped(m)` where `capped = m / (1 + m)`. Without a modulator, applies plain `kick = (hash3) * strength` uniformly. Canonical FluidSim use wires density (kick concentrates where particles cluster); equally useful with audio amplitude maps, masks, depth slices, or any scalar texture. Sibling to node.spread_out (which kicks velocity, un-weighted) — separate atoms because the math, the state field, and the modulator weighting differ.",
    inputs: {
        in: Array(Particle) required,
        strength_modulator: Texture2D optional,
        strength: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("strength"),
            label: "Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.1)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `strength` is port-shadow so an LFO / audio band / outer-card slider can modulate the energy live. Wire a scalar Texture2D into `strength_modulator` to localize the kick (capped d/(1+d) weighting); leave it unwired for a uniform Brownian jitter. Frame seed (frame_count) reseeds the hash each frame so adjacent frames produce decorrelated kicks rather than a slow drift.",
    examples: [],
    picker: { label: "Anti-Clump Particles", category: Atom },
    summary: "Nudges particles apart where they bunch up, keeping the cloud evenly spread instead of collapsing into blobs.",
    category: Particles2D,
    role: Filter,
    aliases: ["anti-clump", "separation", "spread", "repel"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/anti_clump_particles_body.wgsl"),
    derived_uniforms: ["frame_count:u32"],
    extra_fields: {
        dummy_modulator: Option<GpuTexture> = None,
    },
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `frame_count` field. Matches `run()`'s own computation below.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.anti_clump_particles",
        recompute: |ctx| Some(vec![ctx.frame.frame_count as f32]),
    }
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

    // run() dispatches `active_count` threads, not pool capacity — a fused
    // region containing this atom caps its dispatch the same way.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let strength = ctx.scalar_or_param("strength", 0.0);
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let modulator_wire = ctx.inputs.texture_2d("strength_modulator");
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
        let has_modulator: u32 = if modulator_wire.is_some() { 1 } else { 0 };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident + OPTIONAL Texture2D + derived frame_count + use-flag).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.anti_clump_particles standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.anti_clump_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // Metal requires every declared shader binding to be present
        // at dispatch even when the kernel's `if has_modulator` branch
        // skips the sample. Cache a 1×1 white texture to bind when no
        // wire is connected; allocated once per instance.
        let dummy = self.dummy_modulator.get_or_insert_with(|| {
            let tex = gpu.device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::Rgba8Unorm,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                label: "node.anti_clump_particles dummy modulator",
                mip_levels: 1,
            });
            gpu.device.upload_texture(&tex, &[255u8, 255, 255, 255]);
            tex
        });
        let modulator_tex = modulator_wire.unwrap_or(dummy);

        let uniforms = AntiClumpUniforms {
            strength,
            active_count: active_count as i32,
            frame_count,
            use_strength_modulator: has_modulator,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Generated binding order: uniform(0), buf_in(1, particles read),
        // tex_strength_modulator(2), samp(3), buf_out(4, particles read_write).
        // `in`/`out` alias the particle buffer → bind it to both 1 and 4.
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
                    texture: modulator_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: particles,
                    offset: 0,
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
    fn declares_aliased_particle_in_out_and_optional_strength_modulator_texture() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(AntiClumpParticles::TYPE_ID, "node.anti_clump_particles");
        let names: Vec<&str> = AntiClumpParticles::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["in", "strength_modulator", "strength", "active_count"]
        );
        assert_eq!(
            AntiClumpParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(AntiClumpParticles::INPUTS[0].required);
        assert_eq!(AntiClumpParticles::INPUTS[1].ty, PortType::Texture2D);
        // The modulator is optional — unwired = plain Brownian kick.
        assert!(!AntiClumpParticles::INPUTS[1].required);

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

