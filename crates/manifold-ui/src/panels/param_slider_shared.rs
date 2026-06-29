//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! The unified `ParamCardPanel` (effect + generator kinds) uses identical
//! layout constants, driver/envelope config builders, trim/target handle
//! builders, and formatting helpers across both kinds. This module is the
//! single source of truth for them.

use super::DriverConfigAction;
use super::TrimKind;
use super::param_card::ParamInfo;
use crate::chrome::{Theme, View};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
pub use crate::types::AbletonMappingStatus;

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
/// Top overhang of the mod card above the slider row. The slider's trim / envelope
/// target handles poke a couple px above the track (e.g. `build_envelope_target`
/// starts at `track.y - 2`); the card extends up by this so it covers them instead
/// of clipping their tops. Visual only — does not move the slider or affect height.
pub(crate) const MOD_CARD_TOP_PAD: f32 = 4.0;
// Card inner inset (§14.5 C). The canonical `SPACE_M`: with the card's 1px frame
// border that puts param-label content at `BORDER_W + SPACE_M` =
// `color::SECTION_CONTENT_INSET`, the one column the border-less chrome panels
// align to. `slider_w` / `label_width` / the header trailing-x all derive from
// this, so they cascade.
pub(crate) const PADDING: f32 = color::SPACE_M;
pub(crate) const GAP: f32 = color::SPACE_S;
// Param rows run one step larger than body chrome so the name + value read
// clearly in the inspector (they're the live instrument surface).
pub(crate) const FONT_SIZE: u16 = 12;

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

/// Height of the per-param audio-modulation drawer. Rows: send selector, the
/// Level feature row, the Tone feature row, the three shaping sliders
/// (Amount/Attack/Release), and the modifier toggles (Inv/d-dt). Derived from
/// the shared drawer metrics so the card's reserved height can't drift from
/// what's actually drawn.
pub(crate) fn audio_config_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(7)
}

/// Full-scale for the audio "Amount" (sensitivity) slider: 0..this.
pub(crate) const AUDIO_SENS_MAX: f32 = 4.0;
/// Full-scale for the audio "Attack" slider, in ms: 0..this.
pub(crate) const AUDIO_ATTACK_MAX_MS: f32 = 500.0;
/// Full-scale for the audio "Release" slider, in ms: 0..this.
pub(crate) const AUDIO_RELEASE_MAX_MS: f32 = 2000.0;
/// Leading-label width for the audio shaping sliders.
pub(crate) const AUDIO_SHAPE_LABEL_W: f32 = 52.0;

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

// Waveform text labels kept for accessibility / tooltips if needed later.
#[allow(dead_code)]
pub(crate) const WAVEFORM_LABELS: [&str; WAVEFORM_COUNT] = ["Sin", "Tri", "Saw", "Sqr", "Rnd"];

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

/// Feature-row button labels, in `AudioFeatureKind::ALL` order.
pub(crate) fn audio_kind_labels() -> [&'static str; 5] {
    [
        crate::types::AudioFeatureKind::Amplitude.label(),
        crate::types::AudioFeatureKind::Centroid.label(),
        crate::types::AudioFeatureKind::Noisiness.label(),
        crate::types::AudioFeatureKind::Flux.label(),
        crate::types::AudioFeatureKind::Transients.label(),
    ]
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
pub(crate) const AUDIO_KIND_COUNT: usize = 5;
pub(crate) const AUDIO_BAND_COUNT: usize = 4;

/// Audio-modulation display state for one card, assembled in `state_sync` and
/// applied to [`ParamModState`] via [`ParamModState::sync_audio`]. Bundled so
/// `ParamCardConfig` gains one field, not five.
#[derive(Debug, Default, Clone)]
pub struct AudioCardState {
    /// Per-param: mod exists and is enabled.
    pub active: Vec<bool>,
    /// Per-param: the mod's send id, if any. Resolved to an index into
    /// `send_ids` by [`ParamModState::sync_audio`].
    pub send_id: Vec<Option<manifold_foundation::AudioSendId>>,
    /// Per-param: selected feature `kind` and `band` indices (the matrix axes).
    pub kind_idx: Vec<i32>,
    pub band_idx: Vec<i32>,
    /// Per-param: the mod's output sub-range (`AudioModShape::range_min/max`).
    pub range_min: Vec<f32>,
    pub range_max: Vec<f32>,
    /// Per-param: the mod's invert flag (`AudioModShape::invert`).
    pub invert: Vec<bool>,
    /// Per-param: the mod's rate-of-change flag (`AudioModShape::rate_of_change`).
    pub rate: Vec<bool>,
    /// Per-param: the mod's shaping values (sensitivity, attack ms, release ms).
    pub sensitivity: Vec<f32>,
    pub attack_ms: Vec<f32>,
    pub release_ms: Vec<f32>,
    /// Card-level: available send labels.
    pub send_labels: Vec<String>,
    /// Card-level: send ids parallel to `send_labels` — what the click handler
    /// turns a selected index into for the `AudioModSetSource` command.
    pub send_ids: Vec<manifold_foundation::AudioSendId>,
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
        }
    }

    /// Sync audio-modulation display state from the card config.
    pub fn sync_audio(&mut self, n: usize, audio: &AudioCardState) {
        for i in 0..n {
            self.audio_active[i] = audio.active.get(i).copied().unwrap_or(false);
            self.audio_kind_idx[i] = audio.kind_idx.get(i).copied().unwrap_or(0);
            self.audio_band_idx[i] = audio.band_idx.get(i).copied().unwrap_or(0);
            self.audio_range_min[i] = audio.range_min.get(i).copied().unwrap_or(0.0);
            self.audio_range_max[i] = audio.range_max.get(i).copied().unwrap_or(1.0);
            self.audio_invert[i] = audio.invert.get(i).copied().unwrap_or(false);
            self.audio_rate[i] = audio.rate.get(i).copied().unwrap_or(false);
            self.audio_sensitivity[i] = audio.sensitivity.get(i).copied().unwrap_or(1.0);
            self.audio_attack_ms[i] = audio.attack_ms.get(i).copied().unwrap_or(5.0);
            self.audio_release_ms[i] = audio.release_ms.get(i).copied().unwrap_or(120.0);
            self.audio_send_idx[i] = audio
                .send_id
                .get(i)
                .and_then(|o| o.as_ref())
                .and_then(|sid| audio.send_ids.iter().position(|s| s == sid))
                .map(|p| p as i32)
                .unwrap_or(-1);
        }
        self.audio_send_labels = audio.send_labels.clone();
        self.audio_send_ids = audio.send_ids.clone();
    }

    /// Sync driver/envelope/trim/target/decay state from config vectors.
    /// `n` is the param count. Reads from config slices with fallback defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_from_config(
        &mut self,
        n: usize,
        driver_active: &[bool],
        envelope_active: &[bool],
        trim_min: &[f32],
        trim_max: &[f32],
        target_norm: &[f32],
        env_decay: &[f32],
        driver_beat_div_idx: &[i32],
        driver_waveform_idx: &[i32],
        driver_reversed: &[bool],
        driver_dotted: &[bool],
        driver_triplet: &[bool],
        driver_free_period: &[Option<f32>],
    ) {
        for i in 0..n {
            self.driver_expanded[i] = driver_active.get(i).copied().unwrap_or(false);
            self.envelope_expanded[i] = envelope_active.get(i).copied().unwrap_or(false);
            self.trim_min[i] = trim_min.get(i).copied().unwrap_or(0.0);
            self.trim_max[i] = trim_max.get(i).copied().unwrap_or(1.0);
            self.target_norm[i] = target_norm.get(i).copied().unwrap_or(1.0);
            self.env_decay[i] = env_decay.get(i).copied().unwrap_or(DEFAULT_ENV_DECAY);
            self.driver_beat_div_idx[i] = driver_beat_div_idx.get(i).copied().unwrap_or(-1);
            self.driver_waveform_idx[i] = driver_waveform_idx.get(i).copied().unwrap_or(-1);
            self.driver_reversed[i] = driver_reversed.get(i).copied().unwrap_or(false);
            self.driver_dotted[i] = driver_dotted.get(i).copied().unwrap_or(false);
            self.driver_triplet[i] = driver_triplet.get(i).copied().unwrap_or(false);
            self.driver_free_period[i] = driver_free_period.get(i).copied().flatten();
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

/// Drag tracking state for the unified `ParamCardPanel` (both kinds).
pub(crate) struct ParamDragState {
    pub(crate) dragging_param: i32,
    /// The active modulator trim-range drag, if any: `(kind, param_index,
    /// is_min)`. The three formerly parallel driver/Ableton/audio drag slots
    /// are one slot now — only one trim handle is ever dragged at a time, and
    /// [`TrimKind`] records which modulator's range it is.
    pub(crate) dragging_trim: Option<(TrimKind, usize, bool)>,
    /// The envelope target (orange handle / `target_normalized`) on the track.
    pub(crate) dragging_target_param: i32,
    /// The envelope decay slider (`decay_beats`) in the drawer.
    pub(crate) dragging_decay_param: i32,
    /// An audio shaping slider drag in the drawer: `(param_index, which scalar)`.
    pub(crate) dragging_audio_shape: Option<(usize, crate::panels::AudioShapeParam)>,
}

impl ParamDragState {
    pub(crate) fn new() -> Self {
        Self {
            dragging_param: -1,
            dragging_trim: None,
            dragging_target_param: -1,
            dragging_decay_param: -1,
            dragging_audio_shape: None,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging_param >= 0
            || self.dragging_trim.is_some()
            || self.dragging_target_param >= 0
            || self.dragging_decay_param >= 0
            || self.dragging_audio_shape.is_some()
    }
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
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    let fill_x = base_x + new_min * usable;
    let fill_w = (new_max - new_min) * usable;
    let fill_h = track_rect.height - OVERLAY_INSET * 2.0;
    tree.set_bounds(
        ids.fill_id,
        Rect::new(fill_x, track_rect.y + OVERLAY_INSET, fill_w, fill_h),
    );
    tree.set_bounds(
        ids.min_bar_id,
        Rect::new(
            base_x + new_min * usable - TRIM_BAR_W * 0.5,
            track_rect.y,
            TRIM_BAR_W,
            track_rect.height,
        ),
    );
    tree.set_bounds(
        ids.max_bar_id,
        Rect::new(
            base_x + new_max * usable - TRIM_BAR_W * 0.5,
            track_rect.y,
            TRIM_BAR_W,
            track_rect.height,
        ),
    );
}

// ── Shared helper functions ─────────────────────────────────────

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
    let dids = drawer::build(tree, parent, x, y, w, &spec);

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
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let norm = mod_state.target_norm.get(param_idx).copied().unwrap_or(0.5);
    let bar_x = track_rect.x + OVERLAY_INSET + norm * usable - TARGET_BAR_W * 0.5;
    let bar_h = track_rect.height + 4.0;
    let bar_y = track_rect.y - 2.0;

    let target_bar_id = tree.add_button(
        Some(track_parent),
        bar_x,
        bar_y,
        TARGET_BAR_W,
        bar_h,
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
) -> EnvelopeConfigIds {
    use crate::panels::drawer::{self, DrawerRow, DrawerSpec};

    let decay = mod_state
        .env_decay
        .get(param_idx)
        .copied()
        .unwrap_or(DEFAULT_ENV_DECAY);
    let spec = DrawerSpec {
        rows: vec![DrawerRow::Slider {
            label: "Decay".into(),
            norm: (decay / ENV_DECAY_MAX).clamp(0.0, 1.0),
            value_text: format!("{decay:.2}"),
            label_w: ENV_DECAY_LABEL_W,
        }],
        btn_font_size: FONT_SIZE,
        slider_font_size: FONT_SIZE,
        theme: Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32).tinted(),
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec);
    let decay_slider = dids
        .sliders
        .into_iter()
        .next()
        .expect("envelope drawer has one slider row");

    EnvelopeConfigIds {
        _container_id: dids.container,
        decay_slider,
    }
}

pub(crate) fn build_trim_handles(
    tree: &mut UITree,
    track_parent: NodeId,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> TrimHandleIds {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let tmin = mod_state.trim_min.get(param_idx).copied().unwrap_or(0.0);
    let tmax = mod_state.trim_max.get(param_idx).copied().unwrap_or(1.0);

    let fill_x = track_rect.x + OVERLAY_INSET + tmin * usable;
    let fill_w = (tmax - tmin) * usable;
    let fill_id = tree.add_panel(
        Some(track_parent),
        fill_x,
        track_rect.y + OVERLAY_INSET,
        fill_w,
        track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle {
            bg_color: color::TRIM_FILL_C32,
            ..UIStyle::default()
        },
    );

    let min_x = fill_x - TRIM_BAR_W * 0.5;
    let min_bar_id = tree.add_button(
        Some(track_parent),
        min_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    let max_x = track_rect.x + OVERLAY_INSET + tmax * usable - TRIM_BAR_W * 0.5;
    let max_bar_id = tree.add_button(
        Some(track_parent),
        max_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
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
    let usable = track_rect.width - OVERLAY_INSET * 2.0;

    let fill_x = track_rect.x + OVERLAY_INSET + min * usable;
    let fill_w = (max - min) * usable;
    let fill_id = tree.add_panel(
        Some(track_parent),
        fill_x,
        track_rect.y + OVERLAY_INSET,
        fill_w,
        track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle {
            bg_color: fill_color,
            ..UIStyle::default()
        },
    );

    let min_x = fill_x - TRIM_BAR_W * 0.5;
    let min_bar_id = tree.add_button(
        Some(track_parent),
        min_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: bar_color,
            hover_bg_color: bar_hover,
            corner_radius: color::HAIRLINE_RADIUS,
            ..UIStyle::default()
        },
        "",
    );

    let max_x = track_rect.x + OVERLAY_INSET + max * usable - TRIM_BAR_W * 0.5;
    let max_bar_id = tree.add_button(
        Some(track_parent),
        max_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
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

/// Result of checking a click against driver config buttons. The Free field is
/// *not* here — it opens a type-in (handled via [`driver_free_field_index`] on
/// the tree-aware click path), not a config command.
pub(crate) enum DriverClickResult {
    BeatDiv(usize),
    Straight,
    Dotted,
    Triplet,
    Invert,
    Wave(usize),
}

/// Check if a click hit any button in a driver config panel.
/// Returns `Some((param_index, result))` if matched.
pub(crate) fn check_driver_config_click(
    node_id: NodeId,
    driver_config_ids: &[Option<DriverConfigIds>],
) -> Option<(usize, DriverClickResult)> {
    for (pi, cfg) in driver_config_ids.iter().enumerate() {
        if let Some(c) = cfg {
            for (j, &bid) in c.beat_div_btn_ids.iter().enumerate() {
                if node_id == bid {
                    return Some((pi, DriverClickResult::BeatDiv(j)));
                }
            }
            if node_id == c.straight_btn_id {
                return Some((pi, DriverClickResult::Straight));
            }
            if node_id == c.dotted_btn_id {
                return Some((pi, DriverClickResult::Dotted));
            }
            if node_id == c.triplet_btn_id {
                return Some((pi, DriverClickResult::Triplet));
            }
            if node_id == c.invert_btn_id {
                return Some((pi, DriverClickResult::Invert));
            }
            for (j, &wid) in c.wave_btn_ids.iter().enumerate() {
                if node_id == wid {
                    return Some((pi, DriverClickResult::Wave(j)));
                }
            }
        }
    }
    None
}

/// If `node_id` is a driver drawer's Free-period field, return its param index.
/// The Free field opens a beats type-in (free mode) rather than issuing a config
/// command, so it's matched separately from [`check_driver_config_click`].
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
    let dids = drawer::build(tree, parent, x, y, w, &spec);
    let invert_btn_id = dids.button_ids()[0];

    AbletonConfigIds {
        _container_id: dids.container,
        invert_btn_id,
    }
}

/// Check if a click hit an Ableton config button. Returns param index if matched.
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
pub(crate) fn active_mod_tabs(mod_state: &ParamModState, info: &ParamInfo, i: usize) -> Vec<ModTab> {
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
    if info.ableton_display.is_some() {
        v.push(ModTab::Ableton);
    }
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
pub(crate) fn mod_config_height(tab: ModTab) -> f32 {
    match tab {
        ModTab::Envelope => ENV_CONFIG_HEIGHT,
        ModTab::Driver => driver_config_height(),
        ModTab::Audio => audio_config_height(),
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
// Per-row interactive-control roles, mixed into a row's key base to give each
// control a stable, reorder-proof WidgetId. Values only need to be unique within
// one row; the row base (`param_index << 8`) separates rows. Shared with the
// editor card, which keys its chevron / toggle the same way.
pub(crate) const ROW_ROLE_ENV: u64 = 1;
pub(crate) const ROW_ROLE_DRV: u64 = 2;
pub(crate) const ROW_ROLE_AUDIO: u64 = 3;
pub(crate) const ROW_ROLE_CHEVRON: u64 = 4;
pub(crate) const ROW_ROLE_TOGGLE: u64 = 5;

/// Add a row arm button: explicitly keyed (`base | role`) when a row key base is
/// supplied (editor card), else auto-salted by sibling index (perform inspector,
/// unchanged).
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

pub(crate) fn build_param_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    slider_w: f32,
    info: &ParamInfo,
    mod_state: &ParamModState,
    i: usize,
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
) -> ParamRowIds {
    let mut ids = ParamRowIds {
        // Overwritten with the real row-catcher node below before any read.
        row_catcher: NodeId::PLACEHOLDER,
        slider: None,
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

    let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
    let val_text = format_param_value(
        info.default,
        info.min,
        info.whole_numbers,
        info.is_angle,
        info.value_labels.as_deref(),
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
        // Extend up by MOD_CARD_TOP_PAD so the card covers the slider's trim /
        // target handles (they poke a couple px above the track), then keep the
        // same bottom by adding that pad into the height.
        let card_h = MOD_CARD_TOP_PAD + ROW_HEIGHT + ROW_SPACING + tab_strip_h + mod_config_height(tab);
        let card_w = (row_right - x).max(1.0);
        tree.add_panel(
            parent,
            x,
            cy - MOD_CARD_TOP_PAD,
            card_w,
            card_h,
            card_theme.surface_style(color::CARD_RADIUS),
        );
    }

    // Full-row hit catcher, added BEFORE the slider widgets so reverse-insertion
    // hit-testing lets the track/label win on top and the catcher only collects
    // the value cell + gaps. Transparent + interactive; carries no visual.
    ids.row_catcher = tree.add_node(
        parent,
        slider_rect,
        UINodeType::Panel,
        UIStyle::default(),
        None,
        UIFlags::VISIBLE | UIFlags::INTERACTIVE,
    );

    let slider = BitmapSlider::build(
        tree,
        parent,
        slider_rect,
        Some(&info.name),
        norm,
        &val_text,
        slider_colors,
        FONT_SIZE,
        label_width,
    );

    // Make label interactive for click-to-copy OSC address + Ableton mapping.
    if let Some(label_id) = slider.label {
        tree.set_flag(label_id, UIFlags::INTERACTIVE);
    }

    // Trim handles (if driver expanded).
    if mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
        ids.trim = Some(build_trim_handles(
            tree,
            slider.track,
            slider.track_rect,
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
            slider.track_rect,
            mod_state,
            i,
        ));
    }

    // Ableton trim handles (when the param has an Ableton mapping).
    if let Some((amin, amax)) = info.ableton_range {
        ids.ableton_trim = Some(build_trim_handles_explicit(
            tree,
            slider.track,
            slider.track_rect,
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
            slider.track_rect,
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
    let audio_active = mod_state.audio_active.get(i).copied().unwrap_or(false);
    ids.audio_btn = add_row_button(
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

    cy += ROW_HEIGHT + ROW_SPACING;

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
            build_mod_tab_strip(tree, parent, drawer_x, cy, drawer_w, &active_tabs, shown_tab);
        cy += MOD_TAB_STRIP_H;
    }

    // Envelope drawer — a single "Decay" slider. Depth is the orange target
    // handle on the track above; this is how fast the value falls back.
    if shown_tab == Some(ModTab::Envelope) {
        ids.envelope_config = Some(build_envelope_config(
            tree, parent, drawer_x, cy, drawer_w, mod_state, i,
        ));
        cy += ENV_CONFIG_HEIGHT;
    }

    // Driver config drawer.
    if shown_tab == Some(ModTab::Driver) {
        ids.driver_config = Some(build_driver_config(
            tree,
            parent,
            drawer_x,
            cy,
            drawer_w,
            mod_state,
            i,
            config_font,
        ));
        cy += driver_config_height();
    }

    // Ableton config drawer. ModTab::Ableton is only in the active set when a
    // mapping exists, so the let-binding always resolves here.
    if shown_tab == Some(ModTab::Ableton)
        && let Some(ref display) = info.ableton_display
    {
        ids.ableton_config = Some(build_ableton_config(tree, parent, drawer_x, cy, drawer_w, display));
        cy += ABL_CONFIG_HEIGHT;
    }

    // Audio-modulation drawer — shown when the Audio config tab is active. Two
    // rows: send selector (the project's sends — new sends are created in the
    // Audio Setup panel, not here) and feature selector. Built on the shared
    // drawer API.
    if shown_tab == Some(ModTab::Audio) {
        use crate::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};
        let send_sel = mod_state.audio_send_idx.get(i).copied().unwrap_or(-1);
        let send_count = mod_state.audio_send_labels.len();
        let send_buttons: Vec<DrawerButton> = mod_state
            .audio_send_labels
            .iter()
            .enumerate()
            .map(|(k, label)| {
                let btn = DrawerButton::new(label.clone(), k as i32 == send_sel);
                // Tint the label with the send's identity color so a driven
                // slider reads the same color as its source in the Audio Setup
                // panel — text-only, so the selected send shows the standard
                // highlight instead of a drawer-wide block of saturated color.
                match mod_state.audio_send_ids.get(k) {
                    Some(id) => btn.with_accent_text_only(crate::panels::audio_send_color(id)),
                    None => btn,
                }
            })
            .collect();
        let kind_sel = mod_state.audio_kind_idx.get(i).copied().unwrap_or(0);
        let band_sel = mod_state.audio_band_idx.get(i).copied().unwrap_or(0);
        let invert_on = mod_state.audio_invert.get(i).copied().unwrap_or(false);
        let rate_on = mod_state.audio_rate.get(i).copied().unwrap_or(false);
        // The feature matrix: a Feature row (kind) and a Band row, each a single
        // selection. Flat button indices run sends, then kinds (0..5), then bands
        // (5..9), then the two modifier toggles (9, 10) — see match_param_row_click.
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
        let shape_slider = |label: &str, norm: f32, value_text: String| DrawerRow::Slider {
            label: label.to_string(),
            norm: norm.clamp(0.0, 1.0),
            value_text,
            label_w: AUDIO_SHAPE_LABEL_W,
        };
        // Modifier toggles below the band row: "Inv" (loud → low) then "Delta"
        // (drive on motion). Flat indices sit one and two past the bands.
        let toggle_buttons =
            vec![DrawerButton::new("Inv", invert_on), DrawerButton::new("Delta", rate_on)];
        let spec = DrawerSpec {
            rows: vec![
                DrawerRow::Buttons {
                    buttons: send_buttons,
                    width: ButtonWidth::Proportional,
                    label: Some("Source".into()),
                },
                DrawerRow::Buttons {
                    buttons: kind_buttons,
                    width: ButtonWidth::Uniform,
                    label: Some("Feature".into()),
                },
                DrawerRow::Buttons {
                    buttons: band_buttons,
                    width: ButtonWidth::Uniform,
                    label: Some("Band".into()),
                },
                DrawerRow::Buttons {
                    buttons: toggle_buttons,
                    width: ButtonWidth::Proportional,
                    label: None,
                },
                shape_slider("Amount", sens / AUDIO_SENS_MAX, format!("{sens:.2}")),
                shape_slider("Attack", attack / AUDIO_ATTACK_MAX_MS, format!("{attack:.0} ms")),
                shape_slider("Release", release / AUDIO_RELEASE_MAX_MS, format!("{release:.0} ms")),
            ],
            btn_font_size: config_font,
            slider_font_size: FONT_SIZE,
            theme: Theme::INSPECTOR.with_accent(AUDIO_MOD_ACTIVE_C32).tinted(),
        };
        let dids = drawer::build(tree, parent, drawer_x, cy, drawer_w, &spec);
        cy += dids.height;
        ids.audio_config = Some((dids, send_count));
    }

    // Clear break after an expanded drawer so the next slider reads as a separate
    // row (the slider above hugs its own drawer). Mirrored in `row_drawer_height`.
    if !active_tabs.is_empty() {
        cy += DRAWER_BOTTOM_GAP;
    }

    ids.new_cy = cy;
    ids
}

// ── Shared per-parameter click dispatch ─────────────────────────────

/// A click on one of a parameter row's interactive elements, abstracted away
/// from the effect-vs-generator [`PanelAction`] vocabulary. Each panel maps
/// these to its own kind-specific actions (e.g. `EffectDriverToggle(ei, …)`
/// vs `GenDriverToggle(…)`).
pub(crate) enum RowClick {
    /// The row's `→` driver toggle button (param index).
    DriverToggle(usize),
    /// The row's `E` envelope toggle button (param index).
    EnvelopeToggle(usize),
    /// A button inside the driver-config drawer (param index + action).
    DriverConfig(usize, DriverConfigAction),
    /// The Ableton-config invert button (param index).
    AbletonInvert(usize),
    /// The "A" audio-modulation button (param index) — arm/disarm.
    AudioToggle(usize),
    /// A send button in the audio drawer (param index, send index).
    AudioSelectSend(usize, usize),
    /// A feature-kind button in the audio drawer (param index, kind index).
    AudioSelectKind(usize, usize),
    /// A band button in the audio drawer (param index, band index).
    AudioSelectBand(usize, usize),
    /// The "Inv" invert toggle in the audio drawer (param index).
    AudioToggleInvert(usize),
    /// The "d/dt" rate-of-change toggle in the audio drawer (param index).
    AudioToggleRate(usize),
    /// The slider's param label, when it carries an OSC address to copy
    /// (param index). The caller performs the copied-flash side effect and
    /// reads `osc_addresses[pi]`.
    LabelCopy(usize),
}

/// Match a clicked node id against a parameter row's interactive elements,
/// shared by the effect and generator cards' `handle_click`. Returns the
/// abstract [`RowClick`] for the caller to map to a kind-specific action, or
/// `None` if `id` hits nothing in the per-param row surface (the caller then
/// checks its own shell-specific elements — header buttons, toggle/string
/// rows, card selection).
///
/// Driver/envelope toggle buttons on toggle/trigger params are skipped (they
/// carry no slider to modulate). Effects have no toggle/trigger params, so the
/// skip is a no-op there — behavior is identical to the prior per-panel code.
#[allow(clippy::too_many_arguments)]
pub(crate) fn match_param_row_click(
    id: NodeId,
    driver_btn_ids: &[Option<NodeId>],
    envelope_btn_ids: &[Option<NodeId>],
    driver_config_ids: &[Option<DriverConfigIds>],
    ableton_config_ids: &[Option<AbletonConfigIds>],
    audio_btn_ids: &[Option<NodeId>],
    audio_configs: &[Option<(crate::panels::drawer::DrawerIds, usize)>],
    slider_ids: &[Option<SliderNodeIds>],
    osc_addresses: &[Option<String>],
    param_info: &[ParamInfo],
) -> Option<RowClick> {
    let is_unmodulatable = |pi: usize| {
        param_info
            .get(pi)
            .map(|p| p.is_toggle || p.is_trigger)
            .unwrap_or(false)
    };

    // D/E buttons (skip toggle/trigger params).
    for (pi, &btn_id) in driver_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if btn_id == Some(id) {
            return Some(RowClick::DriverToggle(pi));
        }
    }
    for (pi, &btn_id) in envelope_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if btn_id == Some(id) {
            return Some(RowClick::EnvelopeToggle(pi));
        }
    }

    // Driver config drawer buttons (the Free field is handled separately, on the
    // tree-aware type-in path).
    if let Some((pi, result)) = check_driver_config_click(id, driver_config_ids) {
        let action = match result {
            DriverClickResult::BeatDiv(j) => DriverConfigAction::BeatDiv(j),
            DriverClickResult::Straight => DriverConfigAction::Straight,
            DriverClickResult::Dotted => DriverConfigAction::Dotted,
            DriverClickResult::Triplet => DriverConfigAction::Triplet,
            DriverClickResult::Invert => DriverConfigAction::Invert,
            DriverClickResult::Wave(j) => DriverConfigAction::Wave(j),
        };
        return Some(RowClick::DriverConfig(pi, action));
    }

    // Ableton config invert button.
    if let Some((pi, AbletonConfigClick::Invert)) =
        check_ableton_config_click(id, ableton_config_ids)
    {
        return Some(RowClick::AbletonInvert(pi));
    }

    // Audio "A" buttons (skip toggle/trigger params).
    for (pi, &btn_id) in audio_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if btn_id == Some(id) {
            return Some(RowClick::AudioToggle(pi));
        }
    }

    // Audio drawer buttons: one flat index across rows in build order — sends,
    // then the Feature (kind) row, the Band row, then the two modifier toggles.
    for (pi, cfg) in audio_configs.iter().enumerate() {
        if let Some((dids, send_count)) = cfg
            && let Some(flat) = dids.resolve_button(id)
        {
            if flat < *send_count {
                return Some(RowClick::AudioSelectSend(pi, flat));
            }
            let f = flat - send_count;
            return Some(if f < AUDIO_KIND_COUNT {
                RowClick::AudioSelectKind(pi, f)
            } else if f < AUDIO_KIND_COUNT + AUDIO_BAND_COUNT {
                RowClick::AudioSelectBand(pi, f - AUDIO_KIND_COUNT)
            } else if f == AUDIO_KIND_COUNT + AUDIO_BAND_COUNT {
                RowClick::AudioToggleInvert(pi)
            } else {
                RowClick::AudioToggleRate(pi)
            });
        }
    }

    // Slider label → copy OSC address (only when one exists for this slot).
    for (pi, slider) in slider_ids.iter().enumerate() {
        if let Some(ids) = slider
            && ids.label == Some(id)
            && osc_addresses.get(pi).and_then(|a| a.as_ref()).is_some()
        {
            return Some(RowClick::LabelCopy(pi));
        }
    }

    None
}
