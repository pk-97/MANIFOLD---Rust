// Halation effect — separable Gaussian blur with threshold extraction.
// Improvement over Unity's 13-tap 2D cross kernel: separable 17-tap Gaussian
// produces smooth, gap-free glow at reduced resolution (D-32).
//
// 4 passes (vs Unity's 3): ThresholdTint → BlurH → BlurV → Composite.
// Effective coverage: 17×17 = 289 unique positions vs Unity's 13-point cross.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use super::HDR_BUFFER_DIVISOR;
use super::dual_texture_blit_helper::DualTextureBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalationUniforms {
    mode: u32,               // 0=ThresholdTint, 1=BlurH, 2=BlurV, 3=Composite
    amount: f32,             // _Amount
    threshold: f32,          // _Threshold
    spread: f32,             // _Spread
    tint_r: f32,             // _TintR
    tint_g: f32,             // _TintG
    tint_b: f32,             // _TintB
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    halo_texel_size_x: f32,  // _HaloTex_TexelSize.x (composite pass only)
    halo_texel_size_y: f32,  // _HaloTex_TexelSize.y (composite pass only)
    _pad: f32,
}

/// Per-owner intermediate buffers (reduced-res, ping-pong for separable blur).
struct HalationState {
    buf_a: RenderTarget, // ThresholdTint output / V blur output
    buf_b: RenderTarget, // H blur output
}

pub struct HalationFX {
    helper: DualTextureBlitHelper,
    states: AHashMap<i64, HalationState>,
    width: u32,
    height: u32,
}

impl HalationFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/fx_halation.wgsl"),
                "Halation",
                std::mem::size_of::<HalationUniforms>() as u64,
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;
        let qw = (self.width / HDR_BUFFER_DIVISOR).max(1);
        let qh = (self.height / HDR_BUFFER_DIVISOR).max(1);
        let buf_a = RenderTarget::new(device, qw, qh, format, &format!("HalationA_{owner_key}"));
        let buf_b = RenderTarget::new(device, qw, qh, format, &format!("HalationB_{owner_key}"));
        self.states.insert(owner_key, HalationState { buf_a, buf_b });
    }

    // HalationFX.cs lines 21-40 — HsvToRgb
    fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
        let h = (h / 360.0).rem_euclid(1.0);
        if s <= 0.0 {
            return (v, v, v);
        }
        let hh = h * 6.0;
        let sector = hh as i32;
        let frac = hh - sector as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * frac);
        let t = v * (1.0 - s * (1.0 - frac));
        match sector % 6 {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        }
    }
}

impl PostProcessEffect for HalationFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::HALATION
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let amount = fx.param_values.first().copied().unwrap_or(0.0);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = match self.states.get(&ctx.owner_key) {
            Some(s) => s,
            None => return,
        };

        let threshold = fx.param_values.get(1).copied().unwrap_or(0.5);
        let spread = fx.param_values.get(2).copied().unwrap_or(0.5);
        let hue = fx.param_values.get(3).copied().unwrap_or(20.0);
        let saturation = fx.param_values.get(4).copied().unwrap_or(0.6);
        let (tint_r, tint_g, tint_b) = Self::hsv_to_rgb(hue, saturation, 1.0);

        let base = HalationUniforms {
            mode: 0,
            amount,
            threshold,
            spread,
            tint_r,
            tint_g,
            tint_b,
            main_texel_size_x: 0.0,
            main_texel_size_y: 0.0,
            halo_texel_size_x: 0.0,
            halo_texel_size_y: 0.0,
            _pad: 0.0,
        };

        let qw = state.buf_a.width;
        let qh = state.buf_a.height;

        // Pass 0: ThresholdTint — source (full-res) → buf_a (quarter-res)
        self.helper.draw(
            device, queue, encoder,
            source,
            &self.helper.dummy_view,
            &state.buf_a.view,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 0,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                ..base
            }),
            "Halation ThresholdTint",
            qw, qh,
            profiler,
        );

        // Pass 1: Horizontal Gaussian blur — buf_a → buf_b (quarter-res)
        self.helper.draw(
            device, queue, encoder,
            &state.buf_a.view,
            &self.helper.dummy_view,
            &state.buf_b.view,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 1,
                main_texel_size_x: 1.0 / qw as f32,
                main_texel_size_y: 1.0 / qh as f32,
                ..base
            }),
            "Halation BlurH",
            qw, qh,
            profiler,
        );

        // Pass 2: Vertical Gaussian blur — buf_b → buf_a (quarter-res)
        self.helper.draw(
            device, queue, encoder,
            &state.buf_b.view,
            &self.helper.dummy_view,
            &state.buf_a.view,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 2,
                main_texel_size_x: 1.0 / qw as f32,
                main_texel_size_y: 1.0 / qh as f32,
                ..base
            }),
            "Halation BlurV",
            qw, qh,
            profiler,
        );

        // Pass 3: Composite — source (full-res) + buf_a (quarter-res) → target
        self.helper.draw(
            device, queue, encoder,
            source,
            &state.buf_a.view,
            target,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 3,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                halo_texel_size_x: 1.0 / qw as f32,
                halo_texel_size_y: 1.0 / qh as f32,
                ..base
            }),
            "Halation Composite",
            ctx.width, ctx.height,
            profiler,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        let qw = (width / HDR_BUFFER_DIVISOR).max(1);
        let qh = (height / HDR_BUFFER_DIVISOR).max(1);
        for (key, state) in &mut self.states {
            state.buf_a = RenderTarget::new(device, qw, qh, format, &format!("HalationA_{key}"));
            state.buf_b = RenderTarget::new(device, qw, qh, format, &format!("HalationB_{key}"));
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for HalationFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
