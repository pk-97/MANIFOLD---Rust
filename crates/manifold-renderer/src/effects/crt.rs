// Mechanical port of Unity CrtFX.cs + CrtEffect.shader.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use super::dual_texture_blit_helper::DualTextureBlitHelper;

// CrtFX.cs lines 8-11 — uniforms matching CrtEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtUniforms {
    mode: u32,               // 0=prefilter, 1=downsample, 2=composite
    amount: f32,             // _Amount
    scanlines: f32,          // _Scanlines
    glow: f32,               // _Glow
    curvature: f32,          // _Curvature
    style: f32,              // _Style
    glow_threshold: f32,     // _GlowThreshold = lerp(0.15, 0.05, style)
    screen_height: f32,      // _ScreenHeight
    main_texel_size_x: f32,  // _MainTex_TexelSize.x
    main_texel_size_y: f32,  // _MainTex_TexelSize.y
    main_texel_size_z: f32,  // _MainTex_TexelSize.z (width in pixels, for phosphor mask)
    _pad: f32,
}

// CrtFX.cs lines 20-24 — CrtState
struct CrtState {
    half_res: RenderTarget,   // CrtFX.cs: halfRes
    quarter_res: RenderTarget, // CrtFX.cs: quarterRes
}

// CrtFX.cs line 13 — CrtFX : SimpleBlitEffect, IStatefulEffect
pub struct CrtFX {
    helper: DualTextureBlitHelper,
    states: AHashMap<i64, CrtState>, // CrtFX.cs: ownerStates
    width: u32,  // CrtFX.cs: _width
    height: u32, // CrtFX.cs: _height
}

impl CrtFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/fx_crt.wgsl"),
                "CRT",
                std::mem::size_of::<CrtUniforms>() as u64,
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    // CrtFX.cs lines 34-52 — GetOrCreateState
    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = wgpu::TextureFormat::Rgba16Float;

        // CrtFX.cs lines 40-43
        let hw = (self.width / 2).max(1);
        let hh = (self.height / 2).max(1);
        let qw = (self.width / 4).max(1);
        let qh = (self.height / 4).max(1);

        self.states.insert(owner_key, CrtState {
            half_res: RenderTarget::new(device, hw, hh, format, &format!("CrtGlowHalf_{owner_key}")),
            quarter_res: RenderTarget::new(device, qw, qh, format, &format!("CrtGlowQuarter_{owner_key}")),
        });
    }
}

impl PostProcessEffect for CrtFX {
    fn effect_type(&self) -> EffectType {
        EffectType::CRT
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
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // ShouldSkip handles the amount <= 0 check at the chain level now.
        let amount = fx.param_values.first().copied().unwrap_or(1.0);

        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = match self.states.get(&ctx.owner_key) {
            Some(s) => s,
            None => return,
        };

        // CrtFX.cs line 61
        let style = fx.param_values.get(4).copied().unwrap_or(0.5);

        // CrtFX.cs line 64: material.SetFloat("_GlowThreshold", Mathf.Lerp(0.15f, 0.05f, style))
        let glow_threshold = 0.15_f32 + (0.05 - 0.15) * style; // lerp(0.15, 0.05, style)

        // ── Pass 0: Prefilter — source → halfRes ──────────────────────────────
        // CrtFX.cs line 66: Graphics.Blit(buffer, state.halfRes, material, 0)
        // _MainTex_TexelSize = 1/source_width, 1/source_height (Unity auto-sets from SOURCE)
        self.helper.draw(
            device, queue, encoder,
            source,                       // main_tex = buffer (source)
            &self.helper.dummy_view,      // glow_tex = dummy (not read in mode 0)
            &state.half_res.view,         // target = halfRes
            bytemuck::bytes_of(&CrtUniforms {
                mode: 0,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.397),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.0),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                main_texel_size_z: ctx.width as f32,
                _pad: 0.0,
            }),
            "CRT Prefilter",
            state.half_res.width, state.half_res.height,
            profiler,
        );

        // ── Pass 1: Downsample — halfRes → quarterRes ─────────────────────────
        // CrtFX.cs line 69: Graphics.Blit(state.halfRes, state.quarterRes, material, 1)
        // _MainTex_TexelSize = 1/halfRes_width, 1/halfRes_height (SOURCE = halfRes)
        let hw = state.half_res.width;
        let hh = state.half_res.height;
        let qw = state.quarter_res.width;
        self.helper.draw(
            device, queue, encoder,
            &state.half_res.view,         // main_tex = halfRes
            &self.helper.dummy_view,      // glow_tex = dummy (not read in mode 1)
            &state.quarter_res.view,      // target = quarterRes
            bytemuck::bytes_of(&CrtUniforms {
                mode: 1,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.397),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.0),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / hw as f32,
                main_texel_size_y: 1.0 / hh as f32,
                main_texel_size_z: qw as f32, // not used in downsample, but kept consistent
                _pad: 0.0,
            }),
            "CRT Downsample",
            state.quarter_res.width, state.quarter_res.height,
            profiler,
        );

        // ── Pass 2: CRT Composite — source + quarterRes(glow) → target ────────
        // CrtFX.cs lines 72-80: material.SetTexture("_GlowTex", state.quarterRes); Blit(buffer, target, 2)
        // _MainTex_TexelSize = 1/source_width, 1/source_height (SOURCE = buffer)
        self.helper.draw(
            device, queue, encoder,
            source,                       // main_tex = buffer (source)
            &state.quarter_res.view,      // glow_tex = quarterRes (_GlowTex)
            target,                       // output = target
            bytemuck::bytes_of(&CrtUniforms {
                mode: 2,
                amount,
                scanlines: fx.param_values.get(1).copied().unwrap_or(0.397),
                glow: fx.param_values.get(2).copied().unwrap_or(0.3),
                curvature: fx.param_values.get(3).copied().unwrap_or(0.0),
                style,
                glow_threshold,
                screen_height: ctx.height as f32,
                main_texel_size_x: 1.0 / ctx.width as f32,
                main_texel_size_y: 1.0 / ctx.height as f32,
                main_texel_size_z: ctx.width as f32,
                _pad: 0.0,
            }),
            "CRT Composite",
            ctx.width, ctx.height,
            profiler,
        );
    }

    // CrtFX.cs lines 87-94 — ClearState (clears but keeps buffers alive)
    fn clear_state(&mut self) {
        // In Unity: RenderTextureUtil.Clear() — no equivalent needed in wgpu;
        // contents are overwritten each frame. No-op is correct.
    }

    // CrtFX.cs line 125 — CleanupAllOwners (resize = recreate)
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = wgpu::TextureFormat::Rgba16Float;
        for (owner_key, state) in self.states.iter_mut() {
            let hw = (width / 2).max(1);
            let hh = (height / 2).max(1);
            let qw = (width / 4).max(1);
            let qh = (height / 4).max(1);
            state.half_res = RenderTarget::new(device, hw, hh, format, &format!("CrtGlowHalf_{owner_key}"));
            state.quarter_res = RenderTarget::new(device, qw, qh, format, &format!("CrtGlowQuarter_{owner_key}"));
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for CrtFX {
    // CrtFX.cs lines 96-103 — ClearState(ownerKey): clear but keep alive
    fn clear_state_for_owner(&mut self, _owner_key: i64) {
        // Contents overwritten each frame; no-op is correct.
    }

    // CrtFX.cs lines 105-113 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
