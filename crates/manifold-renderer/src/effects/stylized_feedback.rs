// Mechanical port of StylizedFeedbackFX.cs.
// Same logic, same variables, same constants, same edge cases.

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use super::dual_texture_blit_helper::DualTextureBlitHelper;
use super::simple_blit_helper::SimpleBlitHelper;

// StylizedFeedbackFX.cs line 34 — Mathf.Deg2Rad
const DEG_TO_RAD: f32 = std::f32::consts::PI / 180.0;

// Passthrough blit shader: used for state buffer init (first frame).
const PASSTHROUGH_SHADER: &str = r#"
struct Uniforms { _pad: vec4<f32>, }
@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, }
@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source_tex, tex_sampler, in.uv);
}
"#;

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
    initialized: bool,
}

/// Stylized feedback effect — zoom/rotate/blend current frame with previous frame's state buffer.
pub struct StylizedFeedbackFX {
    helper: DualTextureBlitHelper,
    /// Passthrough blit for copying result into feedback state buffer.
    copy_blit: SimpleBlitHelper,
    states: HashMap<i64, StylizedFeedbackState>,
    width: u32,
    height: u32,
}

impl StylizedFeedbackFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: DualTextureBlitHelper::new(
                device,
                include_str!("shaders/fx_stylized_feedback.wgsl"),
                "StylizedFeedback",
                std::mem::size_of::<StylizedFeedbackUniforms>() as u64,
            ),
            copy_blit: SimpleBlitHelper::new(
                device,
                PASSTHROUGH_SHADER,
                "StylizedFeedback Copy",
                16, // vec4 pad
            ),
            states: HashMap::new(),
            width: 0,
            height: 0,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            self.states.insert(owner_key, StylizedFeedbackState {
                buffer: RenderTarget::new(device, self.width, self.height, format, "StylizedFeedback State"),
                initialized: false,
            });
        }
    }
}

impl PostProcessEffect for StylizedFeedbackFX {
    fn effect_type(&self) -> EffectType {
        EffectType::StylizedFeedback
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
    ) {
        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_state(device, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();

        if !state.initialized {
            // First frame: passthrough + prime state buffer
            self.copy_blit.draw(device, queue, encoder, source, target, &[0u8; 16], "StylizedFeedback Init");
            self.copy_blit.draw(device, queue, encoder, source, &state.buffer.view, &[0u8; 16], "StylizedFeedback Init State");
            let state = self.states.get_mut(&ctx.owner_key).unwrap();
            state.initialized = true;
            return;
        }

        // StylizedFeedbackFX.cs lines 34-37
        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.0).min(0.98);
        let zoom = fx.param_values.get(1).copied().unwrap_or(1.02);
        let rotation = fx.param_values.get(2).copied().unwrap_or(0.0) * DEG_TO_RAD;
        let mode = fx.param_values.get(3).copied().unwrap_or(0.0).round();

        let uniforms = StylizedFeedbackUniforms { feedback_amount, zoom, rotation, mode };

        // main_tex = source (current frame), secondary_tex = state buffer (previous frame)
        self.helper.draw(
            device, queue, encoder,
            source, &state.buffer.view, target,
            bytemuck::bytes_of(&uniforms),
            "StylizedFeedback Pass",
        );

        // PostBlit: copy result → state buffer
        let state = self.states.get(&ctx.owner_key).unwrap();
        self.copy_blit.draw(device, queue, encoder, target, &state.buffer.view, &[0u8; 16], "StylizedFeedback State Copy");
    }

    fn clear_state(&mut self) {
        for state in self.states.values_mut() { state.initialized = false; }
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        for state in self.states.values_mut() {
            state.buffer.resize(device, width, height);
            state.initialized = false;
        }
    }
}

impl StatefulEffect for StylizedFeedbackFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        if let Some(state) = self.states.get_mut(&owner_key) { state.initialized = false; }
    }
    fn cleanup_owner(&mut self, owner_key: i64) { self.states.remove(&owner_key); }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
