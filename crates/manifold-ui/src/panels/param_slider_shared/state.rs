//! Shared modulation/drag state types and id-bundle structs for parameter slider rows.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;

// ── Shared node ID structs ──────────────────────────────────────

/// The orange envelope target handle on a parameter's slider track — sets the
/// depth (`target_normalized`) the envelope pulls the value toward, shown in the
/// parameter's own range.
pub(crate) struct EnvelopeTargetIds {
    pub(crate) target_bar_id: NodeId,
}

/// The envelope drawer — for non-toggle/non-trigger params an Action row,
/// the Decay slider only in Continuous (Step/Random has no decay to tune),
/// and while Step the Amount slider + Wrap row.
pub(crate) struct EnvelopeConfigIds {
    pub(crate) drawer: crate::panels::drawer::DrawerIds,
    /// The Decay slider — `Some` only while Action=Continuous (or on
    /// toggle/trigger rows, which are always Continuous).
    pub(crate) decay_slider: Option<SliderNodeIds>,
    /// Right-click reset for the Decay slider (the `EnvDecay*` trio).
    pub(crate) decay_reset: Option<PanelAction>,
    /// Amount slider for the Step action, only while Action=Step.
    pub(crate) step_slider: Option<SliderNodeIds>,
    /// Right-click reset for the Amount slider.
    pub(crate) step_reset: Option<PanelAction>,
}

#[derive(Clone, Copy)]
pub(crate) struct TrimHandleIds {
    pub(crate) fill_id: NodeId,
    pub(crate) min_bar_id: NodeId,
    pub(crate) max_bar_id: NodeId,
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
    /// Per-param: envelope action index into `[Continuous, Step, Random]`
    /// (D8), read off `ParamEnvelope.action`. The T drawer shows the Action row
    /// (and Amount/Wrap while Step) from this.
    pub envelope_action_idx: Vec<i32>,
    /// Per-param: envelope Step action's `amount` (signed, param units).
    pub envelope_step_amount: Vec<f32>,
    /// Per-param: envelope Step action's wrap-mode index into `[Wrap, Bounce,
    /// Clamp]`.
    pub envelope_wrap_idx: Vec<i32>,
    pub driver_beat_div_idx: Vec<i32>,
    pub driver_waveform_idx: Vec<i32>,
    pub driver_reversed: Vec<bool>,
    pub driver_dotted: Vec<bool>,
    pub driver_triplet: Vec<bool>,
    /// Per-param: free-running LFO period in beats when the driver is in **free
    /// mode** (`Some`), else `None` for sync mode (grid/feel). Drives the Free
    /// field's label + highlight and the type-in prefill.
    pub driver_free_period: Vec<Option<f32>>,
    pub driver_frame_aligned: Vec<bool>,
    pub driver_frame_rate: Vec<Option<(u32, f32)>>,

    // ── Audio modulation (per-row + card-level send list) ──
    /// Per-row audio facts. Keeping the send id with the other row facts makes
    /// filtering/reordering safe by construction.
    pub audio_rows: Vec<AudioRowState>,
    /// Card-level available sends, with labels and ids kept paired.
    pub audio_sends: Vec<AudioSendChoice>,

    // Trigger mode, action, amount, and wrap now live in `audio_rows`.
    /// Per-param: the drawer's full Feature×Band matrix is open (the "Custom"
    /// cell trailing the Listen chips). SESSION-ONLY UI state — `sync_audio`
    /// never writes it; it mirrors no model field.
    pub audio_matrix_open: Vec<bool>,

    // ── Automation lane indicator (P4 section 7 last bullet) ──
    /// Per-param: an enabled automation lane with ≥1 point exists on this
    /// instance for this param (Live's red "automated" dot).
    pub automation_active: Vec<bool>,
    /// Per-param: that lane's `(EffectId, ParamId)` is currently latched in
    /// `ContentState::automation_latched_params` — the dot grays instead of
    /// showing red, mirroring the lane-strip / transport BACK button.
    pub automation_overridden: Vec<bool>,
    pub automation_selected: Vec<bool>,
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

    #[test]
    fn selected_send_index_follows_id_when_choices_reorder() {
        let mut state = ParamModState::allocate(1);
        state.audio_rows[0].send_id = Some(manifold_foundation::AudioSendId::new("second"));
        let first = AudioSendChoice {
            id: manifold_foundation::AudioSendId::new("first"),
            label: "First".into(),
        };
        let second = AudioSendChoice {
            id: manifold_foundation::AudioSendId::new("second"),
            label: "Second".into(),
        };
        state.sync_audio(
            std::iter::once(state.audio_rows[0].clone()),
            &[first.clone(), second.clone()],
        );
        assert_eq!(state.audio_send_index(0), 1);
        state.sync_audio(
            std::iter::once(state.audio_rows[0].clone()),
            &[second, first],
        );
        assert_eq!(state.audio_send_index(0), 0);
        assert_eq!(state.audio_sends[0].id.as_str(), "second");
    }

    /// P4 regression (2026-07-06, found by Peter on a live build): the UI
    /// crate holds a MIRROR of core's `AudioFeatureKind` behind the
    /// translation boundary, and P4 initially extended only core — the
    /// drawer stayed at five buttons while serde/runtime shipped. This pins
    /// the row that actually feeds pixels.
    #[test]
    fn feature_row_carries_kick_snare_pitch_and_presence() {
        let labels = audio_kind_labels();
        assert_eq!(labels.len(), 9);
        // Kick and Snare follow Transients (index 4), shifting Pitch/Presence.
        assert_eq!(labels[5], "Kick");
        assert_eq!(labels[6], "Snare");
        assert_eq!(labels[7], "Pitch");
        assert_eq!(labels[8], "Presence");
        assert_eq!(AUDIO_KIND_COUNT, 9);
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
pub(crate) const TRIGGER_SOURCE_CHIPS: [(
    &str,
    crate::types::AudioFeatureKind,
    crate::types::AudioBand,
); 5] = [
    (
        "Kick",
        crate::types::AudioFeatureKind::Kick,
        crate::types::AudioBand::Low,
    ),
    (
        "Bass",
        crate::types::AudioFeatureKind::Transients,
        crate::types::AudioBand::Low,
    ),
    (
        "Snare",
        crate::types::AudioFeatureKind::Snare,
        crate::types::AudioBand::Mid,
    ),
    (
        "Hats",
        crate::types::AudioFeatureKind::Transients,
        crate::types::AudioBand::High,
    ),
    (
        "Transients",
        crate::types::AudioFeatureKind::Transients,
        crate::types::AudioBand::Full,
    ),
];

/// The chips a clip-trigger drawer shows for `current`: the curated five with
/// the active one highlighted — plus, when the current cell isn't one of the
/// five (an older project pointing at e.g. Flux×Mid, or a future classifier
/// class surfaced through the param-mod drawer's full matrix), a truthful
/// trailing chip naming the actual cell, so the drawer never silently
/// re-points a trigger at a different signal than the one it fires from.
pub(crate) fn trigger_source_chips(current: crate::types::AudioFeature) -> Vec<SourceChip> {
    let mut chips: Vec<SourceChip> = TRIGGER_SOURCE_CHIPS
        .iter()
        .map(|&(label, kind, band)| {
            // `Kick` and `Snare` ignore `band` at evaluation time (always Low / Mid), so
            // a saved Kick or Snare cell matches its chip regardless of the stored band.
            let active = kind == current.kind
                && (band == current.band
                    || matches!(
                        kind,
                        crate::types::AudioFeatureKind::Kick
                            | crate::types::AudioFeatureKind::Snare
                    ));
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

/// One param row's audio-modulation display state. Collapses the former fifteen parallel
/// per-param vecs into one struct per
/// row.
#[derive(Debug, Clone)]
pub struct AudioRowState {
    /// Mod exists and is enabled.
    pub active: bool,
    /// The mod's send id, if any. Resolved against paired send choices by
    /// [`ParamModState::audio_send_index`].
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
    /// Fire-mode index (`ParameterAudioMod.trigger_mode`, section 9 U3), into
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

/// The live scalars one row's drawers and handles show (UI_INVALIDATION D22):
/// Shapes and MappingRanges values of modulators that already exist. The live
/// push writes them in place; a `None` group means the row has no such
/// modulator, which is membership and reaches the row through a rebuild.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RowShapes {
    /// The enabled driver's trim range.
    pub driver_trim: Option<(f32, f32)>,
    pub envelope: Option<EnvelopeShape>,
    pub audio: Option<AudioShape>,
    /// The Ableton mapping's trim range.
    pub ableton_range: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnvelopeShape {
    pub target_norm: f32,
    pub decay: f32,
    /// The Step action's amount; `None` unless the action is Step.
    pub step_amount: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioShape {
    pub range_min: f32,
    pub range_max: f32,
    pub sensitivity: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub invert: bool,
    /// The Step action's amount; `None` unless the action is Step.
    pub step_amount: Option<f32>,
}

/// One available audio send. Labels and ids travel together so send reorder
/// cannot route a click to a different id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSendChoice {
    pub id: manifold_foundation::AudioSendId,
    pub label: String,
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
            sensitivity: AUDIO_SENS_DEFAULT,
            attack_ms: AUDIO_ATTACK_DEFAULT_MS,
            release_ms: AUDIO_RELEASE_DEFAULT_MS,
            trigger_mode_idx: 0,
            action_idx: 0,
            step_amount: 1.0,
            wrap_idx: 0,
        }
    }
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
            envelope_action_idx: vec![0; param_count],
            envelope_step_amount: vec![1.0; param_count],
            envelope_wrap_idx: vec![0; param_count],
            driver_beat_div_idx: vec![-1; param_count],
            driver_waveform_idx: vec![-1; param_count],
            driver_reversed: vec![false; param_count],
            driver_dotted: vec![false; param_count],
            driver_triplet: vec![false; param_count],
            driver_free_period: vec![None; param_count],
            driver_frame_aligned: vec![false; param_count],
            driver_frame_rate: vec![None; param_count],
            audio_rows: vec![AudioRowState::default(); param_count],
            audio_sends: Vec::new(),
            audio_matrix_open: vec![false; param_count],
            automation_active: vec![false; param_count],
            automation_overridden: vec![false; param_count],
            automation_selected: vec![false; param_count],
        }
    }

    /// Per-frame rebuild for `param_count` rows: every model-mirrored field
    /// resets (the caller re-syncs them), session-only UI state survives.
    /// Hosts call this, never `allocate`, so a new session-only field is
    /// carried here once instead of by each host.
    pub fn rebuild(&mut self, param_count: usize) {
        let mut matrix_open = std::mem::take(&mut self.audio_matrix_open);
        matrix_open.resize(param_count, false);
        *self = Self::allocate(param_count);
        self.audio_matrix_open = matrix_open;
    }

    /// Sync audio-modulation display state from the retained row stream.
    pub fn sync_audio(
        &mut self,
        rows: impl ExactSizeIterator<Item = AudioRowState>,
        sends: &[AudioSendChoice],
    ) {
        // Session-only UI state: sized here so a card whose param list grew
        // since `allocate` never has a dead "Custom" toggle. Never overwritten
        // from the model — it's not a mirrored field.
        let n = rows.len();
        assert_eq!(
            n,
            self.driver_expanded.len(),
            "audio row count must match allocated parameter state"
        );
        self.audio_rows.clear();
        self.audio_rows.extend(rows);
        self.audio_sends.clear();
        self.audio_sends.extend_from_slice(sends);
        self.audio_matrix_open.resize(n, false);
    }

    /// Index of the selected send for a row, or -1 when it no longer resolves.
    pub fn audio_send_index(&self, index: usize) -> i32 {
        self.audio_rows
            .get(index)
            .and_then(|row| row.send_id.as_ref())
            .and_then(|id| self.audio_sends.iter().position(|send| &send.id == id))
            .map_or(-1, |index| index as i32)
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
            self.envelope_action_idx[i] = row.envelope_action_idx;
            self.envelope_step_amount[i] = row.envelope_step_amount;
            self.envelope_wrap_idx[i] = row.envelope_wrap_idx;
            self.driver_beat_div_idx[i] = row.driver_beat_div_idx;
            self.driver_waveform_idx[i] = row.driver_waveform_idx;
            self.driver_reversed[i] = row.driver_reversed;
            self.driver_dotted[i] = row.driver_dotted;
            self.driver_triplet[i] = row.driver_triplet;
            self.driver_free_period[i] = row.driver_free_period;
            self.driver_frame_aligned[i] = row.driver_frame_aligned;
            self.driver_frame_rate[i] = row.driver_frame_rate;
            self.automation_active[i] = row.automation_active;
            self.automation_overridden[i] = row.automation_overridden;
            self.automation_selected[i] = row.automation_selected;
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

pub fn format_param_value(
    val: f32,
    min: f32,
    whole_numbers: bool,
    is_angle: bool,
    value_labels: Option<&[String]>,
    unit: Option<manifold_foundation::scene_surface::Unit>,
) -> String {
    let mut buf = ParamValueText::default();
    param_value_text(
        &mut buf,
        val,
        min,
        whole_numbers,
        is_angle,
        value_labels,
        unit,
    )
    .to_owned()
}

/// A formatted number as a param readout shows it: long enough for any `f32`
/// with two decimals and a unit suffix.
pub type ParamValueText = manifold_foundation::scratch::StackText<64>;

/// [`format_param_value`] without allocating: a value label is borrowed, a
/// number is written into `buf`.
pub fn param_value_text<'a>(
    buf: &'a mut ParamValueText,
    val: f32,
    min: f32,
    whole_numbers: bool,
    is_angle: bool,
    value_labels: Option<&'a [String]>,
    unit: Option<manifold_foundation::scene_surface::Unit>,
) -> &'a str {
    if let Some(labels) = value_labels {
        return &labels[label_index(val, min, labels.len())];
    }
    write_value(
        buf,
        val,
        whole_numbers,
        is_angle,
        unit.map_or("", |unit| unit.suffix()),
        2,
    );
    buf.as_str()
}

/// `val` with `decimals` places and the unit text `suffix`.
fn write_value(
    buf: &mut ParamValueText,
    val: f32,
    whole_numbers: bool,
    is_angle: bool,
    suffix: &str,
    decimals: usize,
) {
    use std::fmt::Write;
    buf.clear();
    let _ = if is_angle {
        // `val` is radians; the user always sees and edits degrees.
        write!(buf, "{:.0}°", val.to_degrees())
    } else if whole_numbers {
        write!(buf, "{}{suffix}", val.round() as i32)
    } else {
        write!(buf, "{val:.decimals$}{suffix}")
    };
}

/// Room for text inside a card slider's value box.
pub(crate) const CARD_VALUE_TEXT_W: f32 = crate::slider::VALUE_BOX_W - 4.0;

thread_local! {
    /// The (font size, measurer) whose number glyphs are already measured.
    static GLYPHS_WARM: std::cell::Cell<(u16, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Width of a value string as the sum of its digits, point and sign (each
/// glyph measured once, then served from the measurer's cache) plus its unit
/// tail measured whole. A dragged value changes its text every frame; measuring
/// the whole string would miss the cache every frame.
fn value_text_width(measure: &dyn crate::text::TextMeasure, text: &str, font_size: u16) -> f32 {
    let weight = crate::color::FONT_WEIGHT_DEFAULT;
    let measurer_key = (
        font_size,
        measure as *const dyn crate::text::TextMeasure as *const () as usize,
    );
    if GLYPHS_WARM.get() != measurer_key {
        // All number glyphs at once, so a drag never meets a new digit.
        let mut glyph = [0u8; 4];
        for ch in "0123456789.-+".chars() {
            measure.measure_text(ch.encode_utf8(&mut glyph), font_size, weight);
        }
        GLYPHS_WARM.set(measurer_key);
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .unwrap_or(text.len());
    let (number, tail) = text.split_at(split);
    let mut glyph = [0u8; 4];
    let mut width = 0.0;
    for ch in number.chars() {
        width += measure
            .measure_text(ch.encode_utf8(&mut glyph), font_size, weight)
            .x;
    }
    if !tail.is_empty() {
        width += measure.measure_text(tail, font_size, weight).x;
    }
    width
}

/// [`param_value_text`] that fits `max_width` as `measure` measures it, so a
/// value box never clips a digit. The unit goes first: it shortens ("4.00m"),
/// then drops. Only then does precision go (fewer decimals, then none), and
/// last a compact thousands form. A value label is returned whole (its box
/// elides it).
#[allow(clippy::too_many_arguments)]
pub fn param_value_text_fit<'a>(
    buf: &'a mut ParamValueText,
    val: f32,
    min: f32,
    whole_numbers: bool,
    is_angle: bool,
    value_labels: Option<&'a [String]>,
    unit: Option<manifold_foundation::scene_surface::Unit>,
    measure: &dyn crate::text::TextMeasure,
    font_size: u16,
    max_width: f32,
) -> &'a str {
    use std::fmt::Write;
    if let Some(labels) = value_labels {
        return &labels[label_index(val, min, labels.len())];
    }
    let fits = |text: &str| value_text_width(measure, text, font_size) <= max_width;
    for suffix in [
        unit.map_or("", |unit| unit.suffix()),
        unit.map_or("", |unit| unit.short_suffix()),
        "",
    ] {
        write_value(buf, val, whole_numbers, is_angle, suffix, 2);
        if fits(buf.as_str()) {
            return buf.as_str();
        }
    }
    for decimals in [1, 0] {
        write_value(buf, val, whole_numbers, is_angle, "", decimals);
        if fits(buf.as_str()) {
            return buf.as_str();
        }
    }
    if !is_angle && val.abs() >= 1000.0 {
        buf.clear();
        let _ = if val.abs() >= 1.0e6 {
            write!(buf, "{:.1}M", val / 1.0e6)
        } else {
            write!(buf, "{:.1}k", val / 1.0e3)
        };
    }
    buf.as_str()
}

/// Shared Shift-fine scaling for a param value scrub (the D8 fine-drag
/// contract): holding Shift multiplies the pointer-to-value sensitivity by
/// 0.1. One function so the effect/generator card's `handle_drag` and the
/// scene panel's track scrub share the same sensitivity — the
/// "one implementation" the param surface is for.
///
/// `base` is the value at drag-start (the grab), `dx` the pointer travel in
/// px since grab. A drag converts `dx` px to `dx * range / track_width` value
/// units at full sensitivity (mathematically identical to absolute-position
/// tracking for a non-fine drag, since `base` is the grab value); the fine
/// factor scales that to a tenth. `[min, max]` is the slider range, which sets
/// the speed only: the value carries on past the slider's end and stops at
/// `limit` (SCENE_SURFACES_DESIGN.md D15 (slider range and hard limit)).
pub(crate) fn fine_scrub_value(
    base: f32,
    dx: f32,
    track_width: f32,
    (min, max): (f32, f32),
    limit: manifold_foundation::param_range::HardLimit,
    fine: bool,
) -> f32 {
    let sensitivity = if fine { 0.1 } else { 1.0 };
    let mut value = base;
    if track_width > 0.0 {
        value += dx * (max - min) / track_width * sensitivity;
    }
    limit.clamp(value, (min, max))
}

/// True when `value` sits past either end of the row's slider range, so the
/// thumb is pinned at the end and marked.
pub(crate) fn past_slider_end(value: f32, spec: &crate::param_surface::RowSpec) -> bool {
    value < spec.min.min(spec.max) || value > spec.max.max(spec.min)
}

/// Put a row's hover text on `nodes`: its manifest tooltip, then the reason it
/// does nothing right now (`inactive_reason`), so a disabled row explains
/// itself without standing text.
pub(crate) fn set_row_hover(
    tree: &mut UITree,
    nodes: impl IntoIterator<Item = NodeId>,
    info: &ParamRow,
) {
    let hover = match (
        info.spec.tooltip.as_deref(),
        info.spec.inactive_reason.as_deref(),
    ) {
        (Some(tip), Some(reason)) => Some(format!("{tip}\n{reason}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_owned()),
        (None, None) => None,
    };
    for node in nodes {
        if let Some(node) = tree.get_node_mut(node) {
            node.tooltip.clone_from(&hover);
        }
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
    pub(crate) driver_config: Option<crate::panels::drawer::DrawerIds>,
    pub(crate) ableton_config: Option<crate::panels::drawer::DrawerIds>,
    /// Audio-modulation drawer (send + feature selectors) and its send count,
    /// kept so click resolution can split the flat button index into
    /// send / new-send / feature regions.
    pub(crate) audio_config: Option<(crate::panels::drawer::DrawerIds, usize)>,
    /// Modulation-config tab strip node ids (paired with their `ModTab`). Empty
    /// when fewer than two configs are active (no strip drawn). The caller stores
    /// these to route tab clicks to the active-tab switch.
    pub(crate) mod_tabs: Vec<(NodeId, ModTab)>,
    /// The colour swatch in the label lane, when the row has `rgb_members`.
    pub(crate) swatch: Option<(NodeId, crate::param_surface::ColourGroup)>,
    /// The clip the drawer reveals through, when the row was handed a reveal
    /// height and has a drawer. The card moves it in place as the drawer
    /// tween runs.
    pub(crate) drawer_clip: Option<NodeId>,
    /// `y` after this row's slider + its modulation config drawer — the caller
    /// continues the next row from here.
    pub(crate) new_cy: f32,
}

/// A button row's node ids (button + its label) and the kind of button it
/// was built as, which the click route and the value sync read.
pub(crate) struct ToggleParamIds {
    pub(crate) label_id: Option<NodeId>,
    pub(crate) button_id: NodeId,
    pub(crate) kind: crate::param_surface::RowKind,
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

/// Node IDs produced by [`build_button_row`].
pub(crate) struct ToggleTriggerRowIds {
    pub(crate) label_id: Option<NodeId>,
    pub(crate) button_id: NodeId,
    pub(crate) kind: crate::param_surface::RowKind,
    /// A pick list's T and D lane buttons; `None` on every other button row.
    pub(crate) envelope_btn: Option<NodeId>,
    pub(crate) driver_btn: Option<NodeId>,
    /// The "A" audio-mod button: `Some` for `is_trigger` (D5b),
    /// `is_trigger_gate` (section 9) and pick-list rows. Plain toggles never
    /// build one (`None`, zero lane reserved).
    pub(crate) audio_btn: Option<NodeId>,
    /// The audio-mod drawer, when armed. Same shape as a slider row's
    /// `audio_config` so retained drawer intents resolve both identically.
    pub(crate) audio_config: Option<(crate::panels::drawer::DrawerIds, usize)>,
    /// Collapsed-row mode indicator (section 9 consequence, carried over from section 8 D6:
    /// "Transient mode silently ignores clip launches... the drawer must
    /// show the mode on the collapsed card row"). `Some` only for
    /// `is_trigger_gate` rows; text is set (or left blank for the default
    /// `ClipEdge` mode) by the caller from the live `mod_state` — see
    /// `build_button_row`.
    pub(crate) mode_badge_id: Option<NodeId>,
    /// The audio drawer's reveal clip, as [`ParamRowIds::drawer_clip`].
    pub(crate) drawer_clip: Option<NodeId>,
    pub(crate) new_cy: f32,
}

impl ToggleTriggerRowIds {
    /// The ids the row host keeps for this row.
    pub(crate) fn button_ids(&self) -> ToggleParamIds {
        ToggleParamIds {
            label_id: self.label_id,
            button_id: self.button_id,
            kind: self.kind,
        }
    }
}

/// Which of `count` value labels `value` shows: index = value - min. The one
/// encoding the readout, the pick list and the dropdown share.
pub(crate) fn label_index(value: f32, min: f32, count: usize) -> usize {
    ((value - min).round() as i32).clamp(0, count as i32 - 1) as usize
}

/// The label a pick list shows for `value`. `None` for a row without labels.
/// An empty label is a value with nothing to pick (a deleted camera's cut
/// number): it shows the first choice, which is what that value gives.
pub(crate) fn choice_label(spec: &crate::param_surface::RowSpec, value: f32) -> Option<&str> {
    let labels = spec
        .value_labels
        .as_deref()
        .filter(|labels| !labels.is_empty())?;
    let label = &labels[label_index(value, spec.min, labels.len())];
    Some(if label.is_empty() {
        labels
            .iter()
            .find(|label| !label.is_empty())
            .unwrap_or(label)
    } else {
        label
    })
}

/// A switch's button text.
pub(crate) fn switch_text(on: bool) -> &'static str {
    if on { "ON" } else { "OFF" }
}

#[cfg(test)]
mod fine_scrub_tests {
    use super::*;
    use manifold_foundation::param_range::HardLimit::Slider;

    /// D8: the pure sensitivity contract — 1 px of pointer travel maps to
    /// `range / track_width` value units, a tenth of that under Shift (fine).
    /// A 200-unit range over a 200 px track = 1 unit/px, so the numbers read
    /// directly.
    #[test]
    fn shift_fine_scales_sensitivity_by_a_tenth() {
        // Full sensitivity: 10 px of travel over a 200 px track on a 200-unit
        // range = 10 units.
        let coarse = fine_scrub_value(50.0, 10.0, 200.0, (0.0, 200.0), Slider, false);
        assert!((coarse - 60.0).abs() < 1e-4, "coarse: {coarse}");
        // Shift (fine): the same 10 px is a tenth — 1 unit.
        let fine = fine_scrub_value(50.0, 10.0, 200.0, (0.0, 200.0), Slider, true);
        assert!((fine - 51.0).abs() < 1e-4, "fine: {fine}");
    }

    /// The scene-panel pain case: ±100 world units on translate. A drag far
    /// past the track edge (2000 px → 200 units at 0.1x) must clamp to the
    /// param's own range, not the raw delta.
    #[test]
    fn fine_scrub_clamps_to_the_param_range() {
        let clamped = fine_scrub_value(0.0, 2000.0, 200.0, (-100.0, 100.0), Slider, true);
        assert!((clamped - 100.0).abs() < 1e-4, "clamped: {clamped}");
        let clamped_neg = fine_scrub_value(0.0, -2000.0, 200.0, (-100.0, 100.0), Slider, true);
        assert!(
            (clamped_neg + 100.0).abs() < 1e-4,
            "clamped neg: {clamped_neg}"
        );
    }

    /// INV-S18: past the slider's end a drag keeps going to the hard limit.
    #[test]
    fn drag_past_slider_end_reaches_limit() {
        let past = fine_scrub_value(
            0.0,
            400.0,
            200.0,
            (-1.0, 1.0),
            manifold_foundation::param_range::HardLimit::Between(-5.0, 5.0),
            false,
        );
        assert!((past - 4.0).abs() < 1e-4, "past the end: {past}");
        let stopped = fine_scrub_value(
            0.0,
            4000.0,
            200.0,
            (-1.0, 1.0),
            manifold_foundation::param_range::HardLimit::Between(-5.0, 5.0),
            false,
        );
        assert!(
            (stopped - 5.0).abs() < 1e-4,
            "stops at the limit: {stopped}"
        );
        let free = fine_scrub_value(
            0.0,
            4000.0,
            200.0,
            (-1.0, 1.0),
            manifold_foundation::param_range::HardLimit::Unlimited,
            false,
        );
        assert!((free - 40.0).abs() < 1e-3, "unlimited: {free}");
    }

    /// A zero-width track (a degenerate layout) must not divide by zero — the
    /// value stays at the grab, regardless of the fine flag.
    #[test]
    fn fine_scrub_zero_width_track_is_a_no_op() {
        assert_eq!(
            fine_scrub_value(25.0, 40.0, 0.0, (0.0, 100.0), Slider, false),
            25.0
        );
        assert_eq!(
            fine_scrub_value(25.0, 40.0, 0.0, (0.0, 100.0), Slider, true),
            25.0
        );
    }
}

#[cfg(test)]
mod value_fit_tests {
    use super::*;
    use manifold_foundation::scene_surface::Unit;

    fn fit(val: f32, whole: bool, unit: Option<Unit>) -> String {
        let tree = crate::tree::UITree::new();
        let mut buf = ParamValueText::default();
        param_value_text_fit(
            &mut buf,
            val,
            0.0,
            whole,
            false,
            None,
            unit,
            tree.measurer(),
            FONT_SIZE,
            CARD_VALUE_TEXT_W,
        )
        .to_owned()
    }

    /// The unit shortens, then drops, before a digit does: a value shown
    /// with fewer than two decimals, or in compact form, carries no unit.
    #[test]
    fn the_unit_goes_before_the_digits() {
        assert_eq!(
            fit(4.0, false, Some(Unit::Metres)),
            "4.00 m",
            "a short value keeps everything"
        );
        assert_eq!(fit(600.0, true, Some(Unit::Kilograms)), "600 kg");
        let mut shortened = false;
        for unit in [Unit::Metres, Unit::MetresPerSecondSquared, Unit::Radius] {
            for val in [1.0, 12.5, 123.45, 1234.5, 12345.5, 123_456.7, 123_456_792.0] {
                let text = fit(val, false, Some(unit));
                let digits = text.trim_end_matches(|c: char| !c.is_ascii_digit());
                let full_precision = digits
                    .split_once('.')
                    .is_some_and(|(_, decimals)| decimals.len() == 2);
                // A compact "123.5M" is a magnitude, not a unit.
                let bare = digits == text.trim_end_matches(['k', 'M']);
                assert!(
                    full_precision || bare,
                    "{val} {unit:?} lost digits but kept its unit: {text:?}"
                );
                shortened |= text.ends_with(unit.short_suffix()) && !text.ends_with(unit.suffix());
            }
        }
        assert!(
            shortened,
            "some value showed its short unit before dropping it"
        );
    }

    /// Only when nothing with the unit fits does the unit go; the leading
    /// digit never does.
    #[test]
    fn a_value_that_cannot_hold_its_unit_drops_it_last() {
        assert_eq!(
            fit(100_000_000.0, true, Some(Unit::MillionCells)),
            "100000000"
        );
    }

    struct Recording(std::cell::RefCell<std::collections::HashSet<String>>);

    impl crate::text::TextMeasure for Recording {
        fn measure_text(
            &self,
            text: &str,
            font_size: u16,
            _weight: crate::node::FontWeight,
        ) -> crate::node::Vec2 {
            self.0.borrow_mut().insert(text.to_owned());
            crate::node::Vec2::new(
                text.chars().count() as f32 * font_size as f32 * 0.6,
                font_size as f32,
            )
        }
    }

    /// A dragged value rewrites its text every frame; the fit must keep asking
    /// the measurer about the same few glyphs and unit tails, or every drag
    /// frame misses the measure cache.
    #[test]
    fn dragging_a_value_measures_only_glyphs_and_unit_tails() {
        let measure = Recording(Default::default());
        let mut buf = ParamValueText::default();
        for step in 0..400 {
            let val = step as f32 * 0.37 - 20.0;
            param_value_text_fit(
                &mut buf,
                val,
                -20.0,
                false,
                false,
                None,
                Some(Unit::Metres),
                &measure,
                FONT_SIZE,
                CARD_VALUE_TEXT_W,
            );
        }
        let seen = measure.0.borrow();
        assert!(
            seen.len() <= 16,
            "{} distinct strings measured: {seen:?}",
            seen.len()
        );
        assert!(
            seen.iter().all(|text| text.chars().count() <= 2),
            "{seen:?}"
        );
    }
}
