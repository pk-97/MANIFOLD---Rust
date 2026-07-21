//! Builder functions for parameter slider rows, drawers, and their style helpers.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;


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


// Height of the driver (LFO) drawer container. Three button rows + pads:
//   1. the 11-cell beat-division grid (sync rate),
//   2. the feel + free + invert modifiers (Straight/Dotted/Triplet/Free/Invert),
//   3. the 5 waveform-shape icons.
// Derived from the shared drawer metrics so the card's reserved height can't
// drift from what's actually drawn (mirrors `audio_config_height`).
pub(crate) fn driver_config_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(3)
}


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


/// Container height of a clip-trigger drawer: Source row, Listen (chips)
/// row, Sensitivity slider, Length row — plus the Sensitivity meter strip.
///
/// Paired with [`build_clip_trigger_drawer`] so a caller reserving height
/// (the AUDIO TRIGGERS section) can't drift from what's actually built.
pub(crate) fn clip_trigger_drawer_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(4) + crate::panels::drawer::METER_STRIP_H
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
    // the same `EnvDecay` scrub gesture the drag path already emits, reset to
    // `DEFAULT_ENV_DECAY`.
    let reset = PanelAction::slider_reset(
        PanelAction::Scrub(ValueRef::EnvDecay(target.clone(), pid.clone()), ScrubPhase::Begin),
        PanelAction::Scrub(
            ValueRef::EnvDecay(target.clone(), pid.clone()),
            ScrubPhase::Move(ScrubValue::Scalar(DEFAULT_ENV_DECAY)),
        ),
        PanelAction::Scrub(ValueRef::EnvDecay(target, pid), ScrubPhase::Commit),
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
        PanelAction::Scrub(ValueRef::AudioModShape(gpt.clone(), pid.clone(), which), ScrubPhase::Begin),
        PanelAction::Scrub(
            ValueRef::AudioModShape(gpt.clone(), pid.clone(), which),
            ScrubPhase::Move(ScrubValue::Scalar(default)),
        ),
        PanelAction::Scrub(ValueRef::AudioModShape(gpt, pid, which), ScrubPhase::Commit),
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
        PanelAction::Scrub(ValueRef::AudioTriggerShape(layer_id.clone(), row, which), ScrubPhase::Begin),
        PanelAction::Scrub(
            ValueRef::AudioTriggerShape(layer_id.clone(), row, which),
            ScrubPhase::Move(ScrubValue::Scalar(default)),
        ),
        PanelAction::Scrub(ValueRef::AudioTriggerShape(layer_id.clone(), row, which), ScrubPhase::Commit),
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
                PanelAction::Scrub(ValueRef::AudioModStepAmount(gpt.clone(), pid.clone()), ScrubPhase::Begin),
                PanelAction::Scrub(
                    ValueRef::AudioModStepAmount(gpt.clone(), pid.clone()),
                    ScrubPhase::Move(ScrubValue::Scalar(default_amount)),
                ),
                PanelAction::Scrub(ValueRef::AudioModStepAmount(gpt.clone(), pid.clone()), ScrubPhase::Commit),
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

