// Mechanical port of Unity HalationFX.cs + HalationEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use super::dual_texture_blit_helper::DualTextureBlitHelper;

// HalationEffect.shader uniforms
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalationUniforms {
    mode: u32,               // 0=ThresholdTintBlur, 1=BlurWide, 2=Composite
    amount: f32,             // _Amount
    threshold: f32,          // _Threshold
    spread: f32,             // _Spread
    tint_r: f32,             // _TintR
    tint_g: f32,             // _TintG
    tint_b: f32,             // _TintB
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    halo_texel_size_x: f32,  // _HaloTex_TexelSize.x
    halo_texel_size_y: f32,  // _HaloTex_TexelSize.y
    _pad: f32,
}

// HalationFX.cs lines 17-18 — per-owner intermediate buffers (half-res)
struct HalationState {
    buf_a: RenderTarget, // bufs[0]: ThresholdTintBlur output
    buf_b: RenderTarget, // bufs[1]: BlurWide output
}

// HalationFX.cs line 12 — HalationFX : SimpleBlitEffect, IStatefulEffect
pub struct HalationFX {
    helper: DualTextureBlitHelper,
    states: AHashMap<i64, HalationState>,
    width: u32,  // HalationFX.cs line 17 — _width
    height: u32, // HalationFX.cs line 17 — _height
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

    // HalationFX.cs lines 48-63 — GetOrCreateBuffers
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;
        // HalationFX.cs lines 54-55: half-resolution for blur performance
        let hw = (self.width / 2).max(1);
        let hh = (self.height / 2).max(1);
        let buf_a = RenderTarget::new(device, hw, hh, format, &format!("HalationA_{owner_key}"));
        let buf_b = RenderTarget::new(device, hw, hh, format, &format!("HalationB_{owner_key}"));
        self.states.insert(owner_key, HalationState { buf_a, buf_b });
    }

    // HalationFX.cs lines 21-40 — HsvToRgb (ported to Rust; NOT in shader)
    fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
        // h = Mathf.Repeat(h / 360f, 1f)
        let h = (h / 360.0).rem_euclid(1.0);
        // if (s <= 0f) return new Color(v, v, v)
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
    fn effect_type(&self) -> EffectType {
        EffectType::Halation
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,  // buffer in Unity
        target: &wgpu::TextureView,  // ctx.Host.GetTargetBuffer() in Unity
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // ShouldSkip handles the amount <= 0 check at the chain level now.
        let amount = fx.param_values.first().copied().unwrap_or(0.0);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = match self.states.get(&ctx.owner_key) {
            Some(s) => s,
            None => return,
        };

        // HalationFX.cs lines 72-79: set uniforms
        let threshold = fx.param_values.get(1).copied().unwrap_or(0.5);
        let spread = fx.param_values.get(2).copied().unwrap_or(0.5);
        // HalationFX.cs line 72: Color tint = HsvToRgb(fx.GetParam(3), fx.GetParam(4), 1f)
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

        // HalationFX.cs line 82: Graphics.Blit(buffer, bufs[0], material, 0)
        // Pass 0: ThresholdTintBlur. main_tex = source (full-res), halo_tex = dummy.
        // _MainTex_TexelSize = 1/source_width, 1/source_height.
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
            "Halation ThresholdTintBlur",
        );

        // HalationFX.cs lines 85-86: material.SetTexture("_HaloTex", bufs[0]); Blit(bufs[0], bufs[1], material, 1)
        // Pass 1: BlurWide. main_tex = bufs[0] (half-res), halo_tex = bufs[0].
        // Shader reads _HaloTex with _HaloTex_TexelSize. main_texel_size from bufs[0].
        let half_w = state.buf_a.width;
        let half_h = state.buf_a.height;
        let buf_b_w = state.buf_b.width;
        let buf_b_h = state.buf_b.height;

        self.helper.draw(
            device, queue, encoder,
            &state.buf_a.view,
            &state.buf_a.view,
            &state.buf_b.view,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 1,
                main_texel_size_x: 1.0 / half_w as f32,
                main_texel_size_y: 1.0 / half_h as f32,
                halo_texel_size_x: 1.0 / half_w as f32,
                halo_texel_size_y: 1.0 / half_h as f32,
                ..base
            }),
            "Halation BlurWide",
        );

        // HalationFX.cs lines 89-93: Blit(buffer, target, material, 2) with _HaloTex = bufs[1]
        // Pass 2: Composite. main_tex = source (full-res), halo_tex = bufs[1] (half-res blurred).
        self.helper.draw(
            device, queue, encoder,
            source,
            &state.buf_b.view,
            target,
            bytemuck::bytes_of(&HalationUniforms {
                mode: 2,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                halo_texel_size_x: 1.0 / buf_b_w as f32,
                halo_texel_size_y: 1.0 / buf_b_h as f32,
                ..base
            }),
            "Halation Composite",
        );
    }

    // HalationFX.cs lines 98-108 — ClearState (clears all owner buffers, does NOT release)
    fn clear_state(&mut self) {
        // Unity RenderTextureUtil.Clear() zeros the texture contents.
        // In wgpu we achieve the same by re-creating the textures (no direct clear API).
        // Drop all states; they will be re-created on next apply().
        self.states.clear();
    }

    // HalationFX.cs lines 42-46 — InitializeState / resize
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        let hw = (width / 2).max(1);
        let hh = (height / 2).max(1);
        for (key, state) in &mut self.states {
            state.buf_a = RenderTarget::new(device, hw, hh, format, &format!("HalationA_{key}"));
            state.buf_b = RenderTarget::new(device, hw, hh, format, &format!("HalationB_{key}"));
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for HalationFX {
    // HalationFX.cs lines 110-117 — ClearState(int ownerKey)
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }

    // HalationFX.cs lines 119-130 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
