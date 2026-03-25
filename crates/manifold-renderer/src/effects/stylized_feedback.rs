// Mechanical port of StylizedFeedbackFX.cs.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use super::dual_texture_blit_helper::DualTextureBlitHelper;

// StylizedFeedbackFX.cs line 34 — Mathf.Deg2Rad
const DEG_TO_RAD: f32 = std::f32::consts::PI / 180.0;

// StylizedFeedbackFX.cs lines 34-37 — uniforms matching StylizedFeedbackEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StylizedFeedbackUniforms {
    feedback_amount: f32, // _FeedbackAmount
    zoom:            f32, // _Zoom
    rotation:        f32, // _Rotation (radians)
    mode:            f32, // _Mode (rounded)
}

/// Per-owner state: the previous frame's feedback buffer.
struct StylizedFeedbackState {
    buffer: RenderTarget,
}

/// Stylized feedback effect — zoom/rotate/blend current frame with previous frame's state buffer.
pub struct StylizedFeedbackFX {
    helper: DualTextureBlitHelper,
    states: AHashMap<i64, StylizedFeedbackState>,
    width: u32,
    height: u32,
}

/// Clear a RenderTarget to transparent black via a render pass.
/// Unity ref: RenderTextureUtil.Clear() — zeros texture contents.
fn clear_render_target(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Clear RT"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

impl StylizedFeedbackFX {
    pub fn new(device: &wgpu::Device, hal_ctx: Option<&crate::hal_context::HalContext>) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/fx_stylized_feedback.wgsl"),
                "StylizedFeedback",
                std::mem::size_of::<StylizedFeedbackUniforms>() as u64,
                hal_ctx,
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    /// Create state buffer and clear to black.
    /// Unity ref: GetOrCreateState + RenderTextureUtil.Clear()
    fn ensure_state(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let buffer = RenderTarget::new(device, self.width, self.height, format, "StylizedFeedback State");
            // Clear to black so first-frame shader reads black prev buffer,
            // producing feedback with black → matching Unity behavior.
            clear_render_target(encoder, &buffer.view);
            self.states.insert(owner_key, StylizedFeedbackState { buffer });
        }
    }
}

impl PostProcessEffect for StylizedFeedbackFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::STYLIZED_FEEDBACK
    }

    // ShouldSkip: default (param[0] <= 0) — matches Unity SimpleBlitEffect.ShouldSkip.

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        target_texture: &wgpu::Texture,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(gpu.device, gpu.encoder, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();

        // StylizedFeedbackFX.cs lines 34-37
        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.5).min(0.98);
        let zoom = fx.param_values.get(1).copied().unwrap_or(0.95);
        let rotation = fx.param_values.get(2).copied().unwrap_or(0.0) * DEG_TO_RAD;
        let mode = fx.param_values.get(3).copied().unwrap_or(0.0).round();

        let uniforms = StylizedFeedbackUniforms { feedback_amount, zoom, rotation, mode };

        // main_tex = source (current frame), secondary_tex = state buffer (previous frame)
        self.helper.draw(
            gpu,
            source, &state.buffer.view, target,
            bytemuck::bytes_of(&uniforms),
            "StylizedFeedback Pass",
            ctx.width, ctx.height,
            profiler,
        );

        // PostBlit: copy result → state buffer via GPU memcpy.
        // Unity ref: Graphics.CopyTexture(result, stateBuffer) — zero-cost memcpy.
        let state = self.states.get(&ctx.owner_key).unwrap();
        gpu.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &state.buffer.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ctx.width,
                height: ctx.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.states.clear();
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for StylizedFeedbackFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_owner(&mut self, owner_key: i64) { self.states.remove(&owner_key); }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
