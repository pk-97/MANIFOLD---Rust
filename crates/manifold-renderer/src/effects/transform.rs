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

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::AffineTransform;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::TRANSFORM,
        display_name: "Transform",
        category: "Spatial",
        available: true,
        osc_prefix: "transform",
        legacy_discriminant: Some(0),
        params: &[
            ParamSpec::continuous("x", "X", -1.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("y", "Y", -1.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("zoom", "Zoom", 0.1, 5.0, 1.0, "F2", ""),
            ParamSpec::continuous("rot", "Rot", -180.0, 180.0, 0.0, "F2", ""),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::TRANSFORM,
        create: |device| Box::new(TransformFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::TRANSFORM,
    primitive: AffineTransform,
    handle: "transform",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("x"),
            label: "X",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "transform", param: "translate_x" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("y"),
            label: "Y",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "transform", param: "translate_y" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("zoom"),
            label: "Zoom",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "transform", param: "scale" },
            convert: ParamConvert::Float,
        },
        // Rotation flows through as a plain Float passthrough — the
        // primitive surfaces degrees + screen-CW directly, so the
        // outer slider and the inner editor agree on units. The
        // deg→rad + sign-flip lives inside the primitive.
        ParamBinding {
            id: Cow::Borrowed("rot"),
            label: "Rot",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "transform", param: "rotation" },
            convert: ParamConvert::Float,
        },
    ],
    // Transform never skips — even at identity it's the chain's
    // pass-through stage, and skip would change buffer-swap timing.
    skip: SkipMode::Never,
}

const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniforms {
    translate_x: f32,  // _TranslateX  — GetParam(0)
    translate_y: f32,  // _TranslateY  — GetParam(1)
    scale: f32,        // _Scale       — GetParam(2)
    rotation: f32,     // _Rotation    — GetParam(3) * Deg2Rad
    aspect_ratio: f32, // ctx.width / ctx.height
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Transform effect — translate, scale, and rotate at layer/master level.
/// Clip-level Transform is handled as compositor uniforms in BlitClip, not here.
/// Unity ref: TransformFX.cs
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct TransformFX {
    helper: ComputeBlitHelper,
}

impl TransformFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_transform.wgsl"),
                "Transform",
            ),
        }
    }
}

impl PostProcessEffect for TransformFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::TRANSFORM
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // TransformFX.cs:18 — clip-level Transform is handled as uniforms in BlitClip.
        // Use identity uniforms so source passes through to target unchanged.
        let (translate_x, translate_y, scale, rotation) = if ctx.is_clip_level {
            (0.0_f32, 0.0_f32, 1.0_f32, 0.0_f32)
        } else {
            let p = &fx.param_values;
            // TransformFX.cs:29-33 — SetUniforms: GetParam(0..3), p3*Deg2Rad, ctx.Width/Height
            let tx = p.first().map(|pv| pv.value).unwrap_or(0.0);
            let ty = p.get(1).map(|pv| pv.value).unwrap_or(0.0);
            let sc = p.get(2).map(|pv| pv.value).unwrap_or(1.0);
            // Negate: Unity UVs are Y-up (0 at bottom), compute shader
            // pixel coords are Y-down (id.y=0 at top). Same rotation matrix
            // spins the opposite direction visually, so flip the sign.
            let rot = -(p.get(3).map(|pv| pv.value).unwrap_or(0.0) * DEG2RAD);
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

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Transform Pass",
            ctx.width,
            ctx.height,
        );
    }
}
