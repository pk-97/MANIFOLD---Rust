// Halation effect — separable Gaussian blur with threshold extraction.
// Improvement over Unity's 13-tap 2D cross kernel: separable 17-tap Gaussian
// produces smooth, gap-free glow at reduced resolution (D-32).
//
// 3 passes: ThresholdTintBlurH → BlurV → Composite.
// Pass 0 combines threshold extraction + tinting + horizontal Gaussian blur
// into a single dispatch, applying threshold/tint per sample.
// Effective coverage: 17×17 = 289 unique positions vs Unity's 13-point cross.

use super::HDR_BUFFER_DIVISOR;
use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;
use manifold_core::effects::EffectInstance;
use crate::effects::registration::EffectFactory;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::HALATION,
        display_name: "Halation",
        category: "Filmic",
        available: true,
        osc_prefix: "halation",
        legacy_discriminant: Some(34),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("thresh", "Thresh", 0.0, 1.0, 0.5, "F2", "Threshold"),
            ParamSpec::continuous("spread", "Spread", 0.0, 1.0, 0.5, "F2", "Spread"),
            ParamSpec::whole("hue", "Hue", 0.0, 360.0, 20.0, "Hue"),
            ParamSpec::continuous("sat", "Sat", 0.0, 1.0, 0.6, "F2", "Saturation"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HALATION,
        create: |device| Box::new(HalationFX::new(device)),
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalationUniforms {
    mode: u32,              // 0=ThresholdTintBlurH, 1=BlurV, 2=Composite
    amount: f32,            // _Amount
    threshold: f32,         // _Threshold
    spread: f32,            // _Spread
    tint_r: f32,            // _TintR
    tint_g: f32,            // _TintG
    tint_b: f32,            // _TintB
    main_texel_size_x: f32, // _MainTex_TexelSize.x
    main_texel_size_y: f32, // _MainTex_TexelSize.y
    halo_texel_size_x: f32, // _HaloTex_TexelSize.x (composite pass only)
    halo_texel_size_y: f32, // _HaloTex_TexelSize.y (composite pass only)
    _pad: f32,
}

/// Per-owner intermediate buffers (reduced-res, ping-pong for separable blur).
struct HalationState {
    buf_a: RenderTarget, // V blur output (final halo before composite)
    buf_b: RenderTarget, // Combined ThresholdTint+BlurH output
}

pub struct HalationFX {
    helper: ComputeDualBlitHelper,
    states: AHashMap<i64, HalationState>,
    width: u32,
    height: u32,
}

impl HalationFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeDualBlitHelper::new(
                device,
                include_str!("shaders/fx_halation_compute.wgsl"),
                "Halation Compute",
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

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
        let qw = (self.width / HDR_BUFFER_DIVISOR).max(1);
        let qh = (self.height / HDR_BUFFER_DIVISOR).max(1);
        let buf_a = if let Some(p) = pool {
            RenderTarget::new_pooled(p, qw, qh, format, &format!("HalationA_{owner_key}"))
        } else {
            RenderTarget::new(device, qw, qh, format, &format!("HalationA_{owner_key}"))
        };
        let buf_b = if let Some(p) = pool {
            RenderTarget::new_pooled(p, qw, qh, format, &format!("HalationB_{owner_key}"))
        } else {
            RenderTarget::new(device, qw, qh, format, &format!("HalationB_{owner_key}"))
        };
        self.states
            .insert(owner_key, HalationState { buf_a, buf_b });
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
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.0);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(gpu.device, gpu.pool, ctx.owner_key);

        let state = match self.states.get(&ctx.owner_key) {
            Some(s) => s,
            None => return,
        };

        let threshold = fx.param_values.get(1).map(|p| p.value).unwrap_or(0.5);
        let spread = fx.param_values.get(2).map(|p| p.value).unwrap_or(0.5);
        let hue = fx.param_values.get(3).map(|p| p.value).unwrap_or(20.0);
        let saturation = fx.param_values.get(4).map(|p| p.value).unwrap_or(0.6);
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

        // Pass 0: Combined ThresholdTint + Horizontal Gaussian blur.
        // Use half-res texel size so the H-blur radius matches Pass 1's V-blur radius —
        // both cover ±8 half-res pixels, giving a symmetric (isotropic) glow.
        let pass0_u = HalationUniforms {
            mode: 0,
            main_texel_size_x: 1.0 / qw as f32,
            main_texel_size_y: 1.0 / qh as f32,
            ..base
        };
        self.helper.dispatch_a_only(
            gpu,
            source,
            &state.buf_b.texture,
            bytemuck::bytes_of(&pass0_u),
            "Halation ThresholdTintBlurH",
            qw,
            qh,
        );

        // Pass 1: Vertical Gaussian blur — buf_b → buf_a (reduced-res)
        let pass1_u = HalationUniforms {
            mode: 1,
            main_texel_size_x: 1.0 / qw as f32,
            main_texel_size_y: 1.0 / qh as f32,
            ..base
        };
        self.helper.dispatch_a_only(
            gpu,
            &state.buf_b.texture,
            &state.buf_a.texture,
            bytemuck::bytes_of(&pass1_u),
            "Halation BlurV",
            qw,
            qh,
        );

        // Pass 2: Composite — source (full-res) + buf_a (reduced-res) → target
        let pass2_u = HalationUniforms {
            mode: 2,
            main_texel_size_x: 1.0 / ctx.width as f32,
            main_texel_size_y: 1.0 / ctx.height as f32,
            halo_texel_size_x: 1.0 / qw as f32,
            halo_texel_size_y: 1.0 / qh as f32,
            ..base
        };
        self.helper.dispatch(
            gpu,
            source,
            &state.buf_a.texture,
            target,
            bytemuck::bytes_of(&pass2_u),
            "Halation Composite",
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

