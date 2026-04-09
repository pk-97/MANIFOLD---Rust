// Mechanical port of StylizedFeedbackFX.cs.
// Same logic, same variables, same constants, same edge cases.

use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
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
        id: EffectTypeId::STYLIZED_FEEDBACK,
        display_name: "Stylized Feedback",
        category: "Post-Process",
        available: true,
        osc_prefix: "stylizedFeedback",
        legacy_discriminant: Some(20),
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::continuous("Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
            ParamSpec::continuous("Rotate", -10.0, 10.0, 0.0, "F2", "Rotate"),
            ParamSpec::whole_labels("Mode", 0.0, 2.0, 0.0, &["Screen", "Add", "Max"], "Mode"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::STYLIZED_FEEDBACK,
        create: |device| Box::new(StylizedFeedbackFX::new(device)),
    }
}

// StylizedFeedbackFX.cs line 34 — Mathf.Deg2Rad
const DEG_TO_RAD: f32 = std::f32::consts::PI / 180.0;

/// WGSL source — shared across all specialized mode variants.
const FEEDBACK_WGSL: &str = include_str!("shaders/fx_stylized_feedback_compute.wgsl");

// StylizedFeedbackFX.cs lines 34-37 — uniforms matching StylizedFeedbackEffect.shader Properties
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StylizedFeedbackUniforms {
    feedback_amount: f32, // _FeedbackAmount
    zoom: f32,            // _Zoom
    rotation: f32,        // _Rotation (radians)
    mode: f32,            // _Mode (rounded)
}

/// Per-owner state: the previous frame's feedback buffer.
struct StylizedFeedbackState {
    buffer: RenderTarget,
}

/// Stylized feedback effect — zoom/rotate/blend current frame with previous
/// frame's state buffer.
pub struct StylizedFeedbackFX {
    helper: ComputeDualBlitHelper,
    /// Specialized pipelines per mode: 0=Screen, 1=Additive, 2=Max.
    /// Metal compiler dead-code eliminates inactive if/else branches.
    pipeline_screen: manifold_gpu::GpuComputePipeline,
    pipeline_additive: manifold_gpu::GpuComputePipeline,
    pipeline_max: manifold_gpu::GpuComputePipeline,
    states: AHashMap<i64, StylizedFeedbackState>,
    width: u32,
    height: u32,
}

impl StylizedFeedbackFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_compute_pipeline(
                FEEDBACK_WGSL,
                "cs_main",
                &[("uniforms.mode", mode)],
                label,
            )
        };
        Self {
            helper: ComputeDualBlitHelper::new(device, FEEDBACK_WGSL, "StylizedFeedback Compute"),
            pipeline_screen: spec("0.0", "StylizedFeedback Screen"),
            pipeline_additive: spec("1.0", "StylizedFeedback Additive"),
            pipeline_max: spec("2.0", "StylizedFeedback Max"),
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
        if !self.states.contains_key(&ctx.owner_key) && self.width > 0 && self.height > 0 {
            let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
            let buffer = if let Some(pool) = gpu.pool {
                RenderTarget::new_pooled(
                    pool,
                    self.width,
                    self.height,
                    format,
                    "StylizedFeedback State",
                )
            } else {
                RenderTarget::new(
                    gpu.device,
                    self.width,
                    self.height,
                    format,
                    "StylizedFeedback State",
                )
            };
            gpu.clear_texture(&buffer.texture, 0.0, 0.0, 0.0, 0.0);
            self.states
                .insert(ctx.owner_key, StylizedFeedbackState { buffer });
        }

        let state = self.states.get(&ctx.owner_key).unwrap();

        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.5).min(0.98);
        let zoom = fx
            .param_values
            .get(1)
            .copied()
            .unwrap_or(0.95)
            .clamp(0.01, 10.0);
        let rotation = fx.param_values.get(2).copied().unwrap_or(0.0) * DEG_TO_RAD;
        let mode = fx.param_values.get(3).copied().unwrap_or(0.0).round();

        let uniforms = StylizedFeedbackUniforms {
            feedback_amount,
            zoom,
            rotation,
            mode,
        };

        // Select specialized pipeline based on mode
        let pipeline = match mode.round() as u32 {
            1 => &self.pipeline_additive,
            2 => &self.pipeline_max,
            _ => &self.pipeline_screen,
        };
        self.helper.dispatch_with(
            pipeline,
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
        gpu.copy_texture_to_texture(target, &state.buffer.texture, ctx.width, ctx.height);
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
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
