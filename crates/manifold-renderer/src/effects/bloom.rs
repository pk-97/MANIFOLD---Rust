// Mechanical port of Unity BloomFX.cs + BloomEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use super::HDR_BUFFER_DIVISOR;
use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Bloom;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::BLOOM,
        display_name: "Bloom",
        category: "Filmic",
        available: true,
        osc_prefix: "bloom",
        legacy_discriminant: Some(12),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.5, "F2", ""),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::BLOOM,
        create: |device| Box::new(BloomFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::BLOOM,
    primitive: Bloom,
    handle: "bloom",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.5,
            target: ParamTarget::HandleNode { handle: "bloom", param: "amount" },
            convert: ParamConvert::Float,
        },
    ],
    // Stateful: Bloom owns an expensive mip pyramid. SkipMode::OnZero
    // would tear it down on every amount → 0 drag and force a rebuild
    // on the way back up. Always splice — at `amount = 0` the
    // composite primitive returns the source unchanged.
    skip: SkipMode::Never,
}

// BloomFX.cs lines 19-25 — constants
const MAX_LEVELS: usize = 6;
const MIN_SIZE: u32 = 16;
const PREFILTER_THRESHOLD: f32 = 0.42;
const PREFILTER_KNEE: f32 = 0.24;
const BLOOM_LEVELS: usize = 3;
const RADIUS_AT_ZERO: f32 = 0.70;
const RADIUS_AT_ONE: f32 = 1.25;

// BloomFX.cs lines 9-11 — uniforms matching BloomEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniforms {
    mode: u32,               // 0=prefilter, 1=downsample, 2=upsample, 3=composite
    threshold: f32,          // _Threshold
    knee: f32,               // _Knee
    intensity: f32,          // _Intensity
    radius_scale: f32,       // _RadiusScale
    combine_weight: f32,     // _CombineWeight
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    bloom_texel_size_x: f32, // _BloomTex_TexelSize.x
    bloom_texel_size_y: f32, // _BloomTex_TexelSize.y
    _pad0: f32,
    _pad1: f32,
}

// BloomFX.cs lines 27-32 — OwnerPyramid
struct BloomState {
    mips_a: Vec<RenderTarget>, // Primary mip chain (downsample target, upsample source)
    mips_b: Vec<RenderTarget>, // Secondary mip chain (upsample target)
    count: usize,
}

// Bloom WGSL source — shared across all specialized pipeline variants.
const BLOOM_WGSL: &str = include_str!("shaders/bloom_compute.wgsl");

// BloomFX.cs line 12 — BloomFX : SimpleBlitEffect, IStatefulEffect
pub struct BloomFX {
    helper: ComputeDualBlitHelper,
    /// Specialized pipelines with mode baked in — one per bloom pass type.
    /// Metal compiler dead-code eliminates inactive branches in each variant.
    pipeline_prefilter: manifold_gpu::GpuComputePipeline, // mode=0
    pipeline_downsample: manifold_gpu::GpuComputePipeline, // mode=1
    pipeline_upsample: manifold_gpu::GpuComputePipeline,   // mode=2
    pipeline_composite: manifold_gpu::GpuComputePipeline,  // mode=3
    states: AHashMap<i64, BloomState>,
    width: u32,
    height: u32,
}

impl BloomFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_compute_pipeline(
                BLOOM_WGSL,
                "cs_main",
                &[("uniforms.mode", mode)],
                label,
            )
        };
        Self {
            helper: ComputeDualBlitHelper::new(device, BLOOM_WGSL, "Bloom Compute"),
            pipeline_prefilter: spec("0u", "Bloom Prefilter"),
            pipeline_downsample: spec("1u", "Bloom Downsample"),
            pipeline_upsample: spec("2u", "Bloom Upsample"),
            pipeline_composite: spec("3u", "Bloom Composite"),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    // BloomFX.cs lines 42-68 — GetOrCreatePyramid
    fn ensure_state(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        owner_key: i64,
    ) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let mut mips_a = Vec::new();
        let mut mips_b = Vec::new();
        let mut count = 0;

        // BloomFX.cs lines 51-52
        let mut pw = (self.width / HDR_BUFFER_DIVISOR).max(1);
        let mut ph = (self.height / HDR_BUFFER_DIVISOR).max(1);

        // BloomFX.cs lines 54-64
        for i in 0..MAX_LEVELS {
            if pw < MIN_SIZE || ph < MIN_SIZE {
                break;
            }
            let a = if let Some(p) = pool {
                RenderTarget::new_pooled(p, pw, ph, format, &format!("BloomMipA_{i}"))
            } else {
                RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}"))
            };
            let b = if let Some(p) = pool {
                RenderTarget::new_pooled(p, pw, ph, format, &format!("BloomMipB_{i}"))
            } else {
                RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}"))
            };
            mips_a.push(a);
            mips_b.push(b);
            count += 1;
            pw = (pw / 2).max(1);
            ph = (ph / 2).max(1);
        }

        self.states.insert(
            owner_key,
            BloomState {
                mips_a,
                mips_b,
                count,
            },
        );
    }
}

impl PostProcessEffect for BloomFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::BLOOM
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.5);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(gpu.device, gpu.pool, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();
        if state.count == 0 {
            let skip_u = BloomUniforms {
                mode: 3,
                threshold: 0.0,
                knee: 0.0,
                intensity: 0.0,
                radius_scale: 1.0,
                combine_weight: 0.0,
                main_texel_size_x: 0.0,
                main_texel_size_y: 0.0,
                bloom_texel_size_x: 0.0,
                bloom_texel_size_y: 0.0,
                _pad0: 0.0,
                _pad1: 0.0,
            };
            self.helper.dispatch_a_only_with(
                &self.pipeline_composite,
                gpu,
                source,
                target,
                bytemuck::bytes_of(&skip_u),
                "Bloom Skip",
                ctx.width,
                ctx.height,
            );
            return;
        }

        // BloomFX.cs lines 77-80
        let bloom_t = amount.clamp(0.0, 1.0);
        let used_levels = BLOOM_LEVELS.min(state.count);
        let t_smooth = bloom_t * bloom_t * (3.0 - 2.0 * bloom_t);
        let radius_scale = RADIUS_AT_ZERO + (RADIUS_AT_ONE - RADIUS_AT_ZERO) * t_smooth;

        let base_uniforms = BloomUniforms {
            mode: 0,
            threshold: PREFILTER_THRESHOLD,
            knee: PREFILTER_KNEE,
            intensity: amount,
            radius_scale,
            combine_weight: 1.0,
            main_texel_size_x: 0.0,
            main_texel_size_y: 0.0,
            bloom_texel_size_x: 0.0,
            bloom_texel_size_y: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        // Pass 0: Prefilter
        let prefilter_u = BloomUniforms {
            mode: 0,
            main_texel_size_x: 1.0 / ctx.width as f32,
            main_texel_size_y: 1.0 / ctx.height as f32,
            ..base_uniforms
        };
        self.helper.dispatch_a_only_with(
            &self.pipeline_prefilter,
            gpu,
            source,
            &state.mips_a[0].texture,
            bytemuck::bytes_of(&prefilter_u),
            "Bloom Prefilter",
            state.mips_a[0].width,
            state.mips_a[0].height,
        );

        // Downsample chain
        for i in 1..used_levels {
            let src_w = state.mips_a[i - 1].width;
            let src_h = state.mips_a[i - 1].height;
            let down_u = BloomUniforms {
                mode: 1,
                main_texel_size_x: 1.0 / src_w as f32,
                main_texel_size_y: 1.0 / src_h as f32,
                ..base_uniforms
            };
            self.helper.dispatch_a_only_with(
                &self.pipeline_downsample,
                gpu,
                &state.mips_a[i - 1].texture,
                &state.mips_a[i].texture,
                bytemuck::bytes_of(&down_u),
                "Bloom Down",
                state.mips_a[i].width,
                state.mips_a[i].height,
            );
        }

        // Upsample chain: ping-pong mipsA → mipsB
        for i in (0..used_levels - 1).rev() {
            let hi_w = state.mips_a[i].width;
            let hi_h = state.mips_a[i].height;
            let (lo_tex, lo_w, lo_h) = if i == used_levels - 2 {
                (
                    &state.mips_a[i + 1].texture,
                    state.mips_a[i + 1].width,
                    state.mips_a[i + 1].height,
                )
            } else {
                (
                    &state.mips_b[i + 1].texture,
                    state.mips_b[i + 1].width,
                    state.mips_b[i + 1].height,
                )
            };

            let up_u = BloomUniforms {
                mode: 2,
                main_texel_size_x: 1.0 / hi_w as f32,
                main_texel_size_y: 1.0 / hi_h as f32,
                bloom_texel_size_x: 1.0 / lo_w as f32,
                bloom_texel_size_y: 1.0 / lo_h as f32,
                ..base_uniforms
            };
            self.helper.dispatch_with(
                &self.pipeline_upsample,
                gpu,
                &state.mips_a[i].texture,
                lo_tex,
                &state.mips_b[i].texture,
                bytemuck::bytes_of(&up_u),
                "Bloom Up",
                state.mips_b[i].width,
                state.mips_b[i].height,
            );
        }

        // Final composite
        let bloom_w = state.mips_b[0].width;
        let bloom_h = state.mips_b[0].height;
        let composite_u = BloomUniforms {
            mode: 3,
            main_texel_size_x: 1.0 / ctx.width as f32,
            main_texel_size_y: 1.0 / ctx.height as f32,
            bloom_texel_size_x: 1.0 / bloom_w as f32,
            bloom_texel_size_y: 1.0 / bloom_h as f32,
            ..base_uniforms
        };
        self.helper.dispatch_with(
            &self.pipeline_composite,
            gpu,
            source,
            &state.mips_b[0].texture,
            target,
            bytemuck::bytes_of(&composite_u),
            "Bloom Composite",
            ctx.width,
            ctx.height,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
        for state in self.states.values_mut() {
            let mut pw = (width / HDR_BUFFER_DIVISOR).max(1);
            let mut ph = (height / HDR_BUFFER_DIVISOR).max(1);
            let mut count = 0;
            for i in 0..state.mips_a.len() {
                if pw < MIN_SIZE || ph < MIN_SIZE {
                    break;
                }
                state.mips_a[i] =
                    RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}"));
                state.mips_b[i] =
                    RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}"));
                count += 1;
                pw = (pw / 2).max(1);
                ph = (ph / 2).max(1);
            }
            state.count = count;
        }
    }

}
