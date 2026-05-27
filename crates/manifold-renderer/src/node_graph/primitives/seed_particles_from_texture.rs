//! `node.seed_particles_from_texture` — exact-placement particle seeding
//! from a Texture2D density mask.
//!
//! Two-pass GPU dispatch:
//!   1. **compact** — scan the mask, atomically append every bright
//!      (R > 0.1) texel's UV to a flat `bright_list` buffer. A single
//!      atomic counter tracks the list length.
//!   2. **place** — for each active particle `i`, assign it
//!      `bright_list[i mod count]` with sub-texel hash-jitter so multiple
//!      particles sharing a bright pixel don't visually stack.
//!
//! Guarantees: every active particle lands on a bright texel of the mask
//! (life = 1). The legacy rejection-sampling design left dead particles
//! at the origin when the mask was sparse — exact placement removes that
//! failure mode. When `active_count > bright_count`, particles wrap
//! round-robin across the mask (jittered) so dense particle counts on
//! sparse masks remain visually coherent.
//!
//! ParticleText and FluidSim2D (seed cycle) both consume this. Any
//! Texture2D works as the mask — camera frame, procedural pattern,
//! threshold of an upstream image. `output_width / output_height`
//! control how the mask maps into particle UV space (mask is centered at
//! 0.5, 0.5 and sized `(tex_width/output_w, tex_height/output_h)` of the
//! unit square).
//!
//! The bright_list / counter scratch buffers are allocated lazily on
//! first dispatch, sized to `mask.width * mask.height` worst-case (every
//! texel bright). Realloc on mask-dim change. Counter is zeroed via blit
//! at the start of every dispatch (the cross-encoder transition
//! double-duties as a hazard barrier between previous-frame work and
//! the current compact pass).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedFromTextureUniforms {
    active_count: u32,
    frame_seed: u32,
    tex_width: u32,
    tex_height: u32,
    output_width: f32,
    output_height: f32,
    list_capacity: u32,
    _pad: u32,
}

crate::primitive! {
    name: SeedParticlesFromTexture,
    type_id: "node.seed_particles_from_texture",
    purpose: "Exact-placement particle seeding from a Texture2D density mask. Two-pass dispatch: (1) compact — scan the mask, atomically append every bright texel's UV (R > 0.1) into a flat list; (2) place — assign each active particle a UV via round-robin (i mod bright_count) with sub-texel jitter. Guarantees every particle lands alive on the mask: zero dead particles regardless of mask sparsity. When active_count > bright_count, particles wrap-around the list (jittered so they don't stack). When the mask is empty every particle is parked dead at center.",
    inputs: {
        mask: Texture2D required,
        active_count: ScalarF32 optional,
        output_width: ScalarF32 optional,
        output_height: ScalarF32 optional,
        frame_seed: ScalarF32 optional,
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
    composition_notes: "output_width / output_height set how the mask maps to particle UV space: mask centered at (0.5, 0.5), sized (tex_width/output_width, tex_height/output_height) of the unit square. Full-frame masks → set output_w/h equal to mask dims. Text or sub-region rasters → match the upstream render box. Bright threshold is hardcoded at 0.1. active_count / output_width / output_height / frame_seed are port-shadows-param — wire from system.generator_input or a math chain to drive them live; fall back to the inline value when unwired. Internal bright_list scratch sized to `mask.width × mask.height` (vec2<f32> per texel); reallocs on mask-dim change.",
    examples: [],
    picker: { label: "Seed Particles From Texture", category: Atom },
    extra_fields: {
        // place_main pipeline. The macro-allocated `pipeline` field
        // holds the compact_main pipeline.
        place_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        // Scratch buffers for the two-pass exact-placement design.
        bright_list: Option<GpuBuffer> = None,
        counter: Option<GpuBuffer> = None,
        // Cached mask dims (width, height). Reallocate bright_list when
        // these change so capacity always covers every mask texel.
        cached_mask_dims: (u32, u32) = (0, 0)
    },
}

impl Primitive for SeedParticlesFromTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;
        let output_width = ctx.scalar_or_param("output_width", 1920.0);
        let output_height = ctx.scalar_or_param("output_height", 1080.0);
        let frame_seed = ctx.scalar_or_param("frame_seed", 0.0).round() as u32;

        let Some(mask) = ctx.inputs.texture_2d("mask") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let mask_width = mask.width;
        let mask_height = mask.height;

        let gpu = ctx.gpu_encoder();

        // Lazy-allocate scratch buffers. Realloc when mask dims change so
        // bright_list capacity always covers worst-case (every texel
        // bright). Counter is always 4 bytes; we allocate it alongside
        // for symmetric lifetime.
        let needs_alloc = self.bright_list.is_none()
            || self.cached_mask_dims != (mask_width, mask_height);
        if needs_alloc {
            let total_texels = u64::from(mask_width) * u64::from(mask_height);
            let list_bytes = total_texels.max(1) * 8; // vec2<f32> stride
            self.bright_list = Some(gpu.device.create_buffer(list_bytes));
            self.counter = Some(gpu.device.create_buffer(4));
            self.cached_mask_dims = (mask_width, mask_height);
        }
        let bright_list = self
            .bright_list
            .as_ref()
            .expect("bright_list just allocated");
        let counter = self.counter.as_ref().expect("counter just allocated");
        let list_capacity = (bright_list.size / 8) as u32;

        // Lazy-compile pipelines (compact_main → self.pipeline,
        // place_main → self.place_pipeline). Both entry points live in
        // the same WGSL source.
        const SHADER_SRC: &str =
            include_str!("shaders/seed_particles_from_texture.wgsl");
        if self.pipeline.is_none() {
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "compact_main",
                "node.seed_particles_from_texture.compact",
            ));
        }
        if self.place_pipeline.is_none() {
            self.place_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "place_main",
                "node.seed_particles_from_texture.place",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(gpu.device.create_sampler(&GpuSamplerDesc::default()));
        }
        let compact_pipeline = self.pipeline.as_ref().expect("just inserted");
        let place_pipeline = self.place_pipeline.as_ref().expect("just inserted");

        let uniforms = SeedFromTextureUniforms {
            active_count,
            frame_seed,
            tex_width: mask_width,
            tex_height: mask_height,
            output_width,
            output_height,
            list_capacity,
            _pad: 0,
        };

        // 1. Zero the counter via blit. Switching encoder type (compute
        //    → blit → compute) also acts as a hazard barrier so the
        //    compact pass reads `counter == 0` rather than whatever the
        //    previous frame's place pass left in it.
        gpu.native_enc.clear_buffer(counter);

        // 2. Compact pass — scan mask, append bright UVs.
        let compact_bindings = [
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
            GpuBinding::Buffer {
                binding: 3,
                buffer: bright_list,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: counter,
                offset: 0,
            },
        ];
        gpu.native_enc.dispatch_compute(
            compact_pipeline,
            &compact_bindings,
            [mask_width.div_ceil(16), mask_height.div_ceil(16), 1],
            "node.seed_particles_from_texture.compact",
        );

        // 3. Buffer-scope memory barrier — without this, the GPU may
        //    overlap the compact pass's writes to (counter, bright_list)
        //    with the place pass's reads of the same, and particles
        //    sample stale/partial state.
        gpu.native_enc.compute_memory_barrier_buffers();

        // 4. Place pass — assign each particle a UV from bright_list.
        gpu.native_enc.dispatch_compute(
            place_pipeline,
            &compact_bindings,
            [active_count.div_ceil(256), 1, 1],
            "node.seed_particles_from_texture.place",
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
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(
            SeedParticlesFromTexture::TYPE_ID,
            "node.seed_particles_from_texture"
        );
        // mask is required Texture2D; active_count / output_width /
        // output_height / frame_seed are optional port-shadows.
        let mask = SeedParticlesFromTexture::INPUTS
            .iter()
            .find(|p| p.name == "mask")
            .expect("mask input");
        assert_eq!(mask.ty, PortType::Texture2D);
        assert!(mask.required);
        for name in ["active_count", "output_width", "output_height", "frame_seed"] {
            let port = SeedParticlesFromTexture::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
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
