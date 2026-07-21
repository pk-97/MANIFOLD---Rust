//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! The unified `ParamCardPanel` (effect + generator kinds) uses identical
//! layout constants, driver/envelope config builders, trim/target handle
//! builders, and formatting helpers across both kinds. This module is the
//! single source of truth for them.

use crate::{AudioSetupAction, ModulationAction, RootAction};
use super::DriverConfigAction;
use super::TrimKind;
use super::param_card::RowMod;
use crate::param_surface::ParamRow;
use super::{AudioShapeParam, GraphParamTarget, PanelAction, ScrubPhase, ScrubValue, ValueRef};
use crate::chrome::{Theme, View};
use crate::color;
use crate::drag::DragController;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
pub use crate::types::AbletonMappingStatus;
use manifold_foundation::LayerId;

// ── Shared layout constants ─────────────────────────────────────

pub(crate) const ROW_HEIGHT: f32 = 24.0;
pub(crate) const ROW_SPACING: f32 = 6.0;
/// Extra gap below an expanded modulation drawer, before the next param row. The
/// slider hugs its own drawer (ROW_SPACING above); this larger break after it
/// makes the next slider clearly a separate row. Paired in `row_drawer_height`
/// so build + height computation agree.
pub(crate) const DRAWER_BOTTOM_GAP: f32 = color::SPACE_L;
/// Left inset of a modulation drawer from the row's left edge — the same "belongs
/// to its parent" indent grammar as a layer nested inside a group on the timeline
/// (`color::GROUP_CHILD_INDENT_PX`), but a slighter single-level step: it says the
/// drawer is an operation *under* its slider without re-eating the label column the
/// way the old track-width indent did. Affects geometry only (`drawer_x`), not
/// height — height math is unchanged.
pub(crate) const DRAWER_INDENT: f32 = color::SPACE_L;
/// Padding the mod card extends BEYOND its content on the top + left + right, so
/// the slider, value, and arm buttons sit inset from the card edge instead of flush
/// against it (and so the top covers the slider's trim / target handles, which poke
/// a couple px above the track — `build_envelope_target` starts at `track.y - 2`).
/// The bottom needs none: the drawer's own internal `TOP_PAD` already insets the
/// last row. Visual only — does not move content or affect height math.
pub(crate) const MOD_CARD_PAD: f32 = 4.0;
// Card inner inset (§14.5 C). The canonical `SPACE_M`: with the card's 1px frame
// border that puts param-label content at `BORDER_W + SPACE_M` =
// `color::SECTION_CONTENT_INSET`, the one column the border-less chrome panels
// align to. `slider_w` / `label_width` / the header trailing-x all derive from
// this, so they cascade.
pub(crate) const PADDING: f32 = color::SPACE_M;
pub(crate) const GAP: f32 = color::SPACE_S;
// Param rows track the body-text token so the inspector matches layer-control
// chrome on the type ramp (they're the live instrument surface).
pub(crate) const FONT_SIZE: u16 = color::FONT_BODY;

pub(crate) const DE_BUTTON_SIZE: f32 = 20.0;
/// Gap *between* the three T/∿/A arm buttons. Tight, so they read as one group.
pub(crate) const DE_BUTTON_GAP: f32 = color::SPACE_S;
/// Gap between the slider's right edge and the T/∿/A group. Wider than the
/// inter-button gap so the value and the arm buttons don't crowd each other —
/// the slider reads as one cell, the arm group as another.
pub(crate) const MOD_LANE_GAP: f32 = color::SPACE_M;

/// Per-row modulation config tabs. The T/∿/A arm buttons stay on the row (one-
/// click arm); when two or more configs are active they share ONE drawer with a
/// tab strip rather than stacking three deep (§6.2). A single active config
/// shows directly with no tab strip, exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModTab {
    Envelope,
    Driver,
    Audio,
    Ableton,
}
/// Height of the modulation-config tab strip (only drawn when ≥2 configs active).
pub(crate) const MOD_TAB_STRIP_H: f32 = 18.0;
const MOD_TAB_H: f32 = 16.0;

/// Active tint for the audio-modulation ("A") button + drawer — a clean green,
/// kept distinct from the driver (teal) and envelope (orange) actives. Shares
/// the audio trim-handle green so the whole audio-mod identity reads as one.
pub(crate) const AUDIO_MOD_ACTIVE_C32: crate::node::Color32 = color::AUDIO_TRIM_BAR_C32;

// Height of the driver (LFO) drawer container. Three button rows + pads:
//   1. the 11-cell beat-division grid (sync rate),
//   2. the feel + free + invert modifiers (Straight/Dotted/Triplet/Free/Invert),
//   3. the 5 waveform-shape icons.
// Derived from the shared drawer metrics so the card's reserved height can't
// drift from what's actually drawn (mirrors `audio_config_height`).
pub(crate) fn driver_config_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(3)
}
pub(crate) const BEAT_DIV_COUNT: usize = 11;
pub(crate) const WAVEFORM_COUNT: usize = 5;

pub(crate) const ABL_CONFIG_HEIGHT: f32 = 24.0;

/// Height of the per-param audio-modulation drawer for param `i`. Rows: send
/// selector, the Feature row, the Band row, the Invert toggle, and the three
/// shaping sliders (Sensitivity/Attack/Release) — 7 rows, always. Derived
/// from the shared drawer metrics so the card's reserved height can never
/// drift from what's actually drawn.
///
/// PARAM_STEP_ACTIONS D8: a non-toggle, non-trigger param (`show_action`,
/// mirrors `build_audio_mod_drawer`'s own gate) additionally carries the
/// Action row, and — while armed to Step — the Amount slider + Wrap row.
/// The trailing Mode row (§9 U2) shows for an `is_trigger_gate` target
/// unconditionally, or for a slider row armed to Step/Random (D3). The layer
/// clip-trigger surface reserves its own height via
/// [`clip_trigger_drawer_height`] — its drawer is a different, smaller row
/// set built by [`build_clip_trigger_drawer`].
///
/// Adds [`crate::panels::drawer::METER_STRIP_H`] unconditionally:
/// `build_audio_mod_drawer`'s Sensitivity row carries a live meter on EVERY
/// audio-mod drawer, so the reserved height must always include the strip
/// too.
/// Row budget (must mirror `build_audio_mod_drawer`'s row order exactly):
/// Source + Listen (chips) + Sensitivity always; the Feature/Band matrix rows
/// only while the "Custom" cell is open; Invert + Attack + Release only where
/// they act — an `is_trigger_gate` target fires on the raw sensitivity-scaled
/// edge (BUG-242), so those three are placebo there and not built.
pub(crate) fn audio_config_height(info: &ParamRow, mod_state: &ParamModState, i: usize) -> f32 {
    let mut n = 3; // Source, Listen (chips + Custom), Sensitivity
    if !info.spec.is_trigger_gate {
        n += 3; // Invert, Attack, Release
    }
    if mod_state.audio_matrix_open.get(i).copied().unwrap_or(false) {
        n += 2; // Feature + Band (the Custom matrix)
    }
    let show_action = !info.spec.is_toggle && !info.spec.is_trigger;
    let action_idx = mod_state.audio_action_idx.get(i).copied().unwrap_or(0);
    if show_action {
        n += 1; // Action row
        if action_idx == 1 {
            n += 2; // Step-Amount slider + Wrap row
        }
    }
    if info.spec.is_trigger_gate || (show_action && action_idx != 0) {
        n += 1; // Mode row
    }
    crate::panels::drawer::uniform_rows_height(n) + crate::panels::drawer::METER_STRIP_H
}

/// Full-scale for the audio "Sensitivity" slider: 0..this.
pub(crate) const AUDIO_SENS_MAX: f32 = 4.0;
/// Full-scale for the audio "Attack" slider, in ms: 0..this.
pub(crate) const AUDIO_ATTACK_MAX_MS: f32 = 500.0;
/// Full-scale for the audio "Release" slider, in ms: 0..this.
pub(crate) const AUDIO_RELEASE_MAX_MS: f32 = 2000.0;
/// Leading-label width for the audio shaping sliders.
pub(crate) const AUDIO_SHAPE_LABEL_W: f32 = 52.0;

// `AudioModShape`'s own field defaults (mirrors `manifold_core::audio_mod`'s
// `default_sensitivity()`/`default_attack_ms()`/`default_release_ms()` —
// plain consts here so this crate doesn't need a `manifold-core` type import
// just to know a slider's right-click-reset target, BUG-061).
pub(crate) const AUDIO_SENS_DEFAULT: f32 = 1.0;
pub(crate) const AUDIO_ATTACK_DEFAULT_MS: f32 = 5.0;
pub(crate) const AUDIO_RELEASE_DEFAULT_MS: f32 = 120.0;

// ── PARAM_STEP_ACTIONS D2/D8: the Action/Amount/Wrap rows ──────────────
//
// This crate mirrors core enums rather than depending on `manifold-core`
// directly (the established convention — see `audio_kind_labels`/
// `AudioFeatureKind::ALL` above, and `AudioModSetTriggerMode`'s doc comment).
// `TriggerAction`/`WrapMode` are mirrored the same way.

/// Number of Action choices in the drawer's Action row (`[Continuous, Step,
/// Random]`, D2).
pub(crate) const AUDIO_ACTION_COUNT: usize = 3;
/// Number of Wrap choices in the drawer's Wrap row (`[Wrap, Bounce, Clamp]`,
/// D2), shown only while Action=Step.
pub(crate) const AUDIO_WRAP_COUNT: usize = 3;

/// Action-row button labels, index-parallel to core's `TriggerAction`
/// (`Continuous`/`Step`/`Random`).
pub(crate) fn audio_action_labels() -> [&'static str; AUDIO_ACTION_COUNT] {
    ["Cont", "Step", "Rand"]
}

/// Wrap-row button labels, index-parallel to core's `WrapMode`
/// (`Wrap`/`Bounce`/`Clamp`).
pub(crate) fn audio_wrap_labels() -> [&'static str; AUDIO_WRAP_COUNT] {
    ["Wrap", "Bounce", "Clamp"]
}

/// Length-row musical divisions (beats), for a clip trigger's `one_shot_beats`
/// (P3, D4). Same musical range the deleted Triggers matrix's stepper covered
/// (0.25..16 beats) collapsed to a fixed button row instead of a −/＋ stepper
/// — the drawer's other rows are all fixed button sets, not steppers.
pub(crate) const LENGTH_OPTIONS: [f32; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

/// Length-row button labels, "1b"-style, index-parallel to [`LENGTH_OPTIONS`].
pub(crate) fn length_labels() -> [String; 6] {
    LENGTH_OPTIONS.map(format_beats)
}

/// Nearest [`LENGTH_OPTIONS`] index to a `one_shot_beats` value — used both to
/// highlight the current selection and (by the clip-trigger caller) to snap a
/// legacy-migrated value that doesn't land exactly on an option.
pub(crate) fn length_option_index(beats: f32) -> usize {
    LENGTH_OPTIONS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (**a - beats).abs().total_cmp(&(**b - beats).abs()))
        .map(|(i, _)| i)
        .unwrap_or(2) // default 1b
}

/// Mirrors `manifold_core::audio_mod::default_step_amount` — D2's UI-seeding
/// default for a freshly-armed Step action: 1.0 for a discrete param
/// (whole_numbers/value_labels — one card-step per fire), or an eighth of the
/// param's range for a continuous one. Seeding only; once the user sets an
/// amount this is never consulted again.
pub(crate) fn default_step_amount(min: f32, max: f32, whole_numbers: bool) -> f32 {
    if whole_numbers {
        1.0
    } else {
        (max - min) / 8.0
    }
}

/// Full-scale span the Step-Amount slider maps across: the param's own
/// `max - min`, so dragging end-to-end reaches "one full range jump" per
/// fire in either direction — the same "size any other knob" feel D2 asks
/// for, without a fixed constant that would misfit wildly different param
/// ranges (an angle's ±π vs. a 0..200 discrete count).
fn step_amount_span(min: f32, max: f32) -> f32 {
    (max - min).abs().max(f32::EPSILON)
}

/// Map a signed Step amount to the slider's 0..1 fill, centered at 0.5 for
/// `amount == 0` (no jump), 0.0 at `-span`, 1.0 at `+span`.
pub(crate) fn step_amount_to_norm(amount: f32, min: f32, max: f32) -> f32 {
    let span = step_amount_span(min, max);
    (amount / span * 0.5 + 0.5).clamp(0.0, 1.0)
}

/// Inverse of [`step_amount_to_norm`] — a dragged 0..1 slider position back to
/// a signed amount.
pub(crate) fn norm_to_step_amount(norm: f32, min: f32, max: f32) -> f32 {
    let span = step_amount_span(min, max);
    (norm.clamp(0.0, 1.0) - 0.5) * 2.0 * span
}

// Arming the envelope shows two controls: the orange target handle on the
// parameter's own track (the value it's pulled toward) and a single "Decay"
// slider in a one-row drawer (how fast it falls back).
pub(crate) const ENV_CONFIG_HEIGHT: f32 = 30.0;
pub(crate) const ENV_DECAY_LABEL_W: f32 = 50.0;
/// Decay slider full-scale, in beats (0 → this).
pub(crate) const ENV_DECAY_MAX: f32 = 8.0;
/// Default decay for a freshly-armed envelope — mirrors core's
/// `DEFAULT_ENVELOPE_DECAY_BEATS` so the slider shows a usable value at once.
pub(crate) const DEFAULT_ENV_DECAY: f32 = 1.0;

pub(crate) const TRIM_BAR_W: f32 = 4.0;
pub(crate) const TARGET_BAR_W: f32 = 6.0;
pub(crate) const OVERLAY_INSET: f32 = 1.0;

pub(crate) const BEAT_DIV_LABELS: [&str; BEAT_DIV_COUNT] = [
    "1/32", "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32",
];

/// Period in beats for each grid button index — mirrors core's
/// `BeatDivision::from_button_index(idx).beats()` (the UI carries only the button
/// index, not the core enum). Quarter ("1/4") = 1 beat; "1" = a whole note = 4
/// beats. Used to prefill the Free type-in with the current sync period.
pub(crate) const BEAT_DIV_BEATS: [f32; BEAT_DIV_COUNT] =
    [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0];

// ── Shared node ID structs ──────────────────────────────────────

pub(crate) struct DriverConfigIds {
    pub(crate) _container_id: NodeId,
    pub(crate) beat_div_btn_ids: [NodeId; BEAT_DIV_COUNT],
    /// Feel segment (mutually exclusive): straight / dotted / triplet.
    pub(crate) straight_btn_id: NodeId,
    pub(crate) dotted_btn_id: NodeId,
    pub(crate) triplet_btn_id: NodeId,
    /// Free-period field — clicking opens the beats type-in (free mode).
    pub(crate) free_btn_id: NodeId,
    /// Output polarity invert (`reversed` -> `1 - value`).
    pub(crate) invert_btn_id: NodeId,
    pub(crate) wave_btn_ids: [NodeId; WAVEFORM_COUNT],
}

/// The orange envelope target handle on a parameter's slider track — sets the
/// depth (`target_normalized`) the envelope pulls the value toward, shown in the
/// parameter's own range.
pub(crate) struct EnvelopeTargetIds {
    pub(crate) target_bar_id: NodeId,
}

/// The envelope drawer — a single "Decay" slider (`decay_beats`).
pub(crate) struct EnvelopeConfigIds {
    pub(crate) _container_id: NodeId,
    pub(crate) decay_slider: SliderNodeIds,
    /// Right-click reset for the Decay slider (the `EnvDecay*` trio) —
    /// BUG-070 follow-through; this drawer previously had no reset gesture
    /// at all (`DrawerRow::Slider`'s `reset` field is now required).
    pub(crate) decay_reset: PanelAction,
}

#[derive(Clone, Copy)]
pub(crate) struct TrimHandleIds {
    pub(crate) fill_id: NodeId,
    pub(crate) min_bar_id: NodeId,
    pub(crate) max_bar_id: NodeId,
}

pub(crate) struct AbletonConfigIds {
    pub(crate) _container_id: NodeId,
    pub(crate) invert_btn_id: NodeId,
}

/// Display data for an Ableton-mapped parameter.
/// Constructed in state_sync, consumed by effect_card and gen_param.
#[derive(Debug, Clone, PartialEq)]
pub struct AbletonMappingDisplay {
    pub macro_name: String,
    /// Stored target track name from the mapping address. Surfaced in
    /// the UI so corrupt mappings (where the stored target doesn't match
    /// what the user intended) are visible at a glance — see the
    /// "make corruption visible" thread in feature/unit-types.
    pub track_name: String,
    /// Stored target device name (rack name in Ableton).
    pub device_name: String,
    pub status: AbletonMappingStatus,
    pub inverted: bool,
}

// ── Shared modulation state ─────────────────────────────────────

/// Per-parameter modulation state for the unified `ParamCardPanel` (both kinds).
/// Contains driver expansion, envelope expansion, trim values, the envelope
/// target (`target_norm` — the orange handle) and decay time (`env_decay` — the
/// drawer slider), and driver visual state (beat div, waveform, reversed,
/// dotted, triplet).
pub struct ParamModState {
    pub driver_expanded: Vec<bool>,
    pub envelope_expanded: Vec<bool>,
    pub trim_min: Vec<f32>,
    pub trim_max: Vec<f32>,
    pub target_norm: Vec<f32>,
    /// Envelope decay time in beats.
    pub env_decay: Vec<f32>,
    pub driver_beat_div_idx: Vec<i32>,
    pub driver_waveform_idx: Vec<i32>,
    pub driver_reversed: Vec<bool>,
    pub driver_dotted: Vec<bool>,
    pub driver_triplet: Vec<bool>,
    /// Per-param: free-running LFO period in beats when the driver is in **free
    /// mode** (`Some`), else `None` for sync mode (grid/feel). Drives the Free
    /// field's label + highlight and the type-in prefill.
    pub driver_free_period: Vec<Option<f32>>,

    // ── Audio modulation (per-param + card-level send list) ──
    /// Per-param: an audio modulation exists and is enabled (button highlight +
    /// drawer auto-expands, mirroring the driver).
    pub audio_active: Vec<bool>,
    /// Per-param: index of the selected send in [`Self::audio_send_labels`], or
    /// -1 if the mod's send no longer resolves.
    pub audio_send_idx: Vec<i32>,
    /// Per-param: selected feature `kind` index (into `AudioFeatureKind::ALL`)
    /// and `band` index (into `AudioBand::ALL`) — the two-axis feature matrix.
    pub audio_kind_idx: Vec<i32>,
    pub audio_band_idx: Vec<i32>,
    /// Per-param: audio-mod output sub-range (the green trim handles), 0..1 of the
    /// slider's travel. Mirrors `trim_min`/`trim_max` for drivers — the audio
    /// drives only this slice of the param's range.
    pub audio_range_min: Vec<f32>,
    pub audio_range_max: Vec<f32>,
    /// Per-param: audio-mod invert (`AudioModShape::invert`) — drives the "Inv"
    /// toggle in the drawer (loud → low).
    pub audio_invert: Vec<bool>,
    /// Per-param: audio-mod rate-of-change (`AudioModShape::rate_of_change`) —
    /// drives the "d/dt" toggle.
    pub audio_rate: Vec<bool>,
    /// Per-param: audio-mod shaping values, shown on the drawer sliders.
    /// Sensitivity (Amount), and attack/release in ms.
    pub audio_sensitivity: Vec<f32>,
    pub audio_attack_ms: Vec<f32>,
    pub audio_release_ms: Vec<f32>,
    /// Card-level: available send labels (same for every row on the card).
    pub audio_send_labels: Vec<String>,
    /// Card-level: send ids parallel to `audio_send_labels` — turns a selected
    /// drawer index into the id an `AudioModSetSource` command needs.
    pub audio_send_ids: Vec<manifold_foundation::AudioSendId>,

    /// Per-param: fire-mode index into `[ClipEdge, Transient, Both]` (§9 U3),
    /// read off `ParameterAudioMod.trigger_mode`. Only meaningful on an
    /// `is_trigger_gate` row's mod; harmless elsewhere (never read). Unlike
    /// the pre-§9 `audio_trigger_*` arrays this rides the SAME per-param
    /// `audio_*` state above — a trigger-gate card's config is a normal
    /// `ParameterAudioMod`, not a separate per-instance field.
    pub audio_mode_idx: Vec<i32>,

    /// Per-param: fire ACTION index into `[Continuous, Step, Random]` (D2),
    /// read off `ParameterAudioMod.action`. Drives the drawer's Action row,
    /// the collapsed "A"→"S"/"R" glyph (D8's "silent mode trap" badge), and
    /// gates whether the Amount/Wrap/Mode rows show at all.
    pub audio_action_idx: Vec<i32>,
    /// Per-param: the Step action's `amount` (signed, param units) — the
    /// drawer's Amount slider. Meaningful only while `audio_action_idx == 1`.
    pub audio_step_amount: Vec<f32>,
    /// Per-param: the Step action's wrap-mode index into
    /// `[Wrap, Bounce, Clamp]` (D2) — the drawer's Wrap row. Meaningful only
    /// while `audio_action_idx == 1`.
    pub audio_wrap_idx: Vec<i32>,

    /// Per-param: the drawer's full Feature×Band matrix is open (the "Custom"
    /// cell trailing the Listen chips). SESSION-ONLY UI state — `sync_audio`
    /// never writes it; it mirrors no model field.
    pub audio_matrix_open: Vec<bool>,

    // ── Automation lane indicator (P4 §7 last bullet) ──
    /// Per-param: an enabled automation lane with ≥1 point exists on this
    /// instance for this param (Live's red "automated" dot).
    pub automation_active: Vec<bool>,
    /// Per-param: that lane's `(EffectId, ParamId)` is currently latched in
    /// `ContentState::automation_latched_params` — the dot grays instead of
    /// showing red, mirroring the lane-strip / transport BACK button.
    pub automation_overridden: Vec<bool>,
}

/// Map a feature-row button index to its `AudioFeatureKind` (clamped).
pub(crate) fn audio_kind_from_index(idx: usize) -> crate::types::AudioFeatureKind {
    crate::types::AudioFeatureKind::ALL
        .get(idx)
        .copied()
        .unwrap_or(crate::types::AudioFeatureKind::Amplitude)
}

/// Map a band-row button index to its `AudioBand` (clamped).
pub(crate) fn audio_band_from_index(idx: usize) -> crate::types::AudioBand {
    crate::types::AudioBand::ALL
        .get(idx)
        .copied()
        .unwrap_or(crate::types::AudioBand::Full)
}

/// Feature-row button labels, in `AudioFeatureKind::ALL` order — derived from
/// `ALL` so a new kind (P4 added Pitch/Presence) can never leave the drawer
/// stale.
pub(crate) fn audio_kind_labels() -> [&'static str; AUDIO_KIND_COUNT] {
    crate::types::AudioFeatureKind::ALL.map(|k| k.label())
}

#[cfg(test)]
mod audio_row_tests {
    use super::*;

    /// P4 regression (2026-07-06, found by Peter on a live build): the UI
    /// crate holds a MIRROR of core's `AudioFeatureKind` behind the
    /// translation boundary, and P4 initially extended only core — the
    /// drawer stayed at five buttons while serde/runtime shipped. This pins
    /// the row that actually feeds pixels.
    #[test]
    fn feature_row_carries_kick_pitch_and_presence() {
        let labels = audio_kind_labels();
        assert_eq!(labels.len(), 8);
        // Kick was inserted after Transients (index 4), shifting Pitch/Presence.
        assert_eq!(labels[5], "Kick");
        assert_eq!(labels[6], "Pitch");
        assert_eq!(labels[7], "Presence");
        assert_eq!(AUDIO_KIND_COUNT, 8);
        // Order-parity with core lives in manifold-app's ui_translate tests —
        // this crate deliberately cannot see manifold-core.
    }
}

/// Band-row button labels, in `AudioBand::ALL` order.
pub(crate) fn audio_band_labels() -> [&'static str; 4] {
    [
        crate::types::AudioBand::Full.label(),
        crate::types::AudioBand::Low.label(),
        crate::types::AudioBand::Mid.label(),
        crate::types::AudioBand::High.label(),
    ]
}

/// Number of feature kinds / bands exposed in the drawer.
pub(crate) const AUDIO_KIND_COUNT: usize = crate::types::AudioFeatureKind::ALL.len();
pub(crate) const AUDIO_BAND_COUNT: usize = 4;

// ── Curated trigger-source chips (clip-trigger drawer) ─────────────
//
// A clip trigger fires on an onset, so the raw Feature×Band matrix (32 cells,
// most of them continuous-modulation features that make no sense as a fire
// source) is the wrong vocabulary for that surface. The chips below are the
// musically-named cells a performer actually reaches for. They are PURE
// PRESENTATION: each maps onto the same `AudioFeature { kind, band }` the
// matrix edits, so the model, serialization, and evaluator never know the
// difference. The realtime-analysis backing per chip:
//
//   Kick       — the dedicated descending-FM-ridge kick detector (sub-bass;
//                blind to bassline notes a Low-band flux transient can't
//                separate). The runtime ignores `band` for `Kick` (always
//                reads Low), so the chip matches on kind alone.
//   Bass       — Transients×Low: any low-band onset, bassline notes included
//                ("pulse on every bass note").
//   Snare/Hats — Transients×Mid/High: band transients, NOT instrument
//                classifiers — a mid-band onset from vocals or a synth stab
//                fires Snare too. On a separated stem send they read true.
//   Transients — Transients×Full: any hit, anywhere. The always-works generic.
//
// Classifier-open (AUDIO_EVENT_CLASSIFIER): when the neural labeler earns its
// place, its classes append here as more named cells — the drawer builds from
// this list, never from an assumption that five chips are all that exists.

/// One curated trigger-source cell: a label plus the `AudioFeature` it sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceChip {
    pub label: String,
    pub feature: crate::types::AudioFeature,
    /// Whether the trigger's current cell is this chip.
    pub active: bool,
}

/// The curated cells, in drawer order (low → high, generic last).
pub(crate) const TRIGGER_SOURCE_CHIPS: [(&str, crate::types::AudioFeatureKind, crate::types::AudioBand); 5] = [
    ("Kick", crate::types::AudioFeatureKind::Kick, crate::types::AudioBand::Low),
    ("Bass", crate::types::AudioFeatureKind::Transients, crate::types::AudioBand::Low),
    ("Snare", crate::types::AudioFeatureKind::Transients, crate::types::AudioBand::Mid),
    ("Hats", crate::types::AudioFeatureKind::Transients, crate::types::AudioBand::High),
    ("Transients", crate::types::AudioFeatureKind::Transients, crate::types::AudioBand::Full),
];

/// The chips a clip-trigger drawer shows for `current`: the curated five with
/// the active one highlighted — plus, when the current cell isn't one of the
/// five (an older project pointing at e.g. Flux×Mid, or a future classifier
/// class surfaced through the param-mod drawer's full matrix), a truthful
/// trailing chip naming the actual cell, so the drawer never silently
/// re-points a trigger at a different signal than the one it fires from.
pub(crate) fn trigger_source_chips(current: crate::types::AudioFeature) -> Vec<SourceChip> {    let mut chips: Vec<SourceChip> = TRIGGER_SOURCE_CHIPS
        .iter()
        .map(|&(label, kind, band)| {
            // `Kick` ignores `band` at evaluation time (always reads Low), so
            // a saved Kick cell matches its chip regardless of the stored band.
            let active = kind == current.kind
                && (band == current.band || kind == crate::types::AudioFeatureKind::Kick);
            SourceChip {
                label: label.to_string(),
                feature: crate::types::AudioFeature::new(kind, band),
                active,
            }
        })
        .collect();
    if !chips.iter().any(|c| c.active) {
        chips.push(SourceChip {
            label: format!("{}\u{00B7}{}", current.kind.label(), current.band.label()),
            feature: current,
            active: true,
        });
    }
    chips
}

/// Container height of a clip-trigger drawer: Source row, Listen (chips)
/// row, Sensitivity slider, Length row — plus the Sensitivity meter strip.
///
/// Paired with [`build_clip_trigger_drawer`] so a caller reserving height
/// (the AUDIO TRIGGERS section) can't drift from what's actually built.
pub(crate) fn clip_trigger_drawer_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(4) + crate::panels::drawer::METER_STRIP_H
}

/// One param row's audio-modulation display state — the per-row facts
/// [`AudioCardState::rows`] carries. Collapses the former fifteen parallel
/// per-param vecs (D3, `docs/WIDGET_TREE_DESIGN.md` P1a) into one struct per
/// row.
#[derive(Debug, Clone)]
pub struct AudioRowState {
    /// Mod exists and is enabled.
    pub active: bool,
    /// The mod's send id, if any. Resolved to an index into `send_ids` by
    /// [`ParamModState::sync_audio`].
    pub send_id: Option<manifold_foundation::AudioSendId>,
    /// Selected feature `kind` and `band` indices (the matrix axes).
    pub kind_idx: i32,
    pub band_idx: i32,
    /// The mod's output sub-range (`AudioModShape::range_min/max`).
    pub range_min: f32,
    pub range_max: f32,
    /// The mod's invert flag (`AudioModShape::invert`).
    pub invert: bool,
    /// The mod's rate-of-change flag (`AudioModShape::rate_of_change`).
    pub rate: bool,
    /// The mod's shaping values (sensitivity, attack ms, release ms).
    pub sensitivity: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// Fire-mode index (`ParameterAudioMod.trigger_mode`, §9 U3), into
    /// `[ClipEdge, Transient, Both]`. Only meaningful on an `is_trigger_gate`
    /// target; a harmless default elsewhere.
    pub trigger_mode_idx: i32,
    /// Fire ACTION index (`ParameterAudioMod.action`, D2), into
    /// `[Continuous, Step, Random]`.
    pub action_idx: i32,
    /// The Step action's `amount` (D2). Meaningful only while
    /// `action_idx == 1`.
    pub step_amount: f32,
    /// The Step action's wrap-mode index (D2), into `[Wrap, Bounce, Clamp]`.
    /// Meaningful only while `action_idx == 1`.
    pub wrap_idx: i32,
}

impl Default for AudioRowState {
    fn default() -> Self {
        Self {
            active: false,
            send_id: None,
            kind_idx: 0,
            band_idx: 0,
            range_min: 0.0,
            range_max: 1.0,
            invert: false,
            rate: false,
            sensitivity: 1.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            trigger_mode_idx: 0,
            action_idx: 0,
            step_amount: 1.0,
            wrap_idx: 0,
        }
    }
}

/// Audio-modulation display state for one card, assembled in `state_sync` and
/// applied to [`ParamModState`] via [`ParamModState::sync_audio`]. Bundled so
/// the card config gains one field, not five.
#[derive(Debug, Default, Clone)]
pub struct AudioCardState {
    /// Per-param audio-mod facts, one [`AudioRowState`] per card row (D3).
    pub rows: Vec<AudioRowState>,
    /// Card-level: available send labels.
    pub send_labels: Vec<String>,
    /// Card-level: send ids parallel to `send_labels` — what the click handler
    /// turns a selected index into for the `AudioModSetSource` command.
    pub send_ids: Vec<manifold_foundation::AudioSendId>,
}

/// Number of fire-mode choices in a trigger-gate mod's Mode row
/// (§9 U3: ClipEdge / Transient / Both).
pub(crate) const AUDIO_TRIGGER_MODE_COUNT: usize = 3;

/// Mode-row button labels, index-parallel to core's `TriggerFireMode`
/// (`ClipEdge`/`Transient`/`Both`) — the UI carries only the index (mirrors
/// `BEAT_DIV_LABELS`'s relationship to `BeatDivision`), converted at the
/// `manifold-app` dispatch boundary. §9 unified the trigger-gate drawer onto
/// the standard audio-mod drawer; this is the one extra row it appends.
pub(crate) fn audio_trigger_mode_labels() -> [&'static str; AUDIO_TRIGGER_MODE_COUNT] {
    ["Clip", "Audio", "Both"]
}

impl ParamModState {
    pub fn allocate(param_count: usize) -> Self {
        Self {
            driver_expanded: vec![false; param_count],
            envelope_expanded: vec![false; param_count],
            trim_min: vec![0.0; param_count],
            trim_max: vec![1.0; param_count],
            target_norm: vec![0.5; param_count],
            env_decay: vec![DEFAULT_ENV_DECAY; param_count],
            driver_beat_div_idx: vec![-1; param_count],
            driver_waveform_idx: vec![-1; param_count],
            driver_reversed: vec![false; param_count],
            driver_dotted: vec![false; param_count],
            driver_triplet: vec![false; param_count],
            driver_free_period: vec![None; param_count],
            audio_active: vec![false; param_count],
            audio_send_idx: vec![-1; param_count],
            audio_kind_idx: vec![0; param_count],
            audio_band_idx: vec![0; param_count],
            audio_range_min: vec![0.0; param_count],
            audio_range_max: vec![1.0; param_count],
            audio_invert: vec![false; param_count],
            audio_rate: vec![false; param_count],
            audio_sensitivity: vec![1.0; param_count],
            audio_attack_ms: vec![5.0; param_count],
            audio_release_ms: vec![120.0; param_count],
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            audio_mode_idx: vec![0; param_count],
            audio_action_idx: vec![0; param_count],
            audio_step_amount: vec![1.0; param_count],
            audio_wrap_idx: vec![0; param_count],
            audio_matrix_open: vec![false; param_count],
            automation_active: vec![false; param_count],
            automation_overridden: vec![false; param_count],
        }
    }

    /// Sync audio-modulation display state from the card config.
    pub fn sync_audio(&mut self, n: usize, audio: &AudioCardState) {
        // Session-only UI state: sized here so a card whose param list grew
        // since `allocate` never has a dead "Custom" toggle. Never overwritten
        // from the model — it's not a mirrored field.
        self.audio_matrix_open.resize(n, false);
        let default_row = AudioRowState::default();
        for i in 0..n {
            let row = audio.rows.get(i).unwrap_or(&default_row);
            self.audio_active[i] = row.active;
            self.audio_kind_idx[i] = row.kind_idx;
            self.audio_band_idx[i] = row.band_idx;
            self.audio_range_min[i] = row.range_min;
            self.audio_range_max[i] = row.range_max;
            self.audio_invert[i] = row.invert;
            self.audio_rate[i] = row.rate;
            self.audio_sensitivity[i] = row.sensitivity;
            self.audio_attack_ms[i] = row.attack_ms;
            self.audio_release_ms[i] = row.release_ms;
            self.audio_mode_idx[i] = row.trigger_mode_idx;
            self.audio_action_idx[i] = row.action_idx;
            self.audio_step_amount[i] = row.step_amount;
            self.audio_wrap_idx[i] = row.wrap_idx;
            self.audio_send_idx[i] = row
                .send_id
                .as_ref()
                .and_then(|sid| audio.send_ids.iter().position(|s| s == sid))
                .map(|p| p as i32)
                .unwrap_or(-1);
        }
        self.audio_send_labels = audio.send_labels.clone();
        self.audio_send_ids = audio.send_ids.clone();
    }

    /// Sync driver/envelope/trim/target/decay state from the config's per-row
    /// modulation facts. `n` is the param count. Reads `rows` with a
    /// fallback default for any row past its end.
    pub fn sync_from_config(&mut self, n: usize, rows: &[RowMod]) {
        let default_row = RowMod::default();
        for i in 0..n {
            let row = rows.get(i).unwrap_or(&default_row);
            self.driver_expanded[i] = row.driver_active;
            self.envelope_expanded[i] = row.envelope_active;
            self.trim_min[i] = row.trim_min;
            self.trim_max[i] = row.trim_max;
            self.target_norm[i] = row.target_norm;
            self.env_decay[i] = row.env_decay;
            self.driver_beat_div_idx[i] = row.driver_beat_div_idx;
            self.driver_waveform_idx[i] = row.driver_waveform_idx;
            self.driver_reversed[i] = row.driver_reversed;
            self.driver_dotted[i] = row.driver_dotted;
            self.driver_triplet[i] = row.driver_triplet;
            self.driver_free_period[i] = row.driver_free_period;
            self.automation_active[i] = row.automation_active;
            self.automation_overridden[i] = row.automation_overridden;
        }
    }

    /// The driver's current effective period in beats — the free period when in
    /// free mode, else the sync division's period with its feel modifier applied.
    /// Used to prefill the Free type-in so the box opens at the live value.
    pub fn driver_effective_period(&self, i: usize) -> f32 {
        if let Some(p) = self.driver_free_period.get(i).copied().flatten() {
            return p;
        }
        let idx = self.driver_beat_div_idx.get(i).copied().unwrap_or(3).max(0) as usize;
        let mut beats = BEAT_DIV_BEATS.get(idx).copied().unwrap_or(1.0);
        if self.driver_dotted.get(i).copied().unwrap_or(false) {
            beats *= 1.5;
        } else if self.driver_triplet.get(i).copied().unwrap_or(false) {
            beats *= 2.0 / 3.0;
        }
        beats
    }
}

// ── Shared drag state ───────────────────────────────────────────
// P7.1 (docs/UI_WIDGET_UNIFICATION_DESIGN.md, D8/D10): the six formerly
// parallel `Option`/sentinel slots below fold into one `DragController`
// payload enum. Single-active is enforced at the type level — a fresh grab
// always wins (drag.rs) — which only forbids states that were already bugs
// (two slots armed at once was never a feature, D8).

/// What a `ParamDragState` drag is targeting, captured at grab time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ParamDragTarget {
    /// A plain param-slider drag. Was `dragging_param: i32` (−1 idle).
    Param { index: usize },
    /// The active modulator trim-range drag: which modulator
    /// ([`TrimKind`] — driver/Ableton/audio share one path), the param
    /// index, and which edge. Was `dragging_trim`.
    Trim {
        kind: TrimKind,
        index: usize,
        is_min: bool,
    },
    /// The envelope target (orange handle / `target_normalized`) on the
    /// track. Was `dragging_target_param: i32`.
    EnvTarget { index: usize },
    /// The envelope decay slider (`decay_beats`) in the drawer. Was
    /// `dragging_decay_param: i32`.
    EnvDecay { index: usize },
    /// An audio shaping slider drag in the drawer. A trigger-gate row's
    /// Amount/Attack/Release sliders ride this SAME path (§9 unified the
    /// drawer) — no separate trigger-mod drag target. Was
    /// `dragging_audio_shape: Option<(usize, AudioShapeParam)>`.
    AudioShape {
        index: usize,
        param: crate::panels::AudioShapeParam,
    },
    /// The Step-Amount slider drag, only ever built while Action=Step
    /// (PARAM_STEP_ACTIONS D8) — `amount` lives on `TriggerAction::Step`,
    /// not `AudioModShape`, so `AudioShapeParam` doesn't apply here. Was
    /// `dragging_step_amount: Option<usize>`.
    StepAmount { index: usize },
    /// A D3 "3D Shading" relight-knob drag (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// P5b) — the six always-visible rows below the normal params, not
    /// indexed into `rows`/`ParamId` at all.
    Relight { field: crate::panels::UiRelightField },
}

/// Drag tracking state for the unified `ParamCardPanel` (both kinds). A thin
/// wrapper over [`DragController`] — the six accessors below let the ~49
/// call sites convert from the old sentinel fields one-for-one.
pub(crate) struct ParamDragState {
    drag: DragController<ParamDragTarget>,
}

impl ParamDragState {
    pub(crate) fn new() -> Self {
        Self {
            drag: DragController::new(),
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_active()
    }

    /// Begin a drag. `pos` is the real pointer position already in scope at
    /// the `handle_pointer_down` call site — never a synthesized geometry.
    pub(crate) fn begin(&mut self, target: ParamDragTarget, pos: Vec2) {
        self.drag.start(target, pos);
    }

    /// Release — hands back the target that was active, if any, as the
    /// signal to emit a commit.
    pub(crate) fn end(&mut self) -> Option<ParamDragTarget> {
        self.drag.release()
    }

    pub(crate) fn param_index(&self) -> Option<usize> {
        match self.drag.payload() {
            Some(ParamDragTarget::Param { index }) => Some(*index),
            _ => None,
        }
    }

    pub(crate) fn trim(&self) -> Option<(TrimKind, usize, bool)> {
        match self.drag.payload() {
            Some(ParamDragTarget::Trim { kind, index, is_min }) => Some((*kind, *index, *is_min)),
            _ => None,
        }
    }

    pub(crate) fn env_target_index(&self) -> Option<usize> {
        match self.drag.payload() {
            Some(ParamDragTarget::EnvTarget { index }) => Some(*index),
            _ => None,
        }
    }

    pub(crate) fn env_decay_index(&self) -> Option<usize> {
        match self.drag.payload() {
            Some(ParamDragTarget::EnvDecay { index }) => Some(*index),
            _ => None,
        }
    }

    pub(crate) fn audio_shape(&self) -> Option<(usize, crate::panels::AudioShapeParam)> {
        match self.drag.payload() {
            Some(ParamDragTarget::AudioShape { index, param }) => Some((*index, *param)),
            _ => None,
        }
    }

    pub(crate) fn step_amount(&self) -> Option<usize> {
        match self.drag.payload() {
            Some(ParamDragTarget::StepAmount { index }) => Some(*index),
            _ => None,
        }
    }

    pub(crate) fn relight_field(&self) -> Option<crate::panels::UiRelightField> {
        match self.drag.payload() {
            Some(ParamDragTarget::Relight { field }) => Some(*field),
            _ => None,
        }
    }
}

/// The ONE trim-geometry source (BUG-258): the fill + bar rects for a trim
/// range on a track. Build, reposition, and hit-zone math all derive from
/// this, so the grabbable zone can never drift from the drawn handle.
pub(crate) struct TrimBarRects {
    pub fill: Rect,
    pub min_bar: Rect,
    pub max_bar: Rect,
}

pub(crate) fn trim_bar_rects(track_rect: Rect, min: f32, max: f32) -> TrimBarRects {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    TrimBarRects {
        fill: Rect::new(
            base_x + min * usable,
            track_rect.y + OVERLAY_INSET,
            (max - min) * usable,
            track_rect.height - OVERLAY_INSET * 2.0,
        ),
        min_bar: Rect::new(base_x + min * usable - TRIM_BAR_W * 0.5, track_rect.y, TRIM_BAR_W, track_rect.height),
        max_bar: Rect::new(base_x + max * usable - TRIM_BAR_W * 0.5, track_rect.y, TRIM_BAR_W, track_rect.height),
    }
}

/// The envelope target bar's rect for a depth `norm` on a track — the
/// single geometry source for build, drag-reposition, and hit-zone math
/// (same anti-drift contract as [`trim_bar_rects`], BUG-258).
pub(crate) fn target_bar_rect(track_rect: Rect, norm: f32) -> Rect {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    Rect::new(
        base_x + norm * usable - TARGET_BAR_W * 0.5,
        track_rect.y - 2.0,
        TARGET_BAR_W,
        track_rect.height + 4.0,
    )
}

/// Reposition a trim overlay's three nodes (fill + min/max bars) along a slider
/// track for a new `[min, max]`. The pixel math is identical for driver,
/// Ableton, and audio trims — this is the single copy they all share, so a
/// layout tweak lands once instead of drifting across three near-identical
/// blocks.
pub(crate) fn reposition_trim_bars(
    tree: &mut UITree,
    track_rect: Rect,
    ids: &TrimHandleIds,
    new_min: f32,
    new_max: f32,
) {
    let r = trim_bar_rects(track_rect, new_min, new_max);
    tree.set_bounds(ids.fill_id, r.fill);
    tree.set_bounds(ids.min_bar_id, r.min_bar);
    tree.set_bounds(ids.max_bar_id, r.max_bar);
}

// ── Shared helper functions ─────────────────────────────────────

/// BUG-250: the click-to-change action set for an enum (`value_labels`)
/// row's value cell — the behavior SCENE_OBJECT_AND_PANEL_V2 D9 committed
/// to, restored in the shared card core after C-P1c/d deleted the bespoke
/// producers. A 2-label row cycles to the next value through the
/// `ParamSnapshot`/`ParamChanged`/`ParamCommit` trio (one undo unit; the
/// scene id_map interception comes free); a 3+-label row opens the shared
/// dropdown via [`PanelAction::ParamEnumDropdown`]. `current_value` is the
/// row's base value, `min` the param's range minimum (enum index = value −
/// min, same encoding as [`format_param_value`]).
pub(crate) fn enum_value_cell_actions(
    target: crate::panels::GraphParamTarget,
    param_id: manifold_foundation::ParamId,
    labels: &[String],
    current_value: f32,
    min: f32,
    cell_node_id: NodeId,
) -> Vec<crate::panels::PanelAction> {
    use crate::panels::PanelAction;
    let count = labels.len();
    if count == 0 {
        return Vec::new();
    }
    let current_index =
        ((current_value - min).round() as i32).clamp(0, count as i32 - 1) as usize;
    if count <= 2 {
        let next = (current_index + 1) % count;
        let new_value = min + next as f32;
        vec![
            PanelAction::Scrub(ValueRef::Param(target.clone(), param_id.clone()), ScrubPhase::Begin),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), param_id.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(new_value)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, param_id), ScrubPhase::Commit),
        ]
    } else {
        vec![PanelAction::Root(RootAction::ParamEnumDropdown {
            target,
            param_id,
            labels: labels.to_vec(),
            current_index: current_index as u32,
            cell_node_id,
        })]
    }
}


pub(crate) fn format_param_value(
    val: f32,
    min: f32,
    whole_numbers: bool,
    is_angle: bool,
    value_labels: Option<&[String]>,
) -> String {
    if let Some(labels) = value_labels {
        let idx = ((val - min).round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
        return labels[idx].clone();
    }
    if is_angle {
        // `val` is radians; the user always sees and edits degrees.
        format!("{:.0}°", val.to_degrees())
    } else if whole_numbers {
        format!("{}", val.round() as i32)
    } else {
        format!("{:.2}", val)
    }
}

// The three card-button helpers below are the inspector-density applications of
// the chrome component kit's state-button mechanic (`components::state_button`).
// The mechanic — active fills with the caller's hue (hover/press derived), off
// sits on a neutral chip — lives in one place; these pick the card *skin*
// (`CARD_RAISED` raised dim chip, `CARD_RECESSED` recessed dark cell) and the
// per-caller font. See `chrome::components::StateButtonSkin`.

/// Modulation-source activation buttons (envelope / driver / audio): a raised dim
/// chip, filled with the source hue when active.
pub(crate) fn de_btn_style(active: bool, active_color: Color32) -> UIStyle {
    crate::chrome::components::state_button_skinned(
        active_color,
        active,
        color::FONT_CAPTION,
        &crate::chrome::components::StateButtonSkin::CARD_RAISED,
    )
}

/// A recessed option cell filled with `active_color` when on (e.g. Ableton purple
/// for the INV button). The drawer's own option cells now resolve from
/// [`crate::chrome::Theme::option_style`]; this remains for the few callers that
/// build a one-off config button outside a themed drawer. `font_size` is the
/// caller's (effect card 8, gen param 10).
pub(crate) fn config_btn_style_colored(
    active: bool,
    active_color: Color32,
    font_size: u16,
) -> UIStyle {
    crate::chrome::components::state_button_skinned(
        active_color,
        active,
        font_size,
        &crate::chrome::components::StateButtonSkin::CARD_RECESSED,
    )
}

// The canonical toggle look now lives in the Phase-4 component kit; this
// shared helper delegates so every toggle (effect header, generator, param
// rows) tracks the same tokens. Off-state moves onto the grey ramp (BG_3)
// instead of the old BUTTON_INACTIVE grey.
pub(crate) fn toggle_btn_style(enabled: bool) -> UIStyle {
    crate::chrome::components::toggle_style(enabled)
}

/// Style for a dropdown trigger — a control cell that shows the current selection
/// and opens a `DropdownPanel` on click. The canonical neutral dropdown chip
/// (`components::dropdown_chip_style` on the grey ramp): the layer-header routing
/// chip on a hueless surface, so the detection inspector, string-param cards, clip
/// chrome, and any future picker all read identically — caret affordance, chip
/// radius, and padding included.
pub(crate) fn dropdown_trigger_style(font_size: u16) -> UIStyle {
    crate::chrome::components::dropdown_trigger_style(font_size)
}

/// A dropdown trigger as a typed Chrome [`View`] component — the declarative twin
/// of the imperative builder. A panel drops this into its description (size it +
/// `.key(K)` to resolve the click, and `.inert()` since the gesture routes through
/// the panel's `handle_click`). The caret is the style's `dropdown_caret` flag, so
/// `current` is the bare value (no baked `\u{25BC}`).
pub(crate) fn dropdown_trigger_view(current: &str, font_size: u16) -> View {
    View::button(current.to_string()).style(dropdown_trigger_style(font_size))
}

// ── Shared builder functions ────────────────────────────────────

pub(crate) fn build_driver_config(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
    btn_font_size: u16,
    key: Option<u64>,
) -> DriverConfigIds {
    use crate::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};

    let active_div = mod_state
        .driver_beat_div_idx
        .get(param_idx)
        .copied()
        .unwrap_or(-1);
    let active_wave = mod_state
        .driver_waveform_idx
        .get(param_idx)
        .copied()
        .unwrap_or(-1);
    let is_reversed = mod_state
        .driver_reversed
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let is_dotted = mod_state
        .driver_dotted
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let is_triplet = mod_state
        .driver_triplet
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let free_period = mod_state
        .driver_free_period
        .get(param_idx)
        .copied()
        .flatten();
    let is_free = free_period.is_some();
    let is_sync = !is_free;

    // Row 1 — Rate: the 11 beat-division cells then Free (an alternative rate, so
    // it sits with the divisions). Uniform width keeps the row neat. The grid
    // lights the base division only in sync mode; Free lights in free mode and
    // shows the typed period (else "Free"), opening the beats type-in.
    let free_label = match free_period {
        Some(p) => fmt_free_period(p),
        None => "Free".to_string(),
    };
    let mut row1_buttons: Vec<DrawerButton> = (0..BEAT_DIV_COUNT)
        .map(|j| DrawerButton::new(BEAT_DIV_LABELS[j], is_sync && j as i32 == active_div))
        .collect();
    row1_buttons.push(DrawerButton::new(free_label, is_free));

    // Row 2 — Feel: [Straight][Dotted][Triplet], a mutually-exclusive segment
    // (one lit) shown only in sync mode.
    let row2_buttons: Vec<DrawerButton> = vec![
        DrawerButton::new("Straight", is_sync && !is_dotted && !is_triplet),
        DrawerButton::new("Dotted", is_sync && is_dotted),
        DrawerButton::new("Triplet", is_sync && is_triplet),
    ];

    // Row 3 — Shape + polarity: 5 waveform icons then Invert. The wave glyphs are
    // atlas icons (the UIRenderer draws the SDF waveform icon); both shape and
    // Invert apply in either rate mode.
    let mut row3_buttons: Vec<DrawerButton> = (0..WAVEFORM_COUNT)
        .map(|j| {
            let icon_char = crate::icons::waveform_icon_char(j as i32);
            DrawerButton::new(icon_char.to_string(), j as i32 == active_wave)
        })
        .collect();
    row3_buttons.push(DrawerButton::new("Invert", is_reversed));

    let spec = DrawerSpec {
        rows: vec![
            DrawerRow::Buttons {
                buttons: row1_buttons,
                width: ButtonWidth::Uniform,
                label: None,
            },
            DrawerRow::Buttons { buttons: row2_buttons, width: ButtonWidth::Uniform, label: None },
            DrawerRow::Buttons { buttons: row3_buttons, width: ButtonWidth::Uniform, label: None },
        ],
        btn_font_size,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(color::DRIVER_ACTIVE_C32).tinted(),
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec, key);

    // Reconstruct typed ids from the flat button list (row order):
    //   0..11  grid · 11 free · 12 straight · 13 dotted · 14 triplet
    //   15..20 waveforms · 20 invert.
    let ids = dids.button_ids();
    let beat_div_btn_ids: [NodeId; BEAT_DIV_COUNT] = std::array::from_fn(|j| ids[j]);
    let free_btn_id = ids[BEAT_DIV_COUNT];
    let straight_btn_id = ids[BEAT_DIV_COUNT + 1];
    let dotted_btn_id = ids[BEAT_DIV_COUNT + 2];
    let triplet_btn_id = ids[BEAT_DIV_COUNT + 3];
    let wave_base = BEAT_DIV_COUNT + 4;
    let wave_btn_ids: [NodeId; WAVEFORM_COUNT] = std::array::from_fn(|j| ids[wave_base + j]);
    let invert_btn_id = ids[wave_base + WAVEFORM_COUNT];

    DriverConfigIds {
        _container_id: dids.container,
        beat_div_btn_ids,
        straight_btn_id,
        dotted_btn_id,
        triplet_btn_id,
        free_btn_id,
        invert_btn_id,
        wave_btn_ids,
    }
}

/// Format a free LFO period (beats) for the Free field label. Whole numbers show
/// without a decimal ("3"); fractional values keep two places ("1.5", "0.38").
pub(crate) fn fmt_free_period(p: f32) -> String {
    if (p.fract()).abs() < 1e-3 {
        format!("{}", p.round() as i64)
    } else {
        format!("{p:.2}")
    }
}

/// Orange envelope target handle on a parameter's slider track. Sits at the
/// `target_normalized` position across the track — the depth the envelope pulls
/// the value toward, read in the parameter's own range. Grabbable by feel via
/// the proximity catch-zone in the panel's pointer-down handler.
pub(crate) fn build_envelope_target(
    tree: &mut UITree,
    track_parent: NodeId,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> EnvelopeTargetIds {
    let norm = mod_state.target_norm.get(param_idx).copied().unwrap_or(0.5);
    let bar = target_bar_rect(track_rect, norm);

    let target_bar_id = tree.add_button(
        Some(track_parent),
        bar.x,
        bar.y,
        bar.width,
        bar.height,
        UIStyle {
            bg_color: color::ENVELOPE_ACTIVE_C32,
            hover_bg_color: color::TARGET_BAR_HOVER_C32,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    EnvelopeTargetIds { target_bar_id }
}

/// The envelope drawer: a single "Decay" slider (`decay_beats`, 0..ENV_DECAY_MAX
/// beats). The one ADSR stage kept — how fast the value falls back after a
/// trigger. Depth is the orange target handle on the track above.
pub(crate) fn build_envelope_config(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
    target: GraphParamTarget,
    pid: manifold_foundation::ParamId,
    key: Option<u64>,
) -> EnvelopeConfigIds {
    use crate::panels::drawer::{self, DrawerRow, DrawerSpec};

    let decay = mod_state
        .env_decay
        .get(param_idx)
        .copied()
        .unwrap_or(DEFAULT_ENV_DECAY);
    // BUG-070 follow-through: the envelope drawer never had a reset gesture
    // before (`DrawerRow::Slider::reset` is now required) — wired here using
    // the same EnvDecay Snapshot/Changed/Commit trio the drag path already
    // emits, reset to `DEFAULT_ENV_DECAY`.
    let reset = PanelAction::slider_reset(
        PanelAction::Modulation(ModulationAction::EnvDecaySnapshot(target.clone(), pid.clone())),
        PanelAction::Modulation(ModulationAction::EnvDecayChanged(target.clone(), pid.clone(), DEFAULT_ENV_DECAY)),
        PanelAction::Modulation(ModulationAction::EnvDecayCommit(target, pid)),
    );
    let spec = DrawerSpec {
        rows: vec![DrawerRow::Slider {
            label: "Decay".into(),
            norm: (decay / ENV_DECAY_MAX).clamp(0.0, 1.0),
            default_norm: (DEFAULT_ENV_DECAY / ENV_DECAY_MAX).clamp(0.0, 1.0),
            value_text: format!("{decay:.2}"),
            label_w: ENV_DECAY_LABEL_W,
            reset: reset.clone(),
            show_meter: false,
        }],
        btn_font_size: FONT_SIZE,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32).tinted(),
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec, key);
    let decay_slider = dids
        .sliders
        .into_iter()
        .next()
        .expect("envelope drawer has one slider row");

    EnvelopeConfigIds {
        _container_id: dids.container,
        decay_slider,
        decay_reset: reset,
    }
}

pub(crate) fn build_trim_handles(
    tree: &mut UITree,
    track_parent: NodeId,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> TrimHandleIds {
    let tmin = mod_state.trim_min.get(param_idx).copied().unwrap_or(0.0);
    let tmax = mod_state.trim_max.get(param_idx).copied().unwrap_or(1.0);
    let r = trim_bar_rects(track_rect, tmin, tmax);

    let fill_id = tree.add_panel(
        Some(track_parent),
        r.fill.x,
        r.fill.y,
        r.fill.width,
        r.fill.height,
        UIStyle {
            bg_color: color::TRIM_FILL_C32,
            ..UIStyle::default()
        },
    );

    let min_bar_id = tree.add_button(
        Some(track_parent),
        r.min_bar.x,
        r.min_bar.y,
        r.min_bar.width,
        r.min_bar.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    let max_bar_id = tree.add_button(
        Some(track_parent),
        r.max_bar.x,
        r.max_bar.y,
        r.max_bar.width,
        r.max_bar.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    TrimHandleIds {
        fill_id,
        min_bar_id,
        max_bar_id,
    }
}

/// Build trim handles from explicit min/max values (used by Ableton mappings).
/// Same visual as driver trim handles but with configurable colors.
pub(crate) fn build_trim_handles_explicit(
    tree: &mut UITree,
    track_parent: NodeId,
    track_rect: Rect,
    min: f32,
    max: f32,
    bar_color: Color32,
    bar_hover: Color32,
    fill_color: Color32,
) -> TrimHandleIds {
    let r = trim_bar_rects(track_rect, min, max);

    let fill_id = tree.add_panel(
        Some(track_parent),
        r.fill.x,
        r.fill.y,
        r.fill.width,
        r.fill.height,
        UIStyle {
            bg_color: fill_color,
            ..UIStyle::default()
        },
    );

    let min_bar_id = tree.add_button(
        Some(track_parent),
        r.min_bar.x,
        r.min_bar.y,
        r.min_bar.width,
        r.min_bar.height,
        UIStyle {
            bg_color: bar_color,
            hover_bg_color: bar_hover,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    let max_bar_id = tree.add_button(
        Some(track_parent),
        r.max_bar.x,
        r.max_bar.y,
        r.max_bar.width,
        r.max_bar.height,
        UIStyle {
            bg_color: bar_color,
            hover_bg_color: bar_hover,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    TrimHandleIds {
        fill_id,
        min_bar_id,
        max_bar_id,
    }
}

// ── Shared event helpers ────────────────────────────────────────

impl DriverConfigIds {
    /// Resolve a clicked node against THIS drawer's own buttons (the
    /// widget-contract split, D5 `docs/WIDGET_TREE_DESIGN.md` — the bundle
    /// knows its own nodes; `row_action` supplies the row via `RowIndex`).
    /// The Free field is *not* here — it opens a type-in (handled via
    /// [`driver_free_field_index`] on the tree-aware click path), not a
    /// config command.
    pub(crate) fn resolve(&self, node_id: NodeId) -> Option<DriverConfigAction> {
        for (j, &bid) in self.beat_div_btn_ids.iter().enumerate() {
            if node_id == bid {
                return Some(DriverConfigAction::BeatDiv(j));
            }
        }
        if node_id == self.straight_btn_id {
            return Some(DriverConfigAction::Straight);
        }
        if node_id == self.dotted_btn_id {
            return Some(DriverConfigAction::Dotted);
        }
        if node_id == self.triplet_btn_id {
            return Some(DriverConfigAction::Triplet);
        }
        if node_id == self.invert_btn_id {
            return Some(DriverConfigAction::Invert);
        }
        for (j, &wid) in self.wave_btn_ids.iter().enumerate() {
            if node_id == wid {
                return Some(DriverConfigAction::Wave(j));
            }
        }
        None
    }
}

/// If `node_id` is a driver drawer's Free-period field, return its param index.
/// The Free field opens a beats type-in (free mode) rather than issuing a config
/// command, so it's matched separately from [`DriverConfigIds::resolve`].
pub(crate) fn driver_free_field_index(
    node_id: NodeId,
    driver_config_ids: &[Option<DriverConfigIds>],
) -> Option<usize> {
    driver_config_ids.iter().enumerate().find_map(|(pi, cfg)| {
        cfg.as_ref()
            .filter(|c| c.free_btn_id == node_id)
            .map(|_| pi)
    })
}

// ── Ableton config drawer ───────────────────────────────────────

pub(crate) fn build_ableton_config(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    display: &AbletonMappingDisplay,
    key: Option<u64>,
) -> AbletonConfigIds {
    use crate::panels::drawer::{self, DrawerRow, DrawerSpec, StatusDot, StatusStrip, TrailingButton};

    let dot_color = match display.status {
        AbletonMappingStatus::Active => color::STATUS_DOT_GREEN,
        AbletonMappingStatus::Dormant => color::STATUS_DOT_YELLOW,
        AbletonMappingStatus::Ambiguous => color::STATUS_BAD,
    };

    // Compose the label as "macro_name  ·  track > device" so the user can see
    // the actual stored target rack at a glance. This makes corrupted mappings
    // (where the stored target doesn't match what was originally mapped)
    // immediately visible without changing any routing — the values still flow
    // wherever the resolver landed, but the user can audit it from the card.
    let composite_label = if display.track_name.is_empty() && display.device_name.is_empty() {
        display.macro_name.clone()
    } else {
        format!(
            "{}  ·  {} > {}",
            display.macro_name, display.track_name, display.device_name
        )
    };

    // The strip's row height is the container height minus the drawer's top/
    // bottom pad; the API centers each element within the row, reproducing the
    // original metrics (6px dot, 28×16 INV, 12px label) exactly.
    let spec = DrawerSpec {
        rows: vec![DrawerRow::Status(StatusStrip {
            height: ABL_CONFIG_HEIGHT - drawer::TOP_PAD * 2.0,
            dot: Some(StatusDot { size: 6.0, color: dot_color }),
            label: composite_label,
            label_color: color::TEXT_DIMMED_C32,
            label_font: color::FONT_CAPTION,
            trailing: Some(TrailingButton {
                label: "INV".into(),
                width: 28.0,
                height: 16.0,
                style: config_btn_style_colored(
                    display.inverted,
                    color::ABL_BADGE_C32,
                    color::FONT_CAPTION,
                ),
            }),
        })],
        btn_font_size: color::FONT_CAPTION,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(color::ABL_BADGE_C32).tinted(),
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec, key);
    let invert_btn_id = dids.button_ids()[0];

    AbletonConfigIds {
        _container_id: dids.container,
        invert_btn_id,
    }
}

impl AbletonConfigIds {
    /// Resolve a clicked node against THIS drawer's own Invert button (the
    /// widget-contract split, D5) — the `ParamCardPanel` row-model twin of
    /// [`check_ableton_config_click`] below, which stays for `macros_panel`
    /// (a different, non-row-model panel; not this design's scope).
    pub(crate) fn resolve(&self, node_id: NodeId) -> bool {
        node_id == self.invert_btn_id
    }
}

/// Check if a click hit an Ableton config button. Returns param index if matched.
/// Used by `macros_panel` (its own bespoke, non-`RowIndex` dispatch) — kept
/// array-scanning; `ParamCardPanel` uses [`AbletonConfigIds::resolve`] instead.
pub(crate) fn check_ableton_config_click(
    node_id: NodeId,
    ableton_config_ids: &[Option<AbletonConfigIds>],
) -> Option<(usize, AbletonConfigClick)> {
    for (pi, ids) in ableton_config_ids.iter().enumerate() {
        if let Some(c) = ids
            && node_id == c.invert_btn_id
        {
            return Some((pi, AbletonConfigClick::Invert));
        }
    }
    None
}

pub(crate) enum AbletonConfigClick {
    Invert,
}

// ── Shared per-parameter slider row ─────────────────────────────────

/// Node IDs produced by [`build_param_row`] for one parameter row. The caller
/// stores each into its parallel per-param vectors at the row's index.
pub(crate) struct ParamRowIds {
    /// Transparent, interactive full-row hit catcher sitting *behind* the
    /// slider widgets (added first, so the track/label win on top). Carries the
    /// param's right-click menu intent so a right-click on the value cell, the
    /// gaps, or anywhere on the row that isn't the track folds to the param
    /// menu — instead of each narrow widget being its own lottery target.
    /// See `docs/NODE_INTENT_DISPATCH.md`.
    pub(crate) row_catcher: NodeId,
    pub(crate) slider: Option<SliderNodeIds>,
    /// The main slider's right-click reset action — always constructed
    /// alongside `slider` (both are `Some`/real together; `slider_reset` is
    /// never `Option` because `build_param_row` always builds a main slider).
    /// The caller stores it beside `slider` for a later replay pass.
    pub(crate) slider_reset: PanelAction,
    pub(crate) trim: Option<TrimHandleIds>,
    /// Orange envelope target handle on the slider track (when armed).
    pub(crate) target: Option<EnvelopeTargetIds>,
    pub(crate) ableton_trim: Option<TrimHandleIds>,
    /// Green audio-mod trim handles on the slider track (when an audio mod is
    /// armed) — the output sub-range the audio drives.
    pub(crate) audio_trim: Option<TrimHandleIds>,
    /// The "E" envelope toggle button. `None` when the row didn't build it
    /// (effects gate it on `supports_envelopes`).
    pub(crate) envelope_btn: Option<NodeId>,
    pub(crate) driver_btn: NodeId,
    /// The "A" audio-modulation button (right of the driver button).
    pub(crate) audio_btn: NodeId,
    /// Envelope drawer (the single "Decay" slider).
    pub(crate) envelope_config: Option<EnvelopeConfigIds>,
    pub(crate) driver_config: Option<DriverConfigIds>,
    pub(crate) ableton_config: Option<AbletonConfigIds>,
    /// Audio-modulation drawer (send + feature selectors) and its send count,
    /// kept so click resolution can split the flat button index into
    /// send / new-send / feature regions.
    pub(crate) audio_config: Option<(crate::panels::drawer::DrawerIds, usize)>,
    /// Modulation-config tab strip node ids (paired with their `ModTab`). Empty
    /// when fewer than two configs are active (no strip drawn). The caller stores
    /// these to route tab clicks to the active-tab switch.
    pub(crate) mod_tabs: Vec<(NodeId, ModTab)>,
    /// `y` after this row's slider + its modulation config drawer — the caller
    /// continues the next row from here.
    pub(crate) new_cy: f32,
}

/// The modulation configs active on param `i`, in tab display order (E, →, A,
/// ABL). Drives both the build and the height calc, so they can't drift.
pub(crate) fn active_mod_tabs(mod_state: &ParamModState, info: &ParamRow, i: usize) -> Vec<ModTab> {
    let mut v = Vec::new();
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        v.push(ModTab::Envelope);
    }
    if mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
        v.push(ModTab::Driver);
    }
    if mod_state.audio_active.get(i).copied().unwrap_or(false) {
        v.push(ModTab::Audio);
    }
    if info.mapping.ableton_display.is_some() {
        v.push(ModTab::Ableton);
    }
    // §9: a trigger-gate row's config is a normal `ParameterAudioMod`, so
    // `audio_active` above already covers it — no separate tab. The row is
    // still built directly by `build_toggle_trigger_row` (bypassing the tab
    // strip, same as `is_trigger`'s `Audio` tab), but height computation now
    // shares the identical `ModTab::Audio` path every other Audio config uses.
    v
}

/// Which config is shown in the drawer: the stored choice if it's still active,
/// otherwise the first active one. `None` when nothing is active.
pub(crate) fn resolve_active_tab(active: &[ModTab], stored: ModTab) -> Option<ModTab> {
    if active.contains(&stored) {
        Some(stored)
    } else {
        active.first().copied()
    }
}

/// Height a single config tab's drawer contributes (excludes the tab strip).
/// `info`/`mod_state`/`i` feed `audio_config_height` when `tab` is `Audio` —
/// the only tab whose height varies by more than which tab it is (an
/// `is_trigger_gate` row's Mode row, D8's Action/Amount/Wrap rows on a
/// slider row armed to Step/Random).
pub(crate) fn mod_config_height(
    tab: ModTab,
    info: &ParamRow,
    mod_state: &ParamModState,
    i: usize,
) -> f32 {
    match tab {
        ModTab::Envelope => ENV_CONFIG_HEIGHT,
        ModTab::Driver => driver_config_height(),
        ModTab::Audio => audio_config_height(info, mod_state, i),
        ModTab::Ableton => ABL_CONFIG_HEIGHT,
    }
}

fn mod_tab_label(tab: ModTab) -> &'static str {
    match tab {
        ModTab::Envelope => "Trigger",
        ModTab::Driver => "LFO",
        ModTab::Audio => "Audio",
        ModTab::Ableton => "Ableton",
    }
}

/// The source-identity colour for a modulation tab — the single mapping the mod
/// card's tint and the drawer's control accent both derive from, so a tab and its
/// card always read as the same source (Trigger orange / LFO teal / Audio green /
/// Ableton purple).
pub(crate) fn mod_tab_accent(tab: ModTab) -> Color32 {
    match tab {
        ModTab::Envelope => color::ENVELOPE_ACTIVE_C32,
        ModTab::Driver => color::DRIVER_ACTIVE_C32,
        ModTab::Audio => AUDIO_MOD_ACTIVE_C32,
        ModTab::Ableton => color::ABL_BADGE_C32,
    }
}

/// Tab strip selecting which active config the drawer shows. Drawn only when ≥2
/// configs are active. Returns the tab node ids paired with their `ModTab` for
/// click routing.
fn build_mod_tab_strip(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    w: f32,
    active: &[ModTab],
    shown: Option<ModTab>,
) -> Vec<(NodeId, ModTab)> {
    let n = active.len().max(1);
    let gap = DE_BUTTON_GAP;
    let tab_w = ((w - gap * (n as f32 - 1.0)) / n as f32).floor().max(1.0);
    let mut out = Vec::with_capacity(active.len());
    let mut tx = x;
    for &tab in active {
        let id = tree.add_button(
            parent,
            tx,
            cy,
            tab_w,
            MOD_TAB_H,
            crate::chrome::components::segment_style(shown == Some(tab)),
            mod_tab_label(tab),
        );
        out.push((id, tab));
        tx += tab_w + gap;
    }
    out
}

/// Build one parameter's slider row plus its modulation config drawer (one
/// active config shown directly, or several behind a tab strip), returning the
/// created node IDs and the post-row `y`.
///
/// This is the per-parameter core shared verbatim by the effect and generator
/// kinds of `ParamCardPanel` — the bulk of what used to be duplicated between
/// the two cards' build paths. The two kinds
/// differ only in the parameters threaded in here: `parent` (the effect card
/// nests rows under its inner-bg panel, the generator card parents flat to
/// `-1`), `slider_colors` (`default_slider` vs `gen_param`), `config_font`
/// (the driver-config button font), and `build_env_button` (effects gate the
/// `E` button on `supports_envelopes`; generators always show it).
///
/// `x` is the row's left edge (already padded); `slider_w` the slider width
/// (track + label, D/E buttons reserved to its right). The drawers inset to the
/// slider TRACK span (`slider.track_rect`), so they read as an operation over
/// that slider. Node creation order is identical to the prior inline code, so
/// first-node/node-count bookkeeping is preserved.
#[allow(clippy::too_many_arguments)]
// Per-row interactive-control roles, OR'd into a row's key base (D4,
// `docs/WIDGET_TREE_DESIGN.md`) to give each of a row's flat-parented
// controls a stable, reorder-proof WidgetId. Pre-shifted left 4 bits so the
// low nibble is free for a role's own sub-tags (only `ROW_ROLE_SLIDER` uses
// it today — see `slider::SLIDER_KEY_*`); `row_key_base()` shifts the row's
// identity hash left 8, leaving this whole byte for role + sub-tag. Values
// only need to be unique within one row — `row_key_base()` separates rows.
pub(crate) const ROW_ROLE_ENV: u64 = 1 << 4;
pub(crate) const ROW_ROLE_DRV: u64 = 2 << 4;
pub(crate) const ROW_ROLE_AUDIO: u64 = 3 << 4;
pub(crate) const ROW_ROLE_CHEVRON: u64 = 4 << 4;
pub(crate) const ROW_ROLE_TOGGLE: u64 = 5 << 4;
/// D5 card-section header row (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2) —
/// keyed by the run's first row's identity, same scheme as the per-param
/// roles above.
pub(crate) const ROW_ROLE_SECTION_HEADER: u64 = 6 << 4;
pub(crate) const ROW_ROLE_ROW_CATCHER: u64 = 7 << 4;
/// The main param slider (`SliderNodeIds`'s three flat-parented nodes —
/// label/track/value-cell; fill/thumb nest under the track and need no tag
/// of their own). `slider::SLIDER_KEY_*` picks the low nibble.
pub(crate) const ROW_ROLE_SLIDER: u64 = 8 << 4;
pub(crate) const ROW_ROLE_DRIVER_CONFIG: u64 = 9 << 4;
pub(crate) const ROW_ROLE_ENVELOPE_CONFIG: u64 = 10 << 4;
pub(crate) const ROW_ROLE_AUDIO_CONFIG: u64 = 11 << 4;
pub(crate) const ROW_ROLE_ABLETON_CONFIG: u64 = 12 << 4;
/// The reveal-height `ClipRegion` a drawer builds under while its open/close
/// tween is in flight (`build_param_row`/`build_toggle_trigger_row`) — the
/// drawer container mints under THIS node, so it must be stable too or the
/// container's own explicit key composes onto a moving parent.
pub(crate) const ROW_ROLE_DRAWER_CLIP: u64 = 13 << 4;
pub(crate) const ROW_ROLE_TOGGLE_LABEL: u64 = 14 << 4;

/// A row's identity-derived key base (D4): every interactive node the row
/// builds flat-parented (siblings of every other row's controls, under the
/// card's shared inner-bg panel) derives its explicit `WidgetId` key from
/// this OR'd with a role tag above — never from sibling position, so arming a
/// modulator on an earlier row (which inserts drawer nodes ahead of it) can't
/// renumber a later row's controls, and a row's own identity survives card
/// reorder / section fold / insertion. Nodes nested under an already-keyed
/// node (fill/thumb under the slider track; a drawer's own buttons/sliders
/// under its keyed container) inherit stability through the parent chain and
/// need no key of their own (`docs/WIDGET_TREE_DESIGN.md` D4/P2).
pub(crate) fn param_row_key_base(id: &str) -> u64 {
    crate::param_surface::stable_key(id) << 8
}

/// Add a row arm button: explicitly keyed (`base | role`) when a row key base is
/// supplied, else auto-salted by sibling index.
#[allow(clippy::too_many_arguments)]
fn add_row_button(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    style: UIStyle,
    text: &str,
    row_key_base: Option<u64>,
    role: u64,
) -> NodeId {
    match row_key_base {
        Some(base) => tree.add_button_keyed(parent, x, y, w, h, style, text, base | role),
        None => tree.add_button(parent, x, y, w, h, style, text),
    }
}

/// Add a row label (non-interactive by default, matching [`UITree::add_label`]):
/// explicitly keyed when a row key base is supplied, else auto-salted.
#[allow(clippy::too_many_arguments)]
fn add_row_label(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    text: &str,
    style: UIStyle,
    row_key_base: Option<u64>,
    role: u64,
) -> NodeId {
    match row_key_base {
        Some(base) => tree.add_node_keyed(
            parent,
            Rect::new(x, y, w, h),
            UINodeType::Label,
            style,
            Some(text),
            UIFlags::empty(),
            base | role,
        ),
        None => tree.add_label(parent, x, y, w, h, text, style),
    }
}

// A toggle/trigger row stands in for a value, so its button is the same
// width as the slider value box and right-aligns to the same column — the
// right edge of every row lines up. Shared by both card kinds
// (`build_toggle_trigger_row`).
pub(crate) const TOGGLE_BTN_W: f32 = crate::slider::VALUE_BOX_W;
pub(crate) const TOGGLE_BTN_H: f32 = 16.0;
/// Width of the collapsed-row mode-indicator slot on an `is_trigger_gate` row
/// (D6 consequence: a non-default fire mode must stay visible even when the
/// drawer is closed). Reserved unconditionally on every such row so the
/// badge appearing/disappearing on a mode change never shifts other columns.
pub(crate) const TRIGGER_GATE_BADGE_W: f32 = 40.0;

/// Toggle/trigger row node IDs (button + its label). Shared by both card
/// kinds.
pub(crate) struct ToggleParamIds {
    pub(crate) label_id: Option<NodeId>,
    pub(crate) button_id: NodeId,
}

/// Format a one-shot length (beats) compactly for a drawer's Length row.
/// Common musical divisions read as fractions; whole beats get a "b" suffix.
/// Moved from `audio_setup_panel.rs` (the deleted Triggers matrix's stepper
/// label) so `build_audio_mod_drawer`'s new Length row (P3, D4/D5) can reuse
/// the exact "1b"-style formatting instead of re-deriving it.
pub(crate) fn format_beats(b: f32) -> String {
    let near = |v: f32| (b - v).abs() < 0.01;
    if near(0.25) {
        "1/4".to_string()
    } else if near(0.5) {
        "1/2".to_string()
    } else if b.fract().abs() < 0.01 {
        format!("{}b", b.round() as i32)
    } else {
        format!("{b:.2}")
    }
}

/// Build the per-param audio-modulation config drawer (Source/Feature/Band/
/// Invert toggle + Sensitivity/Attack/Release shaping sliders, plus the
/// conditional Action/Amount/Wrap/Mode rows). Shared by `build_param_row`'s
/// Audio mod-tab branch (continuous params, behind the multi-tab drawer) and
/// `build_toggle_trigger_row`'s `is_trigger`/`is_trigger_gate` cases (D5b/§9 —
/// a fire-button OR a trigger-gate toggle reaches the SAME drawer, audio-only,
/// no tab strip since Driver/Envelope/Ableton never apply to either). The
/// layer clip-trigger surface uses its own [`build_clip_trigger_drawer`]
/// instead — a fire-edge config has no use for this drawer's envelope shaping
/// or the raw feature matrix. Returns the built `DrawerIds` plus the send
/// count (the caller needs it to split the drawer's flat button index into
/// send vs. feature/band/mode regions — see `resolve_audio_config_click`).
///
/// PARAM_STEP_ACTIONS D8: a non-toggle, non-trigger `info` (a plain slider
/// row) additionally gets the Action row (Cont/Step/Rand); while armed to
/// Step it also gets the Amount slider + Wrap row. The trailing Mode row
/// (Clip/Audio/Both, §9 U2) appends for an `is_trigger_gate` target
/// unconditionally, or for a slider row armed to Step/Random (D3) — computed
/// here from `mod_state`/`info` rather than threaded in by the caller, so
/// both call sites (`build_toggle_trigger_row`, `build_param_row`) just pass
/// `info` and let this function derive which extra rows apply.
///
/// This function only builds visuals plus the shaping sliders' right-click
/// reset actions. Everything else a click on this drawer can do —
/// Source/Feature/Band selection, Invert, the drag itself — is resolved by
/// the CALLER: `ParamCardPanel` owns its own click/drag dispatch
/// (`row_action`, `handle_pointer_down`/`handle_drag`), keyed on
/// `(GraphParamTarget, ParamId)`.
/// The send-picker row's buttons, with the selected send highlighted and each
/// label tinted its send identity color (text-only, so the selected send shows
/// the standard highlight instead of a block of saturated color). Shared by
/// the param-mod drawer and the clip-trigger drawer.
fn audio_send_buttons(
    mod_state: &ParamModState,
    i: usize,
) -> Vec<crate::panels::drawer::DrawerButton> {
    use crate::panels::drawer::DrawerButton;
    let send_sel = mod_state.audio_send_idx.get(i).copied().unwrap_or(-1);
    mod_state
        .audio_send_labels
        .iter()
        .enumerate()
        .map(|(k, label)| {
            let btn = DrawerButton::new(label.clone(), k as i32 == send_sel);
            match mod_state.audio_send_ids.get(k) {
                Some(id) => btn.with_accent_text_only(crate::panels::audio_send_color(id)),
                None => btn,
            }
        })
        .collect()
}

/// A shaping slider's right-click reset action for a param-card audio mod.
fn param_shape_reset(
    gpt: GraphParamTarget,
    pid: manifold_foundation::ParamId,
    which: AudioShapeParam,
    default: f32,
) -> PanelAction {
    PanelAction::slider_reset(
        PanelAction::Modulation(ModulationAction::AudioModShapeSnapshot(gpt.clone(), pid.clone())),
        PanelAction::Modulation(ModulationAction::AudioModShapeParamChanged(gpt.clone(), pid.clone(), which, default)),
        PanelAction::Modulation(ModulationAction::AudioModShapeCommit(gpt, pid)),
    )
}

/// A shaping slider's right-click reset action for a layer clip trigger
/// (addressed by `LayerId` + row index — no `GraphParamTarget`/`ParamId`).
fn clip_trigger_shape_reset(
    layer_id: &LayerId,
    row: usize,
    which: AudioShapeParam,
    default: f32,
) -> PanelAction {
    PanelAction::slider_reset(
        PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeSnapshot(layer_id.clone(), row)),
        PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeParamChanged(layer_id.clone(), row, which, default)),
        PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeCommit(layer_id.clone(), row)),
    )
}

/// The Length row (`one_shot_beats`, "1b"-style buttons) — clip triggers only.
fn length_row(beats: f32) -> crate::panels::drawer::DrawerRow {
    use crate::panels::drawer::{ButtonWidth, DrawerButton, DrawerRow};
    let length_sel = length_option_index(beats);
    DrawerRow::Buttons {
        buttons: length_labels()
            .into_iter()
            .enumerate()
            .map(|(k, l)| DrawerButton::new(l, k == length_sel))
            .collect(),
        width: ButtonWidth::Uniform,
        label: Some("Length".into()),
    }
}

/// The clip-trigger drawer (AUDIO TRIGGERS section, one per layer row):
/// Source (send picker) → Listen (curated trigger-source chips, see
/// [`TRIGGER_SOURCE_CHIPS`]) → Sensitivity slider with the live fire meter →
/// Length. Deliberately NOT [`build_audio_mod_drawer`] with rows hidden: a
/// clip trigger fires on the raw sensitivity-scaled signal against a fixed
/// edge, so Attack/Release/Invert (which only shape the continuous
/// envelope) would be knobs that do nothing, and the Feature×Band matrix is
/// the wrong vocabulary for an onset — both are replaced by the chips.
///
/// Flat button order (what the section's click resolver walks): send buttons,
/// then the chips [`trigger_source_chips`] returned for the current cell
/// (five, or six when a truthful fallback chip is appended), then the Length
/// options. `DrawerIds.sliders[0]` is Sensitivity; `DrawerIds.meters[0]` its
/// fire meter. Returns the ids plus the send count, same contract as
/// [`build_audio_mod_drawer`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_clip_trigger_drawer(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    w: f32,
    mod_state: &ParamModState,
    i: usize,
    config_font: u16,
    layer_id: &LayerId,
    row: usize,
    length_beats: f32,
) -> (crate::panels::drawer::DrawerIds, usize) {
    use crate::panels::drawer::{ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};
    let send_count = mod_state.audio_send_labels.len();
    let current = crate::types::AudioFeature::new(
        audio_kind_from_index(mod_state.audio_kind_idx.get(i).copied().unwrap_or(0) as usize),
        audio_band_from_index(mod_state.audio_band_idx.get(i).copied().unwrap_or(0) as usize),
    );
    let chip_buttons: Vec<DrawerButton> = trigger_source_chips(current)
        .into_iter()
        .map(|c| DrawerButton::new(c.label, c.active))
        .collect();
    let sens = mod_state.audio_sensitivity.get(i).copied().unwrap_or(AUDIO_SENS_DEFAULT);
    let rows = vec![
        DrawerRow::Buttons {
            buttons: audio_send_buttons(mod_state, i),
            width: ButtonWidth::Proportional,
            label: Some("Source".into()),
        },
        DrawerRow::Buttons {
            buttons: chip_buttons,
            width: ButtonWidth::Proportional,
            label: Some("Listen".into()),
        },
        DrawerRow::Slider {
            label: "Sensitivity".to_string(),
            norm: (sens / AUDIO_SENS_MAX).clamp(0.0, 1.0),
            default_norm: (AUDIO_SENS_DEFAULT / AUDIO_SENS_MAX).clamp(0.0, 1.0),
            value_text: format!("{sens:.2}"),
            label_w: AUDIO_SHAPE_LABEL_W,
            reset: clip_trigger_shape_reset(layer_id, row, AudioShapeParam::Sensitivity, AUDIO_SENS_DEFAULT),
            show_meter: true,
        },
        length_row(length_beats),
    ];
    let spec = DrawerSpec {
        rows,
        btn_font_size: config_font,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(AUDIO_MOD_ACTIVE_C32).tinted(),
    };
    // Out of the widget-tree row model (`LayerId`-addressed clip triggers,
    // not `ParamRow`s) — unkeyed, unchanged.
    let dids = crate::panels::drawer::build(tree, parent, x, cy, w, &spec, None);
    (dids, send_count)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_audio_mod_drawer(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    w: f32,
    mod_state: &ParamModState,
    i: usize,
    config_font: u16,
    info: &ParamRow,
    gpt: GraphParamTarget,
    key: Option<u64>,
) -> (crate::panels::drawer::DrawerIds, usize) {
    use crate::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};
    let pid = info.id.clone();
    let send_count = mod_state.audio_send_labels.len();
    let kind_sel = mod_state.audio_kind_idx.get(i).copied().unwrap_or(0);
    let band_sel = mod_state.audio_band_idx.get(i).copied().unwrap_or(0);
    let invert_on = mod_state.audio_invert.get(i).copied().unwrap_or(false);
    // The Listen row: the curated chips (same `trigger_source_chips` the
    // clip-trigger drawer uses — pure presentation over the same
    // `AudioFeature { kind, band }` cells) plus a trailing "Custom" cell that
    // opens the full Feature×Band matrix behind it. The open state is
    // session-only UI (`ParamModState::audio_matrix_open`), never synced.
    let current = crate::types::AudioFeature::new(
        audio_kind_from_index(kind_sel as usize),
        audio_band_from_index(band_sel as usize),
    );
    let matrix_open = mod_state.audio_matrix_open.get(i).copied().unwrap_or(false);
    let mut chip_buttons: Vec<DrawerButton> = trigger_source_chips(current)
        .into_iter()
        .map(|c| DrawerButton::new(c.label, c.active))
        .collect();
    chip_buttons.push(DrawerButton::new("Custom", matrix_open));
    // An `is_trigger_gate` target fires on the raw sensitivity-scaled edge
    // (BUG-242): Invert/Attack/Release never reach the Schmitt trigger, so
    // the drawer doesn't offer them there. Continuous, `is_trigger`, and
    // Step/Random mods all read the shaped envelope and keep them.
    let shaping_offered = !info.spec.is_trigger_gate;
    // The Feature/Band matrix rows, only while "Custom" is open.
    let kind_buttons: Vec<DrawerButton> = audio_kind_labels()
        .iter()
        .enumerate()
        .map(|(k, l)| DrawerButton::new(*l, k as i32 == kind_sel))
        .collect();
    let band_buttons: Vec<DrawerButton> = audio_band_labels()
        .iter()
        .enumerate()
        .map(|(b, l)| DrawerButton::new(*l, b as i32 == band_sel))
        .collect();
    // Shaping sliders: Amount (sensitivity), Attack, Release. These become
    // `DrawerIds.sliders[0..3]` in row order — what the drag path hit-tests.
    let sens = mod_state.audio_sensitivity.get(i).copied().unwrap_or(1.0);
    let attack = mod_state.audio_attack_ms.get(i).copied().unwrap_or(5.0);
    let release = mod_state.audio_release_ms.get(i).copied().unwrap_or(120.0);
    // D6 (P3c, BUG-082's fix; widened 2026-07-11): the Amount slider on EVERY
    // audio-mod drawer gets the live shaped-signal meter beside it. Used to
    // gate on `is_trigger_gate`/`ClipTrigger` only (U2/D6 scoped it to the
    // configs that fire from a hidden Schmitt trigger a performer couldn't
    // otherwise see) — that left every continuous/Step/Random drawer with no
    // meter at all, even though the content thread now captures a level for
    // every enabled mod regardless of mode. Kept as a named binding (not
    // inlined `true`) so a future re-scoping has one line to change, and so
    // the call site below reads the same either way.
    let show_amount_meter = true;
    let shape_slider = |label: &str,
                         norm: f32,
                         default_norm: f32,
                         value_text: String,
                         reset: PanelAction,
                         show_meter: bool| DrawerRow::Slider {
        label: label.to_string(),
        norm: norm.clamp(0.0, 1.0),
        default_norm: default_norm.clamp(0.0, 1.0),
        value_text,
        label_w: AUDIO_SHAPE_LABEL_W,
        reset,
        show_meter,
    };
    // Each shaping slider's right-click reset — AudioModShape's own default.
    // BUG-070: these never had a reset gesture before this (the drawer only
    // opens when armed, gated the same way the drag hit-test already is).
    let shape_reset = |which: AudioShapeParam, default: f32| {
        param_shape_reset(gpt.clone(), pid.clone(), which, default)
    };
    // Modifier toggle below the band row: "Invert" (loud → low). Flat index
    // sits one past the bands. Delta (rate-of-change) removed from the UI
    // (§7.2 item 2, 2026-07-11: "not very useful and adds a lot of clutter")
    // — the runtime `AudioModShape::rate_of_change` field and its
    // `condition()` arm stay compiled for a possible future re-wire; only
    // this button, and the click routing that read it, are gone.
    let toggle_buttons = vec![DrawerButton::new("Invert", invert_on)];
    let mut rows = vec![
        DrawerRow::Buttons {
            buttons: audio_send_buttons(mod_state, i),
            width: ButtonWidth::Proportional,
            label: Some("Source".into()),
        },
        DrawerRow::Buttons {
            buttons: chip_buttons,
            width: ButtonWidth::Proportional,
            label: Some("Listen".into()),
        },
    ];
    if matrix_open {
        rows.push(DrawerRow::Buttons {
            buttons: kind_buttons,
            width: ButtonWidth::Uniform,
            label: Some("Feature".into()),
        });
        rows.push(DrawerRow::Buttons {
            buttons: band_buttons,
            width: ButtonWidth::Uniform,
            label: Some("Band".into()),
        });
    }
    if shaping_offered {
        rows.push(DrawerRow::Buttons { buttons: toggle_buttons, width: ButtonWidth::Proportional, label: None });
    }
    rows.push(shape_slider(
            // §7.2 item 3, 2026-07-11: display label only — "Amount" reads as
            // a generic gain knob; "Sensitivity" says what it tunes (how
            // easily this config fires/drives against the fixed 0.5 edge).
            // `AudioShapeParam::Sensitivity` was already the internal name.
            "Sensitivity",
            sens / AUDIO_SENS_MAX,
            AUDIO_SENS_DEFAULT / AUDIO_SENS_MAX,
            format!("{sens:.2}"),
            shape_reset(AudioShapeParam::Sensitivity, AUDIO_SENS_DEFAULT),
            show_amount_meter,
        ));
    if shaping_offered {
        rows.push(shape_slider(
            "Attack",
            attack / AUDIO_ATTACK_MAX_MS,
            AUDIO_ATTACK_DEFAULT_MS / AUDIO_ATTACK_MAX_MS,
            format!("{attack:.0} ms"),
            shape_reset(AudioShapeParam::Attack, AUDIO_ATTACK_DEFAULT_MS),
            false,
        ));
        rows.push(shape_slider(
            "Release",
            release / AUDIO_RELEASE_MAX_MS,
            AUDIO_RELEASE_DEFAULT_MS / AUDIO_RELEASE_MAX_MS,
            format!("{release:.0} ms"),
            shape_reset(AudioShapeParam::Release, AUDIO_RELEASE_DEFAULT_MS),
            false,
        ));
    }
    // D8: the Action row (Cont/Step/Rand) — every non-toggle, non-trigger
    // param card. Never built for `is_trigger`/`is_trigger_gate` (F2/D8
    // forbidden move): those rows count events by design, they don't step
    // them. Appended after the shaping sliders, so its flat button index
    // continues right after Invert (the three Slider rows above contribute
    // no buttons) — see `resolve_audio_config_click`, which must stay in
    // lockstep with this row order.
    let show_action = !info.spec.is_toggle && !info.spec.is_trigger;
    let action_idx = mod_state.audio_action_idx.get(i).copied().unwrap_or(0);
    if show_action {
        let action_buttons: Vec<DrawerButton> = audio_action_labels()
            .iter()
            .enumerate()
            .map(|(k, l)| DrawerButton::new(*l, k as i32 == action_idx))
            .collect();
        rows.push(DrawerRow::Buttons {
            buttons: action_buttons,
            width: ButtonWidth::Uniform,
            label: Some("Action".into()),
        });
        // While armed to Step: the Amount slider (a 4th `DrawerRow::Slider`,
        // `DrawerIds.sliders[3]`) then the Wrap row.
        if action_idx == 1 {
            let default_amount = default_step_amount(info.spec.min, info.spec.max, info.spec.whole_numbers);
            let amount = mod_state.audio_step_amount.get(i).copied().unwrap_or(default_amount);
            let value_text = if info.spec.whole_numbers {
                format!("{amount:.0}")
            } else {
                format!("{amount:.2}")
            };
            let step_reset = PanelAction::slider_reset(
                PanelAction::Modulation(ModulationAction::AudioModStepAmountSnapshot(gpt.clone(), pid.clone())),
                PanelAction::Modulation(ModulationAction::AudioModStepAmountChanged(gpt.clone(), pid.clone(), default_amount)),
                PanelAction::Modulation(ModulationAction::AudioModStepAmountCommit(gpt.clone(), pid.clone())),
            );
            rows.push(shape_slider(
                "Step",
                step_amount_to_norm(amount, info.spec.min, info.spec.max),
                step_amount_to_norm(default_amount, info.spec.min, info.spec.max),
                value_text,
                step_reset,
                false,
            ));
            let wrap_sel = mod_state.audio_wrap_idx.get(i).copied().unwrap_or(0);
            let wrap_buttons: Vec<DrawerButton> = audio_wrap_labels()
                .iter()
                .enumerate()
                .map(|(k, l)| DrawerButton::new(*l, k as i32 == wrap_sel))
                .collect();
            rows.push(DrawerRow::Buttons {
                buttons: wrap_buttons,
                width: ButtonWidth::Uniform,
                label: Some("Wrap".into()),
            });
        }
    }
    // §9 U2/D3: the trailing Mode row (Clip/Audio/Both). An `is_trigger_gate`
    // row always shows it; a slider row shows it once armed to Step or
    // Random — a step/random mod fires from the same clip-edge/audio-edge
    // sources a gate does, gated the same way (D3).
    let show_mode = info.spec.is_trigger_gate || (show_action && action_idx != 0);
    if show_mode {
        let mode_sel = mod_state.audio_mode_idx.get(i).copied().unwrap_or(0);
        let mode_buttons: Vec<DrawerButton> = audio_trigger_mode_labels()
            .iter()
            .enumerate()
            .map(|(m, l)| DrawerButton::new(*l, m as i32 == mode_sel))
            .collect();
        rows.push(DrawerRow::Buttons {
            buttons: mode_buttons,
            width: ButtonWidth::Uniform,
            label: Some("Mode".into()),
        });
    }
    let spec = DrawerSpec {
        rows,
        btn_font_size: config_font,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(AUDIO_MOD_ACTIVE_C32).tinted(),
    };
    let dids = drawer::build(tree, parent, x, cy, w, &spec, key);
    (dids, send_count)
}

/// Node IDs produced by [`build_toggle_trigger_row`].
pub(crate) struct ToggleTriggerRowIds {
    pub(crate) label_id: Option<NodeId>,
    pub(crate) button_id: NodeId,
    /// The "A" audio-mod button — `Some` for `is_trigger` (D5b) AND
    /// `is_trigger_gate` (§9) rows alike; both reach the SAME per-param
    /// drawer mechanism now. Plain toggles never build one (`None`, zero
    /// lane reserved).
    pub(crate) audio_btn: Option<NodeId>,
    /// The audio-mod drawer, when armed. Same shape as a slider row's
    /// `audio_config` so `resolve_audio_config_click` resolves both identically.
    pub(crate) audio_config: Option<(crate::panels::drawer::DrawerIds, usize)>,
    /// Collapsed-row mode indicator (§9 consequence, carried over from §8 D6:
    /// "Transient mode silently ignores clip launches... the drawer must
    /// show the mode on the collapsed card row"). `Some` only for
    /// `is_trigger_gate` rows; text is set (or left blank for the default
    /// `ClipEdge` mode) by the caller from the live `mod_state` — see
    /// `build_toggle_trigger_row`.
    pub(crate) mode_badge_id: Option<NodeId>,
    pub(crate) new_cy: f32,
}

/// Build a toggle or trigger row — a label plus a single button (ON/OFF for
/// a sticky toggle, "▶" for a momentary fire-once trigger) instead of a
/// slider. Shared verbatim by the effect and generator cards (Task A of
/// §8.4 P3b: effect cards previously had no toggle-row branch at all and
/// rendered `isToggle`/`isTrigger` params as raw sliders — the bug this
/// function fixes at the root by giving both kinds one code path).
///
/// The button right-aligns to the same column a slider row's VALUE cell
/// uses (`x + slider_w - TOGGLE_BTN_W`) — a toggle can't be modulated, so
/// the D/E/A lane further right stays empty for it. `is_trigger` (D5b) and
/// `is_trigger_gate` (§9) rows are the exception: both reach the standard
/// per-param audio-mod "A" button + drawer at the SAME column a slider row's
/// audio button would occupy, so the "A" column stays visually aligned down
/// the whole card regardless of row kind. `is_trigger_gate` additionally
/// shows the collapsed-row mode badge and gets the drawer's extra Mode row.
/// Driver/Envelope never apply to either (no continuous value to drive), so
/// only the Audio slot is ever built — no tab strip, no `active_mod_tabs`
/// multi-config machinery.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_toggle_trigger_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    slider_w: f32,
    info: &ParamRow,
    mod_state: &ParamModState,
    i: usize,
    target: GraphParamTarget,
    config_font: u16,
    // Whether this card reserves an envelope-button-width gap before the
    // driver column on its slider rows (effects gate on `supports_envelopes`;
    // generators always true) — needed so the trigger row's lone "A" button
    // lands in the same column slider rows in this card use.
    build_env_button: bool,
    has_osc: bool,
    row_key_base: Option<u64>,
    // P1 drawer tween: supplied only while a height tween is in flight
    // (mirrors `build_param_row`'s `drawer_reveal`) — the drawer then
    // builds under a clip region of that height instead of its natural
    // one, so mid-tween or bottom-straddling paint never escapes it.
    drawer_reveal: Option<f32>,
) -> ToggleTriggerRowIds {
    let toggle_btn_x = x + slider_w - TOGGLE_BTN_W;
    // `is_trigger_gate` rows reserve a fixed slot for the collapsed-row mode
    // badge (D6) just left of the toggle button, regardless of whether the
    // current mode has anything to show there — so the name label's width
    // (and therefore where its text can wrap/clip) never shifts when the
    // mode changes.
    let name_label_w = if info.spec.is_trigger_gate {
        (slider_w - TOGGLE_BTN_W - GAP - TRIGGER_GATE_BADGE_W - GAP).max(0.0)
    } else {
        (slider_w - TOGGLE_BTN_W - GAP).max(0.0)
    };
    let label_id = add_row_label(
        tree,
        parent,
        x,
        cy,
        name_label_w,
        ROW_HEIGHT,
        &info.spec.name,
        UIStyle {
            text_color: color::SLIDER_TEXT_C32,
            font_size: FONT_SIZE,
            text_align: TextAlign::Left,
            ..UIStyle::default()
        },
        row_key_base,
        ROW_ROLE_TOGGLE_LABEL,
    );
    if has_osc {
        tree.set_flag(label_id, UIFlags::INTERACTIVE);
    }

    let on = info.spec.default > 0.5;
    let (button_text, button_style) = if info.spec.is_trigger {
        // Trigger renders as a momentary button — always neutral.
        ("▶", toggle_btn_style(false))
    } else {
        (if on { "ON" } else { "OFF" }, toggle_btn_style(on))
    };
    let toggle_y = cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5;
    let button_id = add_row_button(
        tree,
        parent,
        toggle_btn_x,
        toggle_y,
        TOGGLE_BTN_W,
        TOGGLE_BTN_H,
        button_style,
        button_text,
        row_key_base,
        ROW_ROLE_TOGGLE,
    );

    let row_top_y = cy;
    let mut cy = cy + ROW_HEIGHT + ROW_SPACING;
    let mut audio_btn = None;
    let mut audio_config = None;
    let mut mode_badge_id = None;

    // is_trigger (D5b) and is_trigger_gate (§9) both reach the standard
    // per-param audio-mod "A" drawer — a fire-button counts by count-add, a
    // trigger-gate card fires a pulse (never writing the toggle's value, R2)
    // and additionally gets the drawer's trailing Mode row + the collapsed-
    // row mode badge. Plain toggles keep zero lane space (no button, no
    // drawer) — the row-label branch above is unchanged for them.
    if info.spec.is_trigger || info.spec.is_trigger_gate {
        let env_arm_w = if build_env_button { DE_BUTTON_SIZE + DE_BUTTON_GAP } else { 0.0 };
        let btn_x = x + slider_w + MOD_LANE_GAP;
        let drv_btn_x = btn_x + env_arm_w;
        let audio_btn_x = drv_btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP;
        let btn_y = toggle_y;
        let audio_active = mod_state.audio_active.get(i).copied().unwrap_or(false);
        let btn_id = add_row_button(
            tree,
            parent,
            audio_btn_x,
            btn_y,
            DE_BUTTON_SIZE,
            DE_BUTTON_SIZE,
            de_btn_style(audio_active, AUDIO_MOD_ACTIVE_C32),
            "A",
            row_key_base,
            ROW_ROLE_AUDIO,
        );
        audio_btn = Some(btn_id);

        if audio_active {
            let drawer_x = x + DRAWER_INDENT;
            let row_right = audio_btn_x + DE_BUTTON_SIZE;
            let drawer_w = (row_right - drawer_x).max(1.0);
            // P1 drawer tween parity with `build_param_row` (:2089-2101): when a
            // reveal height is supplied, the drawer builds under a clip region of
            // that height (revealing top-down) instead of `parent` directly.
            let drawer_top = cy;
            let animate_drawer = drawer_reveal.is_some();
            let drawer_parent: Option<NodeId> = if animate_drawer {
                let reveal = drawer_reveal.unwrap_or(0.0).max(0.0);
                let rect = Rect::new(x, drawer_top, (row_right - x).max(1.0), reveal);
                Some(match row_key_base {
                    Some(base) => tree.add_node_keyed(
                        parent,
                        rect,
                        UINodeType::ClipRegion,
                        UIStyle::default(),
                        None,
                        UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
                        base | ROW_ROLE_DRAWER_CLIP,
                    ),
                    None => tree.add_node(
                        parent,
                        rect,
                        UINodeType::ClipRegion,
                        UIStyle::default(),
                        None,
                        UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
                    ),
                })
            } else {
                parent
            };
            let (dids, send_count) = build_audio_mod_drawer(
                tree,
                drawer_parent,
                drawer_x,
                cy,
                drawer_w,
                mod_state,
                i,
                config_font,
                info,
                target,
                row_key_base.map(|b| b | ROW_ROLE_AUDIO_CONFIG),
            );
            if animate_drawer {
                cy = drawer_top + drawer_reveal.unwrap_or(0.0).max(0.0);
            } else {
                cy += dids.height;
                // Mirrors `row_drawer_height`'s `+ DRAWER_BOTTOM_GAP` for the
                // ≥1-active-config case, so build and height computation agree.
                cy += DRAWER_BOTTOM_GAP;
            }
            audio_config = Some((dids, send_count));
        }

        if info.spec.is_trigger_gate {
            // Collapsed-row mode indicator (§9, carried over from §8 D6):
            // "Transient mode silently ignores clip launches... the drawer
            // must show the mode on the collapsed card row" — shown whether
            // or not the drawer itself is open, so a user who never re-opens
            // the drawer still sees it. Blank for the default `ClipEdge`
            // (index 0) — the common, unsurprising case gets no badge at
            // all. A fixed-width slot just left of the toggle button,
            // reserved on every `is_trigger_gate` row regardless of current
            // mode, so the badge appearing/disappearing on a mode change
            // never shifts the toggle button's column.
            let mode_idx = mod_state.audio_mode_idx.get(i).copied().unwrap_or(0);
            let mode_text = if audio_active && mode_idx > 0 {
                audio_trigger_mode_labels().get(mode_idx as usize).copied().unwrap_or("")
            } else {
                ""
            };
            let badge_w = TRIGGER_GATE_BADGE_W;
            let badge_x = toggle_btn_x - badge_w - GAP;
            mode_badge_id = Some(tree.add_label(
                parent,
                badge_x,
                row_top_y,
                badge_w,
                ROW_HEIGHT,
                mode_text,
                UIStyle {
                    text_color: AUDIO_MOD_ACTIVE_C32,
                    font_size: color::FONT_CAPTION,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            ));
        }
    }

    // Automation naming pass (`WIDGET_TREE_DESIGN.md` §5) — mirror the slider row.
    // A toggle/trigger row has no separate row-catcher; its button IS the row's
    // identity and its sole drivable control, so the param-id-derived name lands
    // there.
    let pid: &str = &info.id;
    tree.set_name(button_id, format!("param_row.{pid}"));

    ToggleTriggerRowIds {
        label_id: Some(label_id),
        button_id,
        audio_btn,
        audio_config,
        mode_badge_id,
        new_cy: cy,
    }
}

pub(crate) fn build_param_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    slider_w: f32,
    info: &ParamRow,
    mod_state: &ParamModState,
    i: usize,
    target: GraphParamTarget,
    slider_colors: &SliderColors,
    config_font: u16,
    build_env_button: bool,
    // Width of the left-aligned label cell at the row's left edge. The
    // inspector passes the default; the graph editor's wide lane passes a
    // larger value so friendly names ("Particle Count") don't clip.
    label_width: f32,
    // Which config the modulation drawer shows when ≥2 are active (the panel's
    // stored per-param choice). Ignored when 0–1 configs are active.
    active_tab: ModTab,
    // §6b: when false (compact mode), the config drawer + tab strip are not built
    // — the row, arm buttons, and slider track overlays still show, so mods stay
    // armed and their live ranges remain visible; only the settings are hidden.
    show_drawer: bool,
    // When `Some(base)`, the row's interactive arm buttons take an explicit,
    // reorder-stable WidgetId (`base | ROW_ROLE_*`) instead of an auto sibling
    // salt — so arming a modulator on an earlier row (which inserts drawer nodes
    // and shifts every later sibling) can't renumber this row's controls. The
    // editor card (Author context) passes `Some(param_index << 8)`; the perform
    // inspector passes `None` and is unchanged. See `docs/INPUT_IDENTITY_UNIFICATION.md`.
    row_key_base: Option<u64>,
    // P1 drawer open/close tween (`UI_CRAFT_AND_MOTION_PLAN.md`): while a reveal
    // height is supplied, the modulation-drawer block builds under a clip region
    // sized to that height and the row reserves exactly that height, so the drawer
    // grows/shrinks and everything below reflows in lockstep. `None` = settled /
    // no animation → the drawer builds directly under `parent` and reserves its
    // natural height, byte-identical to the pre-motion layout (so the golden card
    // tests, which build settled, are unaffected).
    drawer_reveal: Option<f32>,
) -> ParamRowIds {
    // The main slider's right-click reset — constructed up front so it can
    // seed both `ids.slider_reset` (below) and the `BitmapSlider::build` call
    // that materialises the track it fires on.
    let reset = PanelAction::slider_reset(
        PanelAction::Scrub(ValueRef::Param(target.clone(), info.id.clone()), ScrubPhase::Begin),
        PanelAction::Scrub(
            ValueRef::Param(target.clone(), info.id.clone()),
            ScrubPhase::Move(ScrubValue::Scalar(info.spec.default)),
        ),
        PanelAction::Scrub(ValueRef::Param(target.clone(), info.id.clone()), ScrubPhase::Commit),
    );
    let mut ids = ParamRowIds {
        // Overwritten with the real row-catcher node below before any read.
        row_catcher: NodeId::PLACEHOLDER,
        slider: None,
        slider_reset: reset.clone(),
        trim: None,
        audio_trim: None,
        target: None,
        ableton_trim: None,
        envelope_btn: None,
        // Overwritten with the real driver/audio buttons below.
        driver_btn: NodeId::PLACEHOLDER,
        audio_btn: NodeId::PLACEHOLDER,
        envelope_config: None,
        driver_config: None,
        ableton_config: None,
        audio_config: None,
        mod_tabs: Vec::new(),
        new_cy: cy,
    };
    let mut cy = cy;

    let norm = BitmapSlider::value_to_normalized(info.spec.default, info.spec.min, info.spec.max);
    let val_text = format_param_value(
        info.spec.default,
        info.spec.min,
        info.spec.whole_numbers,
        info.spec.is_angle,
        info.spec.value_labels.as_deref(),
    );
    let slider_rect = Rect::new(x, cy, slider_w, ROW_HEIGHT);

    // Modulation-button column x's (computed up front so the mod card, the drawer,
    // and the arm buttons all derive from one set of positions). `row_right` is the
    // mod-button column's right edge — the right edge of the card and the drawer.
    let env_arm_w = if build_env_button {
        DE_BUTTON_SIZE + DE_BUTTON_GAP
    } else {
        0.0
    };
    let btn_x = x + slider_w + MOD_LANE_GAP;
    let drv_btn_x = btn_x + env_arm_w;
    let audio_btn_x = drv_btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP;
    let row_right = audio_btn_x + DE_BUTTON_SIZE;

    // Which modulation configs are active, and which one the drawer shows. Computed
    // here (not just before the drawer) because the mod card behind the row needs it.
    let active_tabs = if show_drawer {
        active_mod_tabs(mod_state, info, i)
    } else {
        Vec::new()
    };
    let shown_tab = resolve_active_tab(&active_tabs, active_tab);

    // Mod card: when a config drawer is open, the slider row and its drawer share
    // ONE source-tinted card (rounded, no spine) so the drawer reads as part of its
    // slider — the whole modulated param is one backed unit, tinted by the shown
    // source. Drawn FIRST so the slider, arm buttons, and drawer render on top.
    // Visual only: it does not advance `cy`, so the card never affects height math.
    if let Some(tab) = shown_tab {
        let card_theme = Theme::INSPECTOR.with_accent(mod_tab_accent(tab)).tinted();
        let tab_strip_h = if active_tabs.len() >= 2 {
            MOD_TAB_STRIP_H
        } else {
            0.0
        };
        // Pad out on top + left + right so the content sits inset from the card
        // edge (and the top covers the slider's trim / target handles). Bottom needs
        // no pad — the drawer's internal TOP_PAD already insets the last row. The top
        // pad folds into card_h so the bottom edge is unchanged.
        // A slider row is never `is_trigger_gate` (that's always a toggle
        // row, built by `build_toggle_trigger_row` instead) — `mod_config_height`
        // still derives the Action/Amount/Wrap/Mode rows (D8) from `info`/
        // `mod_state` for the Audio tab.
        let card_h = MOD_CARD_PAD
            + ROW_HEIGHT
            + ROW_SPACING
            + tab_strip_h
            + mod_config_height(tab, info, mod_state, i);
        let card_w = (row_right - x + MOD_CARD_PAD * 2.0).max(1.0);
        tree.add_panel(
            parent,
            x - MOD_CARD_PAD,
            cy - MOD_CARD_PAD,
            card_w,
            card_h,
            card_theme.surface_style(color::CARD_RADIUS),
        );
    }

    // Full-row hit catcher, added BEFORE the slider widgets so reverse-insertion
    // hit-testing lets the track/label win on top and the catcher only collects
    // the value cell + gaps. Transparent + interactive; carries no visual.
    ids.row_catcher = match row_key_base {
        Some(base) => tree.add_node_keyed(
            parent,
            slider_rect,
            UINodeType::Panel,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::INTERACTIVE,
            base | ROW_ROLE_ROW_CATCHER,
        ),
        None => tree.add_node(
            parent,
            slider_rect,
            UINodeType::Panel,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::INTERACTIVE,
        ),
    };

    let slider = BitmapSlider::build(
        tree,
        parent,
        slider_rect,
        Some(&info.spec.name),
        norm,
        &val_text,
        slider_colors,
        FONT_SIZE,
        label_width,
        // `norm` above is already `value_to_normalized(info.default, ..)` — the
        // row always builds showing the default (sync_values pushes the live
        // value right after), so it doubles as the reset target.
        norm,
        reset,
        row_key_base.map(|base| base | ROW_ROLE_SLIDER),
    )
    .ids;

    // Make label interactive for click-to-copy OSC address + Ableton mapping.
    if let Some(label_id) = slider.label {
        tree.set_flag(label_id, UIFlags::INTERACTIVE);
    }

    // "Automated" indicator (P4 §7 last bullet, Live's red dot): a small,
    // non-interactive circle at the left edge of the label cell when this
    // param carries an enabled automation lane. Red while live, grays when
    // the lane is overridden (latched) — same red/gray pairing as the lane
    // strips and the transport BACK button.
    if mod_state.automation_active.get(i).copied().unwrap_or(false) {
        let overridden = mod_state.automation_overridden.get(i).copied().unwrap_or(false);
        let dot_color = if overridden {
            color::AUTOMATION_LINE_OVERRIDDEN_COLOR
        } else {
            color::AUTOMATION_LINE_COLOR
        };
        const AUTOMATION_DOT_D: f32 = 5.0;
        let dot_y = cy + (ROW_HEIGHT - AUTOMATION_DOT_D) * 0.5;
        tree.add_panel(
            parent,
            x + 1.0,
            dot_y,
            AUTOMATION_DOT_D,
            AUTOMATION_DOT_D,
            UIStyle {
                bg_color: dot_color,
                corner_radius: AUTOMATION_DOT_D * 0.5,
                ..UIStyle::default()
            },
        );
    }

    // Trim handles (if driver expanded). Bounds come from the tree (the
    // track was just built, so they're live), not the panel cache (BUG-259).
    if mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
        ids.trim = Some(build_trim_handles(
            tree,
            slider.track,
            tree.get_bounds(slider.track),
            mod_state,
            i,
        ));
    }

    // Envelope target handle on the slider track (when the envelope is armed) —
    // the orange grab bar that sets the depth in the parameter's own range.
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        ids.target = Some(build_envelope_target(
            tree,
            slider.track,
            tree.get_bounds(slider.track),
            mod_state,
            i,
        ));
    }

    // Ableton trim handles (when the param has an Ableton mapping).
    if let Some((amin, amax)) = info.mapping.ableton_range {
        ids.ableton_trim = Some(build_trim_handles_explicit(
            tree,
            slider.track,
            tree.get_bounds(slider.track),
            amin,
            amax,
            color::ABL_TRIM_BAR_C32,
            color::ABL_TRIM_BAR_HOVER_C32,
            color::ABL_TRIM_FILL_C32,
        ));
    }

    // Green audio-mod trim handles (when an audio mod is armed) — the output
    // sub-range the audio drives. Drawn on top of any driver/Ableton handles so
    // all active modulators show their range at once, told apart by color.
    if mod_state.audio_active.get(i).copied().unwrap_or(false) {
        let amin = mod_state.audio_range_min.get(i).copied().unwrap_or(0.0);
        let amax = mod_state.audio_range_max.get(i).copied().unwrap_or(1.0);
        ids.audio_trim = Some(build_trim_handles_explicit(
            tree,
            slider.track,
            tree.get_bounds(slider.track),
            amin,
            amax,
            color::AUDIO_TRIM_BAR_C32,
            color::AUDIO_TRIM_BAR_HOVER_C32,
            color::AUDIO_TRIM_FILL_C32,
        ));
    }

    ids.slider = Some(slider);

    // D/E buttons (right of the slider row), at the column x's computed up top.
    let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
    if build_env_button {
        let env_active = mod_state.envelope_expanded.get(i).copied().unwrap_or(false);
        ids.envelope_btn = Some(add_row_button(
            tree,
            parent,
            btn_x,
            btn_y,
            DE_BUTTON_SIZE,
            DE_BUTTON_SIZE,
            de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
            "T", // Trigger
            row_key_base,
            ROW_ROLE_ENV,
        ));
    }
    let drv_active = mod_state.driver_expanded.get(i).copied().unwrap_or(false);
    // LFO arm button shows the waveform icon for the driver's current shape (the
    // UIRenderer draws the SDF waveform atlas icon). Defaults to sine when unset.
    // A plain "∿" char isn't in the UI font — it renders as tofu.
    let lfo_wave = mod_state.driver_waveform_idx.get(i).copied().unwrap_or(0);
    let lfo_icon = crate::icons::waveform_icon_char(lfo_wave).to_string();
    ids.driver_btn = add_row_button(
        tree,
        parent,
        drv_btn_x,
        btn_y,
        DE_BUTTON_SIZE,
        DE_BUTTON_SIZE,
        de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
        &lfo_icon,
        row_key_base,
        ROW_ROLE_DRV,
    );

    // Audio-modulation button — third in the lane, right of the driver button.
    // D8 "silent mode trap": when armed to Step/Random, the glyph swaps to
    // "S"/"R" so a closed drawer still shows the armed action at a glance —
    // the same idiom the driver button's waveform-icon swap uses above.
    let audio_active = mod_state.audio_active.get(i).copied().unwrap_or(false);
    let audio_label = match mod_state.audio_action_idx.get(i).copied().unwrap_or(0) {
        1 if audio_active => "S",
        2 if audio_active => "R",
        _ => "A",
    };
    ids.audio_btn = add_row_button(
        tree,
        parent,
        audio_btn_x,
        btn_y,
        DE_BUTTON_SIZE,
        DE_BUTTON_SIZE,
        de_btn_style(audio_active, AUDIO_MOD_ACTIVE_C32),
        audio_label,
        row_key_base,
        ROW_ROLE_AUDIO,
    );

    // Automation naming pass (`WIDGET_TREE_DESIGN.md` §5, D8/§3): every converged
    // card row carries a param-id-derived name on its row-root and its drivable
    // controls, so a `--script` flow can find and drive the row directly. Unlike
    // the mute/solo-chip idiom (one static name, `under_text` picks the row), a
    // flat param row defeats `under_text`: the nearest preceding texted sibling of
    // the driver button is the VALUE cell, not the label, so the row's own name
    // must BE its selector. Names duplicate across surfaces that render the same
    // param (e.g. the same modifier in the scene dock and the inspector) — flows
    // disambiguate with `nth`, exactly as the resolver intends. Owned names die
    // with the rebuild (see `UITree::set_name`) — no leak, no interner.
    let pid: &str = &info.id;
    tree.set_name(ids.row_catcher, format!("param_row.{pid}"));
    if let Some(s) = ids.slider.as_ref() {
        tree.set_name(s.track, format!("param_row.{pid}.slider"));
        tree.set_name(s.value_text, format!("param_row.{pid}.value"));
    }
    tree.set_name(ids.driver_btn, format!("param_row.{pid}.driver_btn"));

    cy += ROW_HEIGHT + ROW_SPACING;

    // P1 drawer tween: top of the modulation-drawer block. When a reveal height is
    // supplied AND this row actually has a drawer, the whole block builds under a
    // clip region of that height (revealing top-down as it grows) and `new_cy`
    // reserves exactly that height so content below reflows with it. Otherwise the
    // block builds under `parent` and reserves its natural height as before.
    let drawer_top = cy;
    // A reveal height only matters when this row actually has a drawer (active
    // config); a row with no active config stays on the natural path.
    let animate_drawer = drawer_reveal.is_some() && !active_tabs.is_empty();
    // When animating, every drawer node parents to a clip region of the reveal
    // height (revealing top-down); otherwise they parent to `parent` unchanged.
    let drawer_parent: Option<NodeId> = if animate_drawer {
        let reveal = drawer_reveal.unwrap_or(0.0).max(0.0);
        let rect = Rect::new(x, drawer_top, (row_right - x).max(1.0), reveal);
        Some(match row_key_base {
            Some(base) => tree.add_node_keyed(
                parent,
                rect,
                UINodeType::ClipRegion,
                UIStyle::default(),
                None,
                UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
                base | ROW_ROLE_DRAWER_CLIP,
            ),
            None => tree.add_node(
                parent,
                rect,
                UINodeType::ClipRegion,
                UIStyle::default(),
                None,
                UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
            ),
        })
    } else {
        parent
    };

    // Drawer geometry: a slight left inset from the row's label edge so the config
    // rows read as sub-controls under the slider, right edge at the mod-button
    // column's right edge. The drawer's rows render ON the one mod card drawn above
    // (transparent container) — that shared card is what binds drawer to slider.
    let drawer_x = x + DRAWER_INDENT;
    let drawer_w = (row_right - drawer_x).max(1.0);

    // Modulation config drawer. Zero or one active config shows directly (no tab
    // strip — unchanged); two or more share this one drawer behind a tab strip
    // so they never stack three deep (§6.2). The T/∿/A arm buttons above stay on
    // the row, so arming is still one click. Track overlays (driver/audio trim
    // bars, envelope target) live on the slider above and show for every armed
    // mod regardless of which config tab is open. `active_tabs` / `shown_tab` were
    // resolved up top (the mod card needed them).
    if active_tabs.len() >= 2 {
        ids.mod_tabs =
            build_mod_tab_strip(tree, drawer_parent, drawer_x, cy, drawer_w, &active_tabs, shown_tab);
        cy += MOD_TAB_STRIP_H;
    }

    // Envelope drawer — a single "Decay" slider. Depth is the orange target
    // handle on the track above; this is how fast the value falls back.
    if shown_tab == Some(ModTab::Envelope) {
        ids.envelope_config = Some(build_envelope_config(
            tree, drawer_parent, drawer_x, cy, drawer_w, mod_state, i, target.clone(), info.id.clone(),
            row_key_base.map(|b| b | ROW_ROLE_ENVELOPE_CONFIG),
        ));
        cy += ENV_CONFIG_HEIGHT;
    }

    // Driver config drawer.
    if shown_tab == Some(ModTab::Driver) {
        ids.driver_config = Some(build_driver_config(
            tree,
            drawer_parent,
            drawer_x,
            cy,
            drawer_w,
            mod_state,
            i,
            config_font,
            row_key_base.map(|b| b | ROW_ROLE_DRIVER_CONFIG),
        ));
        cy += driver_config_height();
    }

    // Ableton config drawer. ModTab::Ableton is only in the active set when a
    // mapping exists, so the let-binding always resolves here.
    if shown_tab == Some(ModTab::Ableton)
        && let Some(ref display) = info.mapping.ableton_display
    {
        ids.ableton_config = Some(build_ableton_config(
            tree, drawer_parent, drawer_x, cy, drawer_w, display,
            row_key_base.map(|b| b | ROW_ROLE_ABLETON_CONFIG),
        ));
        cy += ABL_CONFIG_HEIGHT;
    }

    // Audio-modulation drawer — shown when the Audio config tab is active.
    // Extracted to `build_audio_mod_drawer` (shared with
    // `build_toggle_trigger_row`'s `is_trigger`/`is_trigger_gate` cases,
    // D5b/§9). A slider row is never `is_trigger_gate`, but it DOES get the
    // Action/Amount/Wrap rows (D8) — derived inside from `info`.
    if shown_tab == Some(ModTab::Audio) {
        let (dids, send_count) = build_audio_mod_drawer(
            tree, drawer_parent, drawer_x, cy, drawer_w, mod_state, i, config_font, info, target,
            row_key_base.map(|b| b | ROW_ROLE_AUDIO_CONFIG),
        );
        cy += dids.height;
        ids.audio_config = Some((dids, send_count));
    }

    // Reserve height for the content below. When animating, reserve exactly the
    // reveal height (which the tween eases toward `row_drawer_height`, gap
    // included), so the drawer's clipped reveal and the reflow below move in
    // lockstep. Otherwise advance the natural cy plus the post-drawer break —
    // byte-identical to before, and mirrored in `row_drawer_height`.
    if animate_drawer {
        cy = drawer_top + drawer_reveal.unwrap_or(0.0).max(0.0);
    } else if !active_tabs.is_empty() {
        cy += DRAWER_BOTTOM_GAP;
    }

    ids.new_cy = cy;
    ids
}

// ── Shared per-parameter click dispatch ─────────────────────────────
//
// The old array-scanning row-click gauntlet DIED in P2
// (`docs/WIDGET_TREE_DESIGN.md` D5) — `ParamCardPanel::row_action` routes
// through `RowIndex` instead. `AudioConfigClick`/`resolve_audio_config_click`
// below is the one surviving per-role resolver: the audio drawer's flat
// button index can't be split into typed sub-fields the way driver/ableton
// config can (`DriverConfigIds::resolve`/`AbletonConfigIds::resolve`), so it
// stays a function — but scoped to the ONE row `row_action` already
// resolved via `RowIndex`, never scanning every row's drawer.

/// A click inside ONE row's audio-mod drawer, resolved from its flat button
/// index (`DrawerIds::resolve_button`). Mirrors the variant shapes
/// `PanelAction`'s `AudioMod*` family expects; `row_action` supplies `pi`.
pub(crate) enum AudioConfigClick {
    SelectSend(usize),
    SelectChip(usize),
    ToggleMatrix,
    SelectKind(usize),
    SelectBand(usize),
    ToggleInvert,
    SelectAction(usize),
    SelectWrap(usize),
    SelectTriggerMode(usize),
}

/// Resolve a clicked node against ONE row's audio-mod drawer. Flat index
/// layout: sends, the Listen chips (`trigger_source_chips(current)` + the
/// trailing "Custom" cell), then — only while the matrix is open — the
/// Feature and Band rows, then — only where shaping is offered (every target
/// EXCEPT `is_trigger_gate`, which fires on the raw BUG-242 edge) the Invert
/// toggle, then (D8, non-toggle/non-trigger rows only) the Action row, then
/// — while armed to Step — the Wrap row, then the trailing Mode row (§9
/// U2/D3). Must stay in lockstep with the row order `build_audio_mod_drawer`
/// actually builds.
pub(crate) fn resolve_audio_config_click(
    dids: &crate::panels::drawer::DrawerIds,
    send_count: usize,
    mod_state: &ParamModState,
    row: &ParamRow,
    pi: usize,
    node_id: NodeId,
) -> Option<AudioConfigClick> {
    let flat = dids.resolve_button(node_id)?;
    if flat < send_count {
        return Some(AudioConfigClick::SelectSend(flat));
    }
    let mut f = flat - send_count;
    let current = crate::types::AudioFeature::new(
        audio_kind_from_index(mod_state.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize),
        audio_band_from_index(mod_state.audio_band_idx.get(pi).copied().unwrap_or(0) as usize),
    );
    let chip_count = trigger_source_chips(current).len();
    if f < chip_count {
        return Some(AudioConfigClick::SelectChip(f));
    }
    f -= chip_count;
    if f == 0 {
        return Some(AudioConfigClick::ToggleMatrix);
    }
    f -= 1;
    if mod_state.audio_matrix_open.get(pi).copied().unwrap_or(false) {
        if f < AUDIO_KIND_COUNT {
            return Some(AudioConfigClick::SelectKind(f));
        }
        f -= AUDIO_KIND_COUNT;
        if f < AUDIO_BAND_COUNT {
            return Some(AudioConfigClick::SelectBand(f));
        }
        f -= AUDIO_BAND_COUNT;
    }
    let is_gate = row.spec.is_trigger_gate;
    if !is_gate {
        if f == 0 {
            return Some(AudioConfigClick::ToggleInvert);
        }
        f -= 1;
    }
    let show_action = !row.spec.is_toggle && !row.spec.is_trigger;
    if show_action {
        if f < AUDIO_ACTION_COUNT {
            return Some(AudioConfigClick::SelectAction(f));
        }
        f -= AUDIO_ACTION_COUNT;
        let action_idx = mod_state.audio_action_idx.get(pi).copied().unwrap_or(0);
        if action_idx == 1 {
            if f < AUDIO_WRAP_COUNT {
                return Some(AudioConfigClick::SelectWrap(f));
            }
            return Some(AudioConfigClick::SelectTriggerMode(f - AUDIO_WRAP_COUNT));
        }
        return Some(AudioConfigClick::SelectTriggerMode(f));
    }
    Some(AudioConfigClick::SelectTriggerMode(f))
}

#[cfg(test)]
mod length_row_tests {
    use super::*;

    // P3 D4/D5: the drawer's new Length row — `format_beats` (moved from the
    // deleted Triggers matrix), `length_labels`, and `length_option_index`.
    // These are the pure functions `build_audio_mod_drawer`'s Length row
    // (`length_beats: Option<f32>`) is built from; a UITree-level test of the
    // drawer itself would need to replicate `param_card.rs`'s fixture
    // scaffolding, so correctness of the row's actual content is proven here
    // at the value level, matching this crate's usual split (pure logic
    // tested directly, layout proven by the headless PNG demo).

    #[test]
    fn format_beats_matches_musical_divisions() {
        assert_eq!(format_beats(0.25), "1/4");
        assert_eq!(format_beats(0.5), "1/2");
        assert_eq!(format_beats(1.0), "1b");
        assert_eq!(format_beats(2.0), "2b");
        assert_eq!(format_beats(4.0), "4b");
        assert_eq!(format_beats(8.0), "8b");
    }

    #[test]
    fn length_labels_are_format_beats_of_length_options() {
        let labels = length_labels();
        for (label, beats) in labels.iter().zip(LENGTH_OPTIONS.iter()) {
            assert_eq!(label, &format_beats(*beats));
        }
    }

    #[test]
    fn length_option_index_snaps_to_nearest() {
        // Exact hits.
        assert_eq!(length_option_index(0.25), 0);
        assert_eq!(length_option_index(1.0), 2);
        assert_eq!(length_option_index(8.0), 5);
        // A legacy-migrated value that doesn't land exactly on an option
        // (BUG-079's sensitivity→Amount U5 mapping is the same "not exact,
        // snap" shape) snaps to the closer neighbor.
        assert_eq!(length_option_index(0.9), 2, "0.9 nearer to 1.0 than 0.5");
        assert_eq!(length_option_index(0.6), 1, "0.6 nearer to 0.5 than 1.0");
        assert_eq!(length_option_index(0.4), 1, "0.4 nearer to 0.5 than 0.25");
        assert_eq!(length_option_index(100.0), 5, "clamps to the largest option");
        assert_eq!(length_option_index(0.0), 0, "clamps to the smallest option");
    }
}
