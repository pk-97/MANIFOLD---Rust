/// Shared widget building-block dimensions used by UIElementBuilder
/// and all consumers of sliders, drivers, toggles, and dropdowns.
/// All spatial values are multiplied by `SCALE` for uniform sizing.
///
/// Mechanical translation of Assets/Scripts/UI/Timeline/Core/WidgetLayout.cs.

/// Uniform scale factor for all effect-card widget dimensions.
pub const SCALE: f32 = 1.25;

// ── Slider rows (UIElementBuilder.CreateSlider) ──────────────────
pub const SLIDER_ROW_HEIGHT: f32 = 22.0 * SCALE;
pub const SLIDER_TRACK_HEIGHT: f32 = 18.0 * SCALE;
pub const SLIDER_HANDLE_WIDTH: f32 = 10.0 * SCALE;
pub const SLIDER_VALUE_TEXT_WIDTH: f32 = 40.0 * SCALE;
pub const SLIDER_ROW_SPACING: f32 = 4.0 * SCALE;
pub const SLIDER_LABEL_WIDTH: f32 = 56.0 * SCALE;
pub const EFFECT_LABEL_WIDTH: f32 = 50.0 * SCALE;
pub const MIN_SLIDER_TRACK_WIDTH: f32 = 30.0 * SCALE;

// ── Driver config (UIElementBuilder.CreateDriverButton/ConfigRow) ─
pub const DRIVER_BUTTON_SIZE: f32 = 20.0 * SCALE;
pub const DRIVER_CONFIG_ROW_HEIGHT: f32 = 18.0 * SCALE;
pub const DRIVER_CONFIG_CONTAINER_HEIGHT: f32 = 41.0 * SCALE;
pub const DRIVER_CONFIG_EXPANDED_CARD_EXTRA: f32 = 43.0 * SCALE;
pub const BEAT_DIV_BUTTON_WIDTH: f32 = 22.0 * SCALE;
pub const WAVEFORM_BUTTON_WIDTH: f32 = 30.0 * SCALE;
pub const DRIVER_CONFIG_PADDING_H: i32 = (4.0 * SCALE) as i32;

// ── Envelope inline config (per-param ADSR drawer on effect cards) ─
pub const ENVELOPE_BUTTON_SIZE: f32 = 20.0 * SCALE;
pub const ENVELOPE_CONFIG_CONTAINER_HEIGHT: f32 = 43.0 * SCALE;
pub const ENVELOPE_CONFIG_EXPANDED_CARD_EXTRA: f32 = 45.0 * SCALE;
pub const ENVELOPE_CONFIG_MINI_SLIDER_LABEL_WIDTH: f32 = 14.0 * SCALE;
pub const ENVELOPE_CONFIG_PADDING_H: i32 = (4.0 * SCALE) as i32;
pub const ENVELOPE_TRIM_BAR_EXTENSION: f32 = 4.0 * SCALE;

// ── Param group sub-card (visual wrapper per slider row) ────────
pub const PARAM_GROUP_PADDING_V: i32 = (2.0 * SCALE) as i32;
pub const PARAM_GROUP_PADDING_H: i32 = (2.0 * SCALE) as i32;

// ── Toggle buttons (UIElementBuilder.CreateToggleButton) ─────────
pub const TOGGLE_BUTTON_FONT_SIZE: i32 = (9.0 * SCALE) as i32;

// ── Dropdown ────────────────────────────────────────────────────
pub const DROPDOWN_ITEM_HEIGHT: f32 = 26.0 * SCALE;
pub const DROPDOWN_SPACING: f32 = 2.0 * SCALE;
pub const DROPDOWN_ARROW_WIDTH: f32 = 12.0 * SCALE;
pub const DROPDOWN_ARROW_FONT_SIZE: i32 = (8.0 * SCALE) as i32;
pub const DROPDOWN_ITEM_FONT_SIZE: i32 = (11.0 * SCALE) as i32;
pub const DROPDOWN_PADDING: i32 = (4.0 * SCALE) as i32;
pub const DROPDOWN_TEXT_LEFT_INSET: f32 = 6.0 * SCALE;
pub const DROPDOWN_TEXT_RIGHT_INSET: f32 = 4.0 * SCALE;

// ── Scrollbar ────────────────────────────────────────────────────
pub const SCROLLBAR_THICKNESS: f32 = 10.0 * SCALE;

// ── Effect card structure (EffectCardPresenter) ─────────────────
pub const ADD_BUTTON_HEIGHT: f32 = 24.0 * SCALE;
pub const RACK_HEADER_HEIGHT: f32 = 44.0 * SCALE;
pub const MOD_BADGE_WIDTH: f32 = 36.0 * SCALE;
pub const MOD_BADGE_HEIGHT: f32 = 14.0 * SCALE;
pub const MOD_BADGE_RADIUS: f32 = 7.0 * SCALE;
pub const MOD_BADGE_FONT_SIZE: i32 = (7.0 * SCALE) as i32;

// ── Font sizes ────────────────────────────────────────────────────
pub const INSPECTOR_LABEL_FONT_SIZE: i32 = (11.0 * SCALE) as i32;
pub const INSPECTOR_BUTTON_FONT_SIZE: i32 = (10.0 * SCALE) as i32;
pub const MINI_BUTTON_FONT_SIZE: i32 = (8.0 * SCALE) as i32;

// ── Shared ──────────────────────────────────────────────────────
/// 0.0: instant state transitions. Non-zero fade durations cause flicker.
pub const BUTTON_FADE_DURATION: f32 = 0.0;
