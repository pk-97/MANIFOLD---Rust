//! The unified scrub wire — one gesture engine, addresses not families
//! (UI_FUNNEL_DECOMPOSITION P-I, D4).
//!
//! A value-scrub gesture (slider drag, knob drag, discrete enum cycle) used to
//! be a *trio* of sibling `PanelAction` variants — `*Snapshot` / `*Changed` /
//! `*Commit` — one hand-written set per scrubable family. `PanelAction::Scrub`
//! collapses every such trio to one address + one phase: the panel names WHAT
//! it scrubs ([`ValueRef`], the ui-relative addressing the panels already
//! speak) and WHICH edge of the gesture this is ([`ScrubPhase`]). The app-side
//! `ui_bridge::scrub` handler resolves each address to a core write target and
//! runs the four operations (read baseline / apply live / live-command /
//! commit-command) its former trio arm ran — one undo entry per gesture,
//! byte-identical commands.
//!
//! The wire stays in `manifold-ui` (`ui` depends only on `foundation`), so
//! `ValueRef` carries only ui-relative addressing — `GraphParamTarget`,
//! `ParamId`, `LayerId`, `AudioSendId` — never a `manifold-core` type and never
//! a new id scheme (D4: reuse widget-tree D2's vocabulary). Whole-shape restore
//! data (the resolved core target captured for the mid-gesture snapshot-stomp
//! guard) lives app-side in `ui_bridge::scrub::ScrubState`, not here.

use manifold_foundation::{LayerId, ParamId};

use super::{AudioShapeParam, GraphParamTarget, TrimKind, UiRelightField};

/// One edge of a scrub gesture — maps 1:1 onto the retired
/// `*Snapshot`/`*Changed`/`*Commit` trio (D4). The scrubbed value rides
/// [`ScrubPhase::Move`] only: `Begin` captures the undo baseline from the
/// model and `Commit` reads the final value back from the model, so neither
/// needs a payload value.
#[derive(Debug, Clone, PartialEq)]
pub enum ScrubPhase {
    /// Pointer-down / gesture start — was `*Snapshot` / `*DragBegin`. Captures
    /// the pre-gesture value as the undo baseline; emits no command.
    Begin,
    /// Live drag tick — was `*Changed`. Applies the new value locally for
    /// immediate feedback and ships a non-undoable live write to the content
    /// thread.
    Move(ScrubValue),
    /// Pointer-up / gesture end — was `*Commit`. Emits exactly one
    /// undo-tracked command spanning the whole gesture (baseline → final).
    Commit,
}

/// The value carried on [`ScrubPhase::Move`]. One variant per value *shape*;
/// never a `manifold-core` type (the wire is `manifold-ui`). More shapes
/// (range, shape-param) are added as the P-I family batches port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrubValue {
    /// A single scalar — an opacity, a card param, a knob position.
    Scalar(f32),
    /// A `(min, max)` sub-range — a modulation trim-bar drag (driver / audio /
    /// Ableton), where one gesture carries both edges.
    Range(f32, f32),
}

impl ScrubValue {
    /// The scalar payload, or `None` for a non-scalar shape.
    pub fn scalar(self) -> Option<f32> {
        match self {
            ScrubValue::Scalar(v) => Some(v),
            ScrubValue::Range(..) => None,
        }
    }

    /// The `(min, max)` range payload, or `None` for a non-range shape.
    pub fn range(self) -> Option<(f32, f32)> {
        match self {
            ScrubValue::Range(min, max) => Some((min, max)),
            ScrubValue::Scalar(_) => None,
        }
    }
}

/// The address a scrub gesture targets — the ui-relative addressing the panels
/// already emit (D4: reuse `GraphParamTarget` / `ParamId` / `LayerId` /
/// `AudioSendId`, no new id scheme). One variant per scrubable family; the
/// app-side handler resolves each to a core write target exactly as its former
/// trio arm did. More families are added as the P-I batches port.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueRef {
    /// An exposed card param on an effect/generator graph — was the
    /// `ParamSnapshot` / `ParamChanged` / `ParamCommit` trio.
    Param(GraphParamTarget, ParamId),
    /// The master-opacity slider (master chrome) — was `MasterOpacity{Snapshot,
    /// Changed,Commit}`.
    MasterOpacity,
    /// The LED master-brightness slider — was `LedBrightness{Snapshot,Changed,
    /// Commit}`.
    LedBrightness,
    /// The active layer's opacity slider (layer chrome) — was
    /// `LayerOpacity{Snapshot,Changed,Commit}`. Resolves through the active
    /// layer, like its retired trio.
    LayerOpacity,
    /// A macro-bank knob by slot index — was `Macro{Snapshot,Changed,Commit}`.
    Macro(usize),
    /// A layer's audio-input gain (layer header) — was `AudioGain{Snapshot,
    /// Changed,Commit}`. Keyed by the layer id.
    LayerAudioGain(LayerId),
    /// A "3D Shading" relight knob on an effect/generator graph — was
    /// `RelightParam{Snapshot,Changed,Commit}`.
    RelightParam(GraphParamTarget, UiRelightField),
    /// A modulation trim-range handle (driver / audio-mod / Ableton sub-range
    /// bars) — was `Trim{Snapshot,Changed,Commit}`. `TrimKind` selects the
    /// backing store; the `(min, max)` value rides `ScrubValue::Range` on Move.
    Trim(TrimKind, GraphParamTarget, ParamId),
    /// An envelope target handle (orange bar, `target_normalized`) — was
    /// `Target{Snapshot,Changed,Commit}`.
    EnvelopeTarget(GraphParamTarget, ParamId),
    /// An envelope decay slider (`decay_beats`) — was
    /// `EnvDecay{Snapshot,Changed,Commit}`.
    EnvDecay(GraphParamTarget, ParamId),
    /// An audio-mod drawer shaping slider (sensitivity / attack / release) — was
    /// `AudioModShape{Snapshot,ParamChanged,Commit}`. The `AudioShapeParam`
    /// names which of the three scalars this gesture drags; the value rides
    /// `ScrubValue::Scalar` on Move (the restore path re-stamps the whole shape).
    AudioModShape(GraphParamTarget, ParamId, AudioShapeParam),
    /// An audio-mod Step-action amount slider — was
    /// `AudioModStepAmount{Snapshot,Changed,Commit}`. The dragged `amount` rides
    /// `ScrubValue::Scalar` on Move; the restore path re-stamps
    /// `TriggerAction::Step { amount, wrap }` (preserving the current wrap), and
    /// the undo baseline is the whole pre-drag `TriggerAction`.
    AudioModStepAmount(GraphParamTarget, ParamId),
}
