// TransformFX.cs → transform.rs
// Unity ref: Assets/Scripts/Compositing/Effects/TransformFX.cs
//
// Pattern: SimpleBlitHelper (single pass, not stateful).
// 4 params: X (p0, default 0), Y (p1, default 0), Zoom (p2, default 1), Rot (p3, default 0)
//
// ShouldSkip logic (Unity TransformFX.cs:15-25):
//   - Always skip at clip level (clip-level Transform is handled in BlitClip uniforms)
//   - Skip if all params are at identity: X≈0, Y≈0, Zoom≈1, Rot≈0
//
// Because the Rust PostProcessEffect trait's should_skip() does not receive EffectContext,
// the is_clip_level guard lives inside apply() as a passthrough with identity uniforms.
// This produces source→target unchanged (scale=1, translate=0, rot=0 is identity in the shader)
// so the buffer swap in effect_chain correctly advances with unmodified content.

use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

// Mathf.Approximately threshold (same as Unity's 1e-5 epsilon)
const APPROX_EPSILON: f32 = 1e-5;

#[inline]
fn approximately(a: f32, b: f32) -> bool {
    (a - b).abs() <= APPROX_EPSILON
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniforms {
    translate_x:  f32,  // _TranslateX  — GetParam(0)
    translate_y:  f32,  // _TranslateY  — GetParam(1)
    scale:        f32,  // _Scale       — GetParam(2)
    rotation:     f32,  // _Rotation    — GetParam(3) * Deg2Rad
    aspect_ratio: f32,  // ctx.width / ctx.height
    _pad0:        f32,
    _pad1:        f32,
    _pad2:        f32,
}

/// Transform effect — translate, scale, and rotate at layer/master level.
/// Clip-level Transform is handled as compositor uniforms in BlitClip, not here.
/// Unity ref: TransformFX.cs
pub struct TransformFX {
    helper: SimpleBlitHelper,
}

impl TransformFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_transform.wgsl"),
                "Transform",
                std::mem::size_of::<TransformUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for TransformFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Transform
    }

    /// TransformFX.cs:15-25 — skip if all params are at identity.
    /// Note: the is_clip_level guard is in apply() because ctx is not available here.
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        let p = &fx.param_values;
        let x    = p.first().copied().unwrap_or(0.0);
        let y    = p.get(1).copied().unwrap_or(0.0);
        let zoom = p.get(2).copied().unwrap_or(1.0);
        let rot  = p.get(3).copied().unwrap_or(0.0);

        approximately(x, 0.0)
            && approximately(y, 0.0)
            && approximately(zoom, 1.0)
            && approximately(rot, 0.0)
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
        // TransformFX.cs:18 — clip-level Transform is handled as uniforms in BlitClip.
        // Use identity uniforms so source passes through to target unchanged.
        let (translate_x, translate_y, scale, rotation) = if ctx.is_clip_level {
            (0.0_f32, 0.0_f32, 1.0_f32, 0.0_f32)
        } else {
            let p = &fx.param_values;
            // TransformFX.cs:29-33 — SetUniforms: GetParam(0..3), p3*Deg2Rad, ctx.Width/Height
            let tx  = p.first().copied().unwrap_or(0.0);
            let ty  = p.get(1).copied().unwrap_or(0.0);
            let sc  = p.get(2).copied().unwrap_or(1.0);
            let rot = p.get(3).copied().unwrap_or(0.0) * DEG2RAD;
            (tx, ty, sc, rot)
        };

        let aspect_ratio = ctx.width as f32 / ctx.height as f32;

        let uniforms = TransformUniforms {
            translate_x,
            translate_y,
            scale,
            rotation,
            aspect_ratio,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Transform Pass",
            profiler,
        );
    }
}
