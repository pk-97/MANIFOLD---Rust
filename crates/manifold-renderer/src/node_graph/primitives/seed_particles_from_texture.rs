//! `node.spawn_from_image` — exact-placement particle seeding
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

use std::borrow::Cow;
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
    type_id: "node.spawn_from_image",
    purpose: "Exact-placement particle seeding from a Texture2D density mask. Two-pass dispatch: (1) compact — scan the mask, atomically append every bright texel's UV (R > 0.1) into a flat list; (2) place — assign each active particle a UV via round-robin (i mod bright_count) with sub-texel jitter. Guarantees every particle lands alive on the mask: zero dead particles regardless of mask sparsity. When active_count > bright_count, particles wrap-around the list (jittered so they don't stack). When the mask is empty every particle is parked dead at center.",
    inputs: {
        mask: Texture2D required,
        active_count: ScalarF32 optional,
        output_width: ScalarF32 optional,
        output_height: ScalarF32 optional,
        frame_seed: ScalarF32 optional,
        // Optional execution gate: when wired, the seed only RECOMPUTES on this
        // value's integer edges (+ the first frame). Wire it from the SAME trigger
        // that drives the downstream node.array_feedback's reset (e.g.
        // system.generator_input.trigger_count) — array_feedback adopts the seed
        // only on alloc/reset, so between resets the four-pass compaction is pure
        // waste. Unwired → recompute every frame (direct per-frame seeding).
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1024.0, 16_000_000.0)),
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
        ParamDef {
            name: Cow::Borrowed("output_width"),
            label: "Output Width",
            ty: ParamType::Float,
            default: ParamValue::Float(1920.0),
            range: Some((64.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("output_height"),
            label: "Output Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1080.0),
            range: Some((64.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frame_seed"),
            label: "Frame Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "output_width / output_height set how the mask maps to particle UV space: mask centered at (0.5, 0.5), sized (tex_width/output_width, tex_height/output_height) of the unit square. Full-frame masks → set output_w/h equal to mask dims. Text or sub-region rasters → match the upstream render box. Bright threshold is hardcoded at 0.1. active_count / output_width / output_height / frame_seed are port-shadows-param — wire from system.generator_input or a math chain to drive them live; fall back to the inline value when unwired. Internal bright_list scratch sized to `mask.width × mask.height` (vec2<f32> per texel); reallocs on mask-dim change.",
    examples: [],
    picker: { label: "Spawn From Image", category: Atom },
    summary: "Creates particles placed by the bright areas of an image, so a picture or mask becomes a cloud of points. Spawn density follows the image.",
    category: Particles2D,
    role: Source,
    aliases: ["spawn from image", "seed particles from texture", "seed from texture", "image particles"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        // The macro-allocated `pipeline` field holds count_main; these hold
        // the other three entry points of the deterministic four-pass compaction.
        scan_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        compact_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        place_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        // Scratch buffers for the deterministic exact-placement design.
        bright_list: Option<GpuBuffer> = None,
        counter: Option<GpuBuffer> = None,
        // Per-256-texel-block scratch: bright count (after count_main),
        // rewritten in place to each block's base offset (after scan_main).
        block_data: Option<GpuBuffer> = None,
        // Cached mask dims (width, height). Reallocate bright_list /
        // block_data when these change so capacity always covers the mask.
        cached_mask_dims: (u32, u32) = (0, 0),
        // Last observed `reset_trigger` integer, for edge-gated recompute. `None`
        // until the first frame (which always recomputes); after that the seed
        // skips its dispatches when the trigger hasn't advanced.
        last_reset_trigger: Option<i32> = None
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

        // Execution gate (see the `reset_trigger` input): when wired, recompute the
        // seed only on the trigger's integer edges (+ the first frame). The output
        // is adopted by node.array_feedback only on alloc/reset, so between resets
        // the four-pass compaction below is pure waste — skip it, the persistent
        // output buffer still holds the last seed. Unwired → never gates (every
        // frame recomputes, the direct per-frame seeding path). Cheap edge check
        // before any allocation or dispatch.
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            // `None` (first frame) ≠ Some(current) ⇒ edge ⇒ compute.
            let edge = self.last_reset_trigger != Some(current);
            self.last_reset_trigger = Some(current);
            if !edge {
                return;
            }
        }

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
        // bright) and block_data covers every 256-texel block. Counter is
        // always 4 bytes; allocated alongside for symmetric lifetime.
        let needs_alloc = self.bright_list.is_none()
            || self.cached_mask_dims != (mask_width, mask_height);
        if needs_alloc {
            let total_texels = u64::from(mask_width) * u64::from(mask_height);
            let list_bytes = total_texels.max(1) * 8; // vec2<f32> stride
            let num_blocks = total_texels.max(1).div_ceil(256);
            self.bright_list = Some(gpu.device.create_buffer(list_bytes));
            self.counter = Some(gpu.device.create_buffer(4));
            self.block_data = Some(gpu.device.create_buffer(num_blocks * 4));
            self.cached_mask_dims = (mask_width, mask_height);
        }
        let bright_list = self
            .bright_list
            .as_ref()
            .expect("bright_list just allocated");
        let counter = self.counter.as_ref().expect("counter just allocated");
        let block_data = self.block_data.as_ref().expect("block_data just allocated");
        let list_capacity = (bright_list.size / 8) as u32;
        let total_texels = mask_width * mask_height;
        let num_blocks = total_texels.div_ceil(256);

        // Lazy-compile the four entry points (count → scan → compact →
        // place), all in the same WGSL source. `self.pipeline` holds count_main.
        const SHADER_SRC: &str =
            include_str!("shaders/seed_particles_from_texture.wgsl");
        if self.pipeline.is_none() {
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "count_main",
                "node.spawn_from_image.count",
            ));
        }
        if self.scan_pipeline.is_none() {
            self.scan_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "scan_main",
                "node.spawn_from_image.scan",
            ));
        }
        if self.compact_pipeline.is_none() {
            self.compact_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "compact_main",
                "node.spawn_from_image.compact",
            ));
        }
        if self.place_pipeline.is_none() {
            self.place_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "place_main",
                "node.spawn_from_image.place",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(gpu.device.create_sampler(&GpuSamplerDesc::default()));
        }
        let count_pipeline = self.pipeline.as_ref().expect("just inserted");
        let scan_pipeline = self.scan_pipeline.as_ref().expect("just inserted");
        let compact_pipeline = self.compact_pipeline.as_ref().expect("just inserted");
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

        // Full binding set — each pass references only the subset it needs
        // (count: params/mask/block_data; scan: params/counter/block_data;
        // compact: params/mask/bright_list/block_data; place: particles/
        // params/bright_list/counter). Binding everything keeps one array.
        let bindings = [
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
            GpuBinding::Buffer {
                binding: 5,
                buffer: block_data,
                offset: 0,
            },
        ];

        // 1. Count bright texels per 256-texel block (deterministic, no atomics).
        gpu.native_enc.dispatch_compute(
            count_pipeline,
            &bindings,
            [num_blocks.div_ceil(64), 1, 1],
            "node.spawn_from_image.count",
        );
        // Each pass reads what the previous wrote — barrier between every stage.
        gpu.native_enc.compute_memory_barrier_buffers();

        // 2. Exclusive prefix sum of block counts → base offsets + grand total.
        gpu.native_enc.dispatch_compute(
            scan_pipeline,
            &bindings,
            [1, 1, 1],
            "node.spawn_from_image.scan",
        );
        gpu.native_enc.compute_memory_barrier_buffers();

        // 3. Compact — append bright UVs at each block's base offset, in scan
        //    order, so bright_list is canonical (race-free) every run.
        gpu.native_enc.dispatch_compute(
            compact_pipeline,
            &bindings,
            [num_blocks.div_ceil(64), 1, 1],
            "node.spawn_from_image.compact",
        );
        gpu.native_enc.compute_memory_barrier_buffers();

        // 4. Place — assign each particle a UV from bright_list (round-robin).
        gpu.native_enc.dispatch_compute(
            place_pipeline,
            &bindings,
            [active_count.div_ceil(256), 1, 1],
            "node.spawn_from_image.place",
        );
    }
}

impl SeedParticlesFromTexture {
    /// BUG-191 (partial fix, Lane 6 2026-07-17): this primitive is a
    /// barriered multi-pass hand-written-pipeline atom (same class as
    /// `ScatterOnMesh`, exempt from the codegen path per CLAUDE.md), so
    /// BUG-146's codegen-sweep prewarm never reached it, and no bundled
    /// preset happens to exercise `node.spawn_from_image` — its four
    /// pipelines stayed genuinely never-compiled until a project's own
    /// first live `run()`, same shape as BUG-037's `scatter_on_mesh` gap.
    /// Mirrors `ScatterOnMesh::prewarm_pipelines` exactly.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        const SHADER_SRC: &str = include_str!("shaders/seed_particles_from_texture.wgsl");
        device.create_compute_pipeline(SHADER_SRC, "count_main", "node.spawn_from_image.count");
        device.create_compute_pipeline(SHADER_SRC, "scan_main", "node.spawn_from_image.scan");
        device.create_compute_pipeline(SHADER_SRC, "compact_main", "node.spawn_from_image.compact");
        device.create_compute_pipeline(SHADER_SRC, "place_main", "node.spawn_from_image.place");
    }
}

/// BUG-191/BUG-037 gate. Run deliberately: `cargo test -p manifold-renderer
/// --features gpu-proofs node_graph::primitives::seed_particles_from_texture::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    /// Mirrors `scatter_on_mesh`'s identical test (BUG-037 precedent):
    /// order-independent by design (BUG-144's documented class) — `device`
    /// is process-global across a `--features gpu-proofs --lib` run, so
    /// another test's `GeneratorRegistry::prewarm_all` may already have
    /// warmed these entry points. Asserting "cache hit after MY prewarm
    /// call" is correct either way.
    #[test]
    fn prewarm_pipelines_populates_the_shared_compute_cache() {
        let device = crate::test_device();
        SeedParticlesFromTexture::prewarm_pipelines(&device);
        const SHADER_SRC: &str = include_str!("shaders/seed_particles_from_texture.wgsl");
        for (entry, label) in [
            ("count_main", "node.spawn_from_image.count"),
            ("scan_main", "node.spawn_from_image.scan"),
            ("compact_main", "node.spawn_from_image.compact"),
            ("place_main", "node.spawn_from_image.place"),
        ] {
            let cache_before_use = device.compute_pipeline_cache_len();
            let _pipeline = device.create_compute_pipeline(SHADER_SRC, entry, label);
            assert_eq!(
                device.compute_pipeline_cache_len(),
                cache_before_use,
                "{entry}'s pipeline compile after prewarm must be a cache hit"
            );
        }
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
            "node.spawn_from_image"
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
            .map(|p| p.name.as_ref())
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
        assert_eq!(node.type_id().as_str(), "node.spawn_from_image");
    }

    #[test]
    fn declares_optional_reset_trigger_gate_input() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let rt = SeedParticlesFromTexture::INPUTS
            .iter()
            .find(|p| p.name == "reset_trigger")
            .expect("reset_trigger input");
        assert_eq!(rt.ty, PortType::Scalar(ScalarType::F32));
        assert!(!rt.required, "reset_trigger is optional (unwired ⇒ recompute every frame)");
    }

    /// The gate only saves work if the preset actually wires a trigger into it.
    /// FluidSim2D routes the clip-trigger counter into seed_spawn.reset_trigger,
    /// so the four-pass compaction runs only on a reset edge, not every frame. Guards
    /// against the wire being dropped (which would silently revert to per-frame work).
    #[test]
    fn fluidsim_wires_a_trigger_into_the_seed_reset_trigger() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        let json = crate::node_graph::bundled_presets::bundled_preset_json(
            &manifold_core::PresetTypeId::new("FluidSim2D"),
        )
        .expect("FluidSim2D bundled");
        let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let flat = manifold_core::flatten::flatten_groups(&def).expect("FluidSim2D flattens");
        let seed = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.spawn_from_image")
            .expect("FluidSim has a seed node");
        let wired = flat
            .wires
            .iter()
            .any(|w| w.to_node == seed.id && w.to_port == "reset_trigger");
        assert!(
            wired,
            "FluidSim must wire a trigger into the seed's reset_trigger, else the seed \
             recomputes its 4-pass compaction every frame instead of only on reset"
        );
    }
}
