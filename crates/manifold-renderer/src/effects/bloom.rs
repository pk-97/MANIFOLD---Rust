// Mechanical port of Unity BloomFX.cs + BloomEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use super::HDR_BUFFER_DIVISOR;
use super::dual_texture_blit_helper::DualTextureBlitHelper;
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
use super::compute_dual_blit_helper::ComputeDualBlitHelper;

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
    mode: u32,           // 0=prefilter, 1=downsample, 2=upsample, 3=composite
    threshold: f32,      // _Threshold
    knee: f32,           // _Knee
    intensity: f32,      // _Intensity
    radius_scale: f32,   // _RadiusScale
    combine_weight: f32, // _CombineWeight
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
    mips_b: Vec<RenderTarget>, // Secondary mip chain (upsample target, avoids read-write hazard)
    count: usize,
}

// BloomFX.cs line 12 — BloomFX : SimpleBlitEffect, IStatefulEffect
pub struct BloomFX {
    helper: DualTextureBlitHelper,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_dual_blit: ComputeDualBlitHelper,
    states: AHashMap<i64, BloomState>,
    width: u32,  // BloomFX.cs line 17 — _width
    height: u32, // BloomFX.cs line 17 — _height
}

impl BloomFX {
    pub fn new(device: &wgpu::Device, hal_ctx: Option<&crate::hal_context::HalContext>) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/bloom.wgsl"),
                "Bloom",
                std::mem::size_of::<BloomUniforms>() as u64,
                hal_ctx,
            ),
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_dual_blit: ComputeDualBlitHelper::new(
                device,
                include_str!("shaders/bloom_compute.wgsl"),
                "Bloom Compute",
                std::mem::size_of::<BloomUniforms>() as u64,
                hal_ctx,
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    // BloomFX.cs lines 42-68 — GetOrCreatePyramid
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;
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
            mips_a.push(RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}")));
            mips_b.push(RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}")));
            count += 1;
            pw = (pw / 2).max(1);
            ph = (ph / 2).max(1);
        }

        self.states.insert(owner_key, BloomState { mips_a, mips_b, count });
    }
}

impl PostProcessEffect for BloomFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::BLOOM
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        _target_texture: &wgpu::Texture,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // ShouldSkip handles the amount <= 0 check at the chain level now.
        let amount = fx.param_values.first().copied().unwrap_or(0.187);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(gpu.device, ctx.owner_key);

        // Check once whether to use compute path for this frame
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let use_compute = gpu.has_hal_encoder();
        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let use_compute = false;

        let state = self.states.get(&ctx.owner_key).unwrap();
        if state.count == 0 {
            let skip_u = BloomUniforms {
                mode: 3, threshold: 0.0, knee: 0.0, intensity: 0.0,
                radius_scale: 1.0, combine_weight: 0.0,
                main_texel_size_x: 0.0, main_texel_size_y: 0.0,
                bloom_texel_size_x: 0.0, bloom_texel_size_y: 0.0,
                _pad0: 0.0, _pad1: 0.0,
            };
            let skip_uniforms = bytemuck::bytes_of(&skip_u);
            if use_compute {
                #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
                self.compute_dual_blit.dispatch_a_only(
                    gpu, source, target, skip_uniforms,
                    "Bloom Skip", ctx.width, ctx.height, profiler,
                );
            } else {
                self.helper.draw_main_only(
                    gpu, source, target, skip_uniforms,
                    "Bloom Skip", ctx.width, ctx.height, profiler,
                );
            }
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
            main_texel_size_x: 0.0, main_texel_size_y: 0.0,
            bloom_texel_size_x: 0.0, bloom_texel_size_y: 0.0,
            _pad0: 0.0, _pad1: 0.0,
        };

        // Pass 0: Prefilter
        let prefilter_u = BloomUniforms {
            mode: 0,
            main_texel_size_x: 1.0 / ctx.width as f32,
            main_texel_size_y: 1.0 / ctx.height as f32,
            ..base_uniforms
        };
        let prefilter_uniforms = bytemuck::bytes_of(&prefilter_u);
        if use_compute {
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            self.compute_dual_blit.dispatch_a_only(
                gpu, source, &state.mips_a[0].view, prefilter_uniforms,
                "Bloom Prefilter",
                state.mips_a[0].width, state.mips_a[0].height, profiler,
            );
        } else {
            self.helper.draw_main_only(
                gpu, source, &state.mips_a[0].view, prefilter_uniforms,
                "Bloom Prefilter",
                state.mips_a[0].width, state.mips_a[0].height, profiler,
            );
        }

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
            let down_uniforms = bytemuck::bytes_of(&down_u);
            if use_compute {
                #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
                self.compute_dual_blit.dispatch_a_only(
                    gpu, &state.mips_a[i - 1].view, &state.mips_a[i].view,
                    down_uniforms, "Bloom Down",
                    state.mips_a[i].width, state.mips_a[i].height, profiler,
                );
            } else {
                self.helper.draw_main_only(
                    gpu, &state.mips_a[i - 1].view, &state.mips_a[i].view,
                    down_uniforms, "Bloom Down",
                    state.mips_a[i].width, state.mips_a[i].height, profiler,
                );
            }
        }

        // Upsample chain: ping-pong mipsA → mipsB
        for i in (0..used_levels - 1).rev() {
            let hi_w = state.mips_a[i].width;
            let hi_h = state.mips_a[i].height;
            let lo_view = if i == used_levels - 2 {
                &state.mips_a[i + 1].view
            } else {
                &state.mips_b[i + 1].view
            };
            let lo_w = if i == used_levels - 2 {
                state.mips_a[i + 1].width
            } else {
                state.mips_b[i + 1].width
            };
            let lo_h = if i == used_levels - 2 {
                state.mips_a[i + 1].height
            } else {
                state.mips_b[i + 1].height
            };

            let up_u = BloomUniforms {
                mode: 2,
                main_texel_size_x: 1.0 / hi_w as f32,
                main_texel_size_y: 1.0 / hi_h as f32,
                bloom_texel_size_x: 1.0 / lo_w as f32,
                bloom_texel_size_y: 1.0 / lo_h as f32,
                ..base_uniforms
            };
            let up_uniforms = bytemuck::bytes_of(&up_u);
            if use_compute {
                #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
                self.compute_dual_blit.dispatch(
                    gpu, &state.mips_a[i].view, lo_view, &state.mips_b[i].view,
                    up_uniforms, "Bloom Up",
                    state.mips_b[i].width, state.mips_b[i].height, profiler,
                );
            } else {
                self.helper.draw(
                    gpu, &state.mips_a[i].view, lo_view, &state.mips_b[i].view,
                    up_uniforms, "Bloom Up",
                    state.mips_b[i].width, state.mips_b[i].height, profiler,
                );
            }
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
        let composite_uniforms = bytemuck::bytes_of(&composite_u);
        if use_compute {
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            self.compute_dual_blit.dispatch(
                gpu, source, &state.mips_b[0].view, target,
                composite_uniforms, "Bloom Composite",
                ctx.width, ctx.height, profiler,
            );
        } else {
            self.helper.draw(
                gpu, source, &state.mips_b[0].view, target,
                composite_uniforms, "Bloom Composite",
                ctx.width, ctx.height, profiler,
            );
        }
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        for state in self.states.values_mut() {
            let mut pw = (width / HDR_BUFFER_DIVISOR).max(1);
            let mut ph = (height / HDR_BUFFER_DIVISOR).max(1);
            let mut count = 0;
            for i in 0..state.mips_a.len() {
                if pw < MIN_SIZE || ph < MIN_SIZE { break; }
                state.mips_a[i] = RenderTarget::new(device, pw, ph, format, &format!("BloomMipA_{i}"));
                state.mips_b[i] = RenderTarget::new(device, pw, ph, format, &format!("BloomMipB_{i}"));
                count += 1;
                pw = (pw / 2).max(1);
                ph = (ph / 2).max(1);
            }
            state.count = count;
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for BloomFX {
    fn clear_state_for_owner(&mut self, _owner_key: i64) {
        // Bloom mips are fully overwritten each frame (prefilter → downsample → upsample).
        // No temporal accumulation, so clearing contents is a no-op. Keep entry alive.
    }
    fn cleanup_owner(&mut self, owner_key: i64) { self.states.remove(&owner_key); }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
