//! Shared modulation/drag state types and id-bundle structs for parameter slider rows.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;


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

