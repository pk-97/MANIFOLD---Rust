//! Bridges `EffectNodeContext` ↔ `EffectInstance` + `PresetContext`
//! for the few node-graph primitives that still wrap a legacy
//! `PostProcessEffect` instance (Infrared, QuadMirror, WireframeDepth).
//!
//! Each wrapper primitive rebuilds the legacy effect's positional-param
//! `EffectInstance` and `PresetContext` per frame from its named
//! `EffectNodeContext`. The two helpers here are the shared bridge
//! code; they used to live in the AutoGain bundle's primitive file and
//! moved here when AutoGain was atom-decomposed (Tranche 7).

use manifold_core::PresetTypeId;
use manifold_core::effects::EffectInstance;

use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::ParamValue;

/// Build a legacy `EffectInstance` from a wrapper primitive's named
/// params. Mirrors the positional param layout the legacy
/// `EffectMetadata` declares — `param_order` must list names in the
/// registered order.
pub(super) fn build_effect_instance(
    type_id: &PresetTypeId,
    ctx: &EffectNodeContext<'_, '_>,
    param_order: &[&str],
) -> EffectInstance {
    let mut fx = EffectInstance::new(type_id.clone());
    fx.align_to_definition();
    fx.enabled = true;
    for (i, name) in param_order.iter().enumerate() {
        let Some(slot) = fx.param_values.get_mut(i) else {
            continue;
        };
        let value = match ctx.params.get(*name) {
            Some(ParamValue::Float(f)) => *f,
            Some(ParamValue::Enum(e)) => *e as f32,
            Some(ParamValue::Bool(b)) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            _ => continue,
        };
        slot.value = value;
    }
    fx
}

/// Build a legacy `PresetContext` from the graph's `EffectNodeContext`.
/// Width/height come from the output texture; time/beat/dt from the
/// frame's `FrameTime`. Fields the graph doesn't track
/// (`is_clip_level`, `output_width/height`) get sensible defaults —
/// primitives that need any of them should be ported rather than
/// wrapped.
///
/// `frame_count` is forwarded from `FrameTime` so wrapped legacy
/// effects (Infrared / QuadMirror / WireframeDepth / BlobTracking) get
/// correct throttling. Hardcoding it to 0 previously broke
/// BlobTracking's frame-stamped tracker and WireframeDepth's
/// mesh-rebuild gate.
pub(super) fn build_effect_context(
    ctx: &EffectNodeContext<'_, '_>,
    width: u32,
    height: u32,
) -> PresetContext {
    PresetContext {
        time: ctx.time.seconds.0,
        beat: ctx.time.beats.0,
        dt: ctx.time.delta.0 as f32,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: if height > 0 {
            width as f32 / height as f32
        } else {
            1.0
        },
        owner_key: ctx.owner_key,
        is_clip_level: false,
        frame_count: ctx.time.frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    }
}
