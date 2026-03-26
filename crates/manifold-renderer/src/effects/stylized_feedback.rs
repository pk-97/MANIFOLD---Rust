// Mechanical port of StylizedFeedbackFX.cs.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use super::compute_dual_blit_helper::ComputeDualBlitHelper;

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

/// Stylized feedback effect — zoom/rotate/blend current frame with previous
/// frame's state buffer.
pub struct StylizedFeedbackFX {
    helper: ComputeDualBlitHelper,
    states: AHashMap<i64, StylizedFeedbackState>,
    width: u32,
    height: u32,
}

impl StylizedFeedbackFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeDualBlitHelper::new(
                device,
                include_str!("shaders/fx_stylized_feedback_compute.wgsl"),
                "StylizedFeedback Compute",
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }
}

impl PostProcessEffect for StylizedFeedbackFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::STYLIZED_FEEDBACK
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;

        // Ensure state buffer exists — clear to black on creation
        if !self.states.contains_key(&ctx.owner_key)
            && self.width > 0
            && self.height > 0
        {
            let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
            let buffer = if let Some(pool) = gpu.pool {
                RenderTarget::new_pooled(
                    pool, self.width, self.height, format, "StylizedFeedback State",
                )
            } else {
                RenderTarget::new(
                    gpu.device, self.width, self.height, format, "StylizedFeedback State",
                )
            };
            gpu.clear_texture(&buffer.texture, 0.0, 0.0, 0.0, 0.0);
            self.states
                .insert(ctx.owner_key, StylizedFeedbackState { buffer });
        }

        let state = self.states.get(&ctx.owner_key).unwrap();

        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.5).min(0.98);
        let zoom = fx.param_values.get(1).copied().unwrap_or(0.95);
        let rotation =
            fx.param_values.get(2).copied().unwrap_or(0.0) * DEG_TO_RAD;
        let mode = fx.param_values.get(3).copied().unwrap_or(0.0).round();

        let uniforms = StylizedFeedbackUniforms {
            feedback_amount,
            zoom,
            rotation,
            mode,
        };

        self.helper.dispatch(
            gpu,
            source,
            &state.buffer.texture,
            target,
            bytemuck::bytes_of(&uniforms),
            "StylizedFeedback Pass",
            ctx.width,
            ctx.height,
        );

        // PostBlit: copy result → state buffer for next frame
        let state = self.states.get(&ctx.owner_key).unwrap();
        gpu.copy_texture_to_texture(
            target,
            &state.buffer.texture,
            ctx.width,
            ctx.height,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(
        &mut self,
        _device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
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
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.states.clear();
    }
}
