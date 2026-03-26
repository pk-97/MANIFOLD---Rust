// Mechanical port of StylizedFeedbackFX.cs.
// Same logic, same variables, same constants, same edge cases.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use super::dual_texture_blit_helper::DualTextureBlitHelper;
#[cfg(target_os = "macos")]
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

/// Stylized feedback effect — zoom/rotate/blend current frame with previous frame's state buffer.
pub struct StylizedFeedbackFX {
    helper: DualTextureBlitHelper,
    #[cfg(target_os = "macos")]
    compute_dual_blit: ComputeDualBlitHelper,
    states: AHashMap<i64, StylizedFeedbackState>,
    width: u32,
    height: u32,
}

/// Clear a RenderTarget to transparent black (all zeros).
/// Unity ref: RenderTextureUtil.Clear() — zeros texture contents.
/// Uses `clear_texture()` instead of a render pass — avoids a full TBDR
/// tile load/store cycle for what is just a memset-to-zero.
fn clear_render_target(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
    encoder.clear_texture(texture, &wgpu::ImageSubresourceRange::default());
}

impl StylizedFeedbackFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/fx_stylized_feedback.wgsl"),
                "StylizedFeedback",
                std::mem::size_of::<StylizedFeedbackUniforms>() as u64,
                hal_ctx,
            ),
            #[cfg(target_os = "macos")]
            compute_dual_blit: ComputeDualBlitHelper::new(
                device,
                include_str!("shaders/fx_stylized_feedback_compute.wgsl"),
                "StylizedFeedback Compute",
                std::mem::size_of::<StylizedFeedbackUniforms>() as u64,
                hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            ),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }

    /// Create state buffer and clear to black.
    /// Unity ref: GetOrCreateState + RenderTextureUtil.Clear()
    #[allow(dead_code)]
    fn ensure_state(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let buffer = RenderTarget::new(device, self.width, self.height, format, "StylizedFeedback State");
            // Clear to black so first-frame shader reads black prev buffer,
            // producing feedback with black → matching Unity behavior.
            clear_render_target(encoder, &buffer.texture);
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

        // Ensure state buffer exists — native/hal clear or wgpu clear
        if !self.states.contains_key(&ctx.owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let buffer = RenderTarget::new(
                gpu.device, self.width, self.height, format, "StylizedFeedback State",
            );
            #[cfg(target_os = "macos")]
            let mut cleared = false;
            #[cfg(target_os = "macos")]
            if gpu.has_native_encoder() {
                let native_tex = unsafe {
                    crate::gpu_encoder::extract_native_texture(&buffer.texture)
                };
                let native_enc = unsafe { gpu.native_encoder_mut() }.unwrap();
                native_enc.clear_texture(&native_tex, 0.0, 0.0, 0.0, 0.0);
                cleared = true;
            }
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            if !cleared && gpu.has_hal_encoder() {
                type MetalApi = wgpu::hal::api::Metal;
                use wgpu::hal::{self as hal, CommandEncoder as _};
                let view_ptr = {
                    let g = unsafe { buffer.view.as_hal::<MetalApi>() }
                        .expect("state view not Metal");
                    &*g as *const _
                };
                let (hal_enc, _) = unsafe { gpu.hal_encoder_mut() }.unwrap();
                unsafe {
                    hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                        label: Some("Clear StylizedFeedback State"),
                        extent: wgpu::Extent3d {
                            width: self.width, height: self.height,
                            depth_or_array_layers: 1,
                        },
                        sample_count: 1,
                        color_attachments: &[Some(hal::ColorAttachment {
                            target: hal::Attachment {
                                view: &*view_ptr,
                                usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                            },
                            resolve_target: None,
                            ops: hal::AttachmentOps::LOAD_CLEAR
                                | hal::AttachmentOps::STORE,
                            clear_value: wgpu::Color::TRANSPARENT,
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        multiview_mask: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    }).expect("hal begin_render_pass failed");
                    hal_enc.end_render_pass();
                }
                cleared = true;
            }
            #[cfg(target_os = "macos")]
            if !cleared {
                clear_render_target(gpu.encoder.as_mut().unwrap(), &buffer.texture);
            }
            #[cfg(not(target_os = "macos"))]
            clear_render_target(gpu.encoder.as_mut().unwrap(), &buffer.texture);
            self.states.insert(ctx.owner_key, StylizedFeedbackState { buffer });
        }

        let state = self.states.get(&ctx.owner_key).unwrap();

        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.5).min(0.98);
        let zoom = fx.param_values.get(1).copied().unwrap_or(0.95);
        let rotation = fx.param_values.get(2).copied().unwrap_or(0.0) * DEG_TO_RAD;
        let mode = fx.param_values.get(3).copied().unwrap_or(0.0).round();

        let uniforms = StylizedFeedbackUniforms { feedback_amount, zoom, rotation, mode };
        let uniform_bytes = bytemuck::bytes_of(&uniforms);

        // Check once whether to use compute path for this frame
        #[cfg(target_os = "macos")]
        let use_compute = gpu.has_native_encoder() || {
            #[cfg(feature = "hal-encoding")]
            { gpu.has_hal_encoder() }
            #[cfg(not(feature = "hal-encoding"))]
            { false }
        };
        #[cfg(not(target_os = "macos"))]
        let use_compute = false;

        if use_compute {
            #[cfg(target_os = "macos")]
            self.compute_dual_blit.dispatch(
                gpu, source, &state.buffer.view, target, uniform_bytes,
                "StylizedFeedback Pass", ctx.width, ctx.height, profiler,
            );
        } else {
            self.helper.draw(
                gpu, source, &state.buffer.view, target, uniform_bytes,
                "StylizedFeedback Pass", ctx.width, ctx.height, profiler,
            );
        }

        // PostBlit: copy result → state buffer
        let state = self.states.get(&ctx.owner_key).unwrap();
        gpu.copy_texture_to_texture(
            target_texture, &state.buffer.texture, ctx.width, ctx.height,
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
