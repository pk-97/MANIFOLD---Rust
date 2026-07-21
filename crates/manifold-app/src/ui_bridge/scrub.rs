//! Scrub-gesture snapshot state, regrouped off `Application`'s field list into
//! one struct (UI_FUNNEL_DECOMPOSITION P-B, D3).
//!
//! INTERIM SHAPE: this carries today's eight dispatch snapshot slots plus
//! `active_inspector_drag` VERBATIM — same `Option` fields, same semantics, no
//! reshaping. P-I replaces these ten in-flight slots with the addressed
//! `ScrubState`/`ValueRef` gesture engine (D4). Until then this is a pure
//! mechanical regroup: `dispatch`/`dispatch_inspector` take `&mut ScrubState`
//! instead of nine separate `&mut Option<…>` args, and `Application` owns one
//! `scrub: ScrubState` instead of nine loose fields.

use crate::app::ActiveInspectorDrag;
use manifold_core::audio_mod::{AudioModShape, TriggerAction};

/// The in-flight scrub-gesture snapshots threaded through `dispatch`. Every
/// field is the undo baseline captured on a drag's `…Snapshot`/`…DragBegin`
/// and consumed on its `…Commit`; `None` when no such gesture is active.
#[derive(Default)]
pub struct ScrubState {
    /// Slider drag snapshot for undo (opacity, slip, etc.). Threaded as
    /// `drag_snapshot` in the dispatch handlers (the arm bodies' name).
    pub slider_snapshot: Option<f32>,
    /// Trim drag snapshot (min, max) for undo.
    pub trim_snapshot: Option<(f32, f32)>,
    /// Envelope target-handle drag snapshot for undo.
    pub target_snapshot: Option<f32>,
    /// Envelope decay-slider drag snapshot for undo.
    pub decay_snapshot: Option<f32>,
    /// Audio-mod shaping-slider drag snapshot (whole shape) for undo.
    pub audio_shape_snapshot: Option<AudioModShape>,
    /// Step-Amount drag snapshot (PARAM_STEP_ACTIONS D8) for undo.
    pub audio_action_snapshot: Option<TriggerAction>,
    /// Band-divider drag snapshot `(low_hz, mid_hz)` for undo.
    pub audio_crossover_snapshot: Option<(f32, f32)>,
    /// Send-gain drag snapshot (old dB) for undo (D7).
    pub audio_send_gain_drag_snapshot: Option<f32>,
    /// Active inspector drag — prevents snapshot from overwriting dragged field.
    pub active_inspector_drag: Option<ActiveInspectorDrag>,
}
