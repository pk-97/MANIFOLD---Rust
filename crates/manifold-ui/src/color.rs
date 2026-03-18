use crate::node::Color32;

// All UI color constants ported from UIConstants.cs.
//
// PALETTE: "Studio"
// Foundation: Neutral-cool grays (+0.005 blue shift for screen neutrality)
// Primary accent: Clean blue (0.35, 0.58, 0.92) — selection, focus, interactive
// Semantic: Desaturated green/gold/coral — status only
// Text: Off-white primary (0.88) — reduces eye strain over long sessions
// Elevation: 6-level hierarchy, each step 3-5% luminance apart

// ── Panel / Background ──────────────────────────────────────────────
// 6-level elevation: Void → Deep → Base → Surface → Raised → Elevated
pub const PANEL_BG: Color32 = Color32::new(37, 37, 38, 245);
pub const TRACK_BG: Color32 = Color32::new(36, 36, 37, 255);
pub const TRACK_BG_ALT: Color32 = Color32::new(27, 27, 28, 255);
pub const INSPECTOR_BG: Color32 = Color32::new(26, 26, 27, 255);
pub const INSPECTOR_BG_FOCUSED: Color32 = Color32::new(32, 32, 34, 255);
pub const HEADER_BG: Color32 = Color32::new(16, 16, 16, 255);
pub const CONTROL_BG: Color32 = Color32::new(27, 27, 28, 255);
pub const INPUT_FIELD_BG: Color32 = Color32::new(49, 49, 51, 255);
pub const INPUT_FIELD_BG_ALT: Color32 = Color32::new(40, 40, 42, 255);
pub const DARK_BG: Color32 = Color32::new(13, 13, 14, 255);
pub const SCROLLBAR_BG: Color32 = Color32::new(18, 18, 19, 128);
pub const DROPDOWN_BG: Color32 = Color32::new(27, 27, 28, 255);
pub const DROPDOWN_ITEM_BG: Color32 = Color32::new(49, 49, 51, 255);
pub const DROPDOWN_TEMPLATE_BG: Color32 = Color32::new(33, 33, 34, 255);
pub const PICKER_BG: Color32 = Color32::new(14, 14, 15, 247);
pub const PROGRESS_BAR_BG: Color32 = Color32::new(36, 36, 38, 242);
pub const TRANSPORT_BAR_BG: Color32 = Color32::new(16, 16, 16, 255);
pub const FOOTER_BG: Color32 = Color32::new(19, 19, 20, 255);
pub const OVERLAY_BG: Color32 = Color32::new(13, 13, 14, 237);
pub const HUD_BG: Color32 = Color32::new(10, 10, 11, 230);

// ── Accent colors ─────────────────────────────────────────────────
pub const ACCENT_BLUE: Color32 = Color32::new(89, 148, 235, 255);
pub const ACCENT_BLUE_SLIDER: Color32 = Color32::new(89, 148, 235, 204);
pub const ACCENT_BLUE_DIM: Color32 = Color32::new(77, 122, 199, 64);
pub const ACCENT_BLUE_SELECTION: Color32 = Color32::new(89, 148, 235, 102);
pub const PLAYHEAD_RED: Color32 = Color32::new(217, 64, 56, 255);
pub const INSERT_CURSOR_BLUE: Color32 = Color32::new(89, 148, 242, 230);
pub const PROGRESS_FILL_BLUE: Color32 = Color32::new(89, 173, 235, 255);

// ── Controls ────────────────────────────────────────────────────────
pub const BUTTON_INACTIVE: Color32 = Color32::new(59, 59, 61, 255);
pub const BUTTON_DIM: Color32 = Color32::new(71, 71, 74, 255);
pub const BUTTON_HIGHLIGHTED: Color32 = Color32::new(87, 87, 89, 255);
pub const BUTTON_PRESSED: Color32 = Color32::new(46, 46, 48, 255);
pub const SEPARATOR_COLOR: Color32 = Color32::new(15, 15, 17, 255);
pub const DIVIDER_COLOR: Color32 = Color32::new(56, 56, 60, 255);
pub const HANDLE_BG: Color32 = Color32::new(59, 59, 61, 255);
pub const CHEVRON_COLOR: Color32 = Color32::new(102, 102, 107, 179);
pub const SCROLLBAR_HANDLE: Color32 = Color32::new(89, 89, 94, 204);
pub const SLIDER_BG: Color32 = Color32::new(31, 31, 32, 255);
pub const DRAG_HANDLE_HOVER: Color32 = Color32::new(46, 46, 48, 255);
pub const SLIDER_HANDLE: Color32 = Color32::new(199, 199, 204, 255);
pub const TOGGLE_HIGHLIGHTED: Color32 = Color32::new(217, 217, 222, 255);
pub const TOGGLE_PRESSED: Color32 = Color32::new(158, 158, 163, 255);
pub const DROPDOWN_HIGHLIGHT: Color32 = Color32::new(64, 64, 71, 255);
pub const DROPDOWN_PRESSED_BG: Color32 = Color32::new(46, 46, 51, 255);
pub const SELECTED_LAYER_CONTROL: Color32 = Color32::new(46, 77, 122, 255);
pub const ACTIVE_SHORTCUT_KEY_BG: Color32 = Color32::new(56, 56, 66, 255);

// ── Clip colors (video) ─────────────────────────────────────────────
pub const CLIP_NORMAL: Color32 = Color32::new(173, 168, 163, 255);
pub const CLIP_SELECTED: Color32 = Color32::new(217, 209, 199, 255);
pub const CLIP_HOVER: Color32 = Color32::new(189, 184, 179, 255);
pub const CLIP_LOCKED: Color32 = Color32::new(82, 79, 77, 128);
pub const CLIP_SEPARATOR: Color32 = Color32::new(20, 20, 22, 255);

// ── Clip colors (generator) ─────────────────────────────────────────
pub const CLIP_GEN_NORMAL: Color32 = Color32::new(101, 252, 255, 255);
pub const CLIP_GEN_SELECTED: Color32 = Color32::new(102, 140, 224, 255);
pub const CLIP_GEN_HOVER: Color32 = Color32::new(77, 97, 153, 255);
pub const GEN_TYPE_LABEL: Color32 = Color32::new(140, 179, 242, 255);

// ── Group layer structural colors ───────────────────────────────────
pub const COLLAPSED_GROUP_OVERLAY_BG: Color32 = Color32::new(20, 20, 28, 255);
pub const DEFAULT_GROUP_ACCENT: Color32 = Color32::new(115, 115, 191, 255);
pub const GROUP_BOTTOM_BORDER: Color32 = Color32::new(97, 97, 148, 153);

// ── Text colors ─────────────────────────────────────────────────────
pub const TEXT_NORMAL: Color32 = Color32::new(224, 224, 230, 255);
pub const TEXT_DIMMED: Color32 = Color32::new(158, 158, 163, 255);
pub const TEXT_SUBTLE: Color32 = Color32::new(107, 107, 112, 255);
pub const PLACEHOLDER_TEXT: Color32 = Color32::new(107, 107, 112, 153);
pub const TEXT_NEAR_WHITE: Color32 = Color32::new(209, 209, 214, 255);
pub const DROPDOWN_INACTIVE_TEXT: Color32 = Color32::new(173, 173, 179, 255);

// ── Status colors ───────────────────────────────────────────────────
pub const STATUS_GOOD: Color32 = Color32::new(89, 191, 115, 255);
pub const STATUS_WARNING: Color32 = Color32::new(217, 184, 77, 255);
pub const STATUS_BAD: Color32 = Color32::new(209, 89, 82, 255);
pub const STATUS_NEUTRAL: Color32 = Color32::new(184, 184, 189, 255);
pub const STATUS_ACTIVE: Color32 = Color32::new(209, 115, 56, 255);
pub const STATUS_OFF: Color32 = Color32::new(89, 89, 94, 255);
pub const STATUS_DOT_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);
pub const STATUS_DOT_GREEN: Color32 = Color32::new(64, 179, 77, 255);
pub const STATUS_DOT_YELLOW: Color32 = Color32::new(204, 166, 38, 255);

// ── Transport colors ────────────────────────────────────────────────
pub const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
pub const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
pub const PAUSED_YELLOW: Color32 = Color32::new(209, 166, 38, 255);
pub const STOP_RED: Color32 = Color32::new(128, 51, 51, 255);
pub const RECORD_RED: Color32 = Color32::new(107, 38, 38, 255);
pub const RECORD_ACTIVE: Color32 = Color32::new(209, 46, 46, 255);
pub const SAVE_FLASH_GREEN: Color32 = Color32::new(64, 158, 89, 255);
pub const TRANSPORT_FIELD_BG: Color32 = Color32::new(40, 40, 42, 255);
pub const BPM_RESET_ACTIVE: Color32 = Color32::new(51, 107, 61, 255);
pub const BPM_CLEAR_ACTIVE: Color32 = Color32::new(133, 51, 51, 255);
pub const MIDI_POPUP_ACTIVE: Color32 = Color32::new(89, 46, 89, 255);

// ── Sync source colors ──────────────────────────────────────────────
pub const SYNC_ACTIVE: Color32 = Color32::new(56, 158, 133, 255);
pub const LINK_ORANGE: Color32 = Color32::new(191, 122, 20, 255);
pub const MIDI_PURPLE: Color32 = Color32::new(148, 77, 148, 255);
pub const ABLETON_LINK_BLUE: Color32 = Color32::new(56, 133, 179, 255);

// ── Delete / Remove button ──────────────────────────────────────────
pub const DELETE_BTN_NORMAL: Color32 = Color32::new(97, 46, 46, 255);
pub const DELETE_BTN_HIGHLIGHTED: Color32 = Color32::new(128, 61, 61, 255);
pub const DELETE_BTN_PRESSED: Color32 = Color32::new(64, 31, 31, 255);

// ── Mute / Solo ─────────────────────────────────────────────────────
pub const MUTED_COLOR: Color32 = Color32::new(199, 97, 71, 255);
pub const SOLO_COLOR: Color32 = Color32::new(209, 191, 64, 255);

// ── Effect rack ─────────────────────────────────────────────────────
pub const RACK_BORDER: Color32 = Color32::new(56, 56, 61, 255);
pub const RACK_BG: Color32 = Color32::new(29, 29, 31, 255);
pub const CARD_BORDER: Color32 = Color32::new(46, 46, 49, 255);
pub const RACK_HANDLE_BG: Color32 = Color32::new(37, 37, 43, 255);
pub const RACK_HANDLE_TEXT: Color32 = Color32::new(122, 128, 158, 255);
pub const EFFECT_HEADER_NAME: Color32 = Color32::new(184, 199, 235, 255);
pub const EFFECT_CARD_INNER_BG: Color32 = Color32::new(19, 19, 20, 255);
pub const REMOVE_BTN_BG: Color32 = Color32::new(71, 33, 33, 255);
pub const REMOVE_BTN_HIGHLIGHTED: Color32 = Color32::new(230, 191, 191, 255);
pub const REMOVE_BTN_PRESSED: Color32 = Color32::new(184, 140, 140, 255);
pub const REMOVE_BTN_TEXT: Color32 = Color32::new(204, 107, 107, 255);
pub const ADD_BTN_BG: Color32 = Color32::new(40, 40, 42, 255);
pub const ADD_BTN_HIGHLIGHTED: Color32 = Color32::new(217, 224, 242, 255);
pub const ADD_BTN_PRESSED: Color32 = Color32::new(166, 184, 209, 255);
pub const EFFECT_DRAG_GHOST_BLUE: Color32 = Color32::new(56, 97, 166, 204);
pub const EFFECT_DRAG_GHOST_RACK: Color32 = Color32::new(61, 61, 107, 204);
pub const EFFECT_DRAG_INDICATOR_UNGROUP: Color32 = Color32::new(209, 128, 56, 255);
pub const EFFECT_DRAG_INDICATOR_REGROUP: Color32 = Color32::new(77, 158, 97, 255);

// ── Selection ───────────────────────────────────────────────────────
pub const SELECTED_BORDER: Color32 = Color32::new(89, 148, 235, 255);

// ── Trim handles (viewport clip edges) ─────────────────────────────
pub const TRIM_HANDLE_COLOR: Color32 = Color32::new(255, 255, 255, 51);

// ── Resize handle ───────────────────────────────────────────────────
pub const RESIZE_HANDLE_IDLE: Color32 = Color32::new(89, 89, 94, 0);
pub const RESIZE_HANDLE_HOVER: Color32 = Color32::new(128, 128, 133, 128);
pub const RESIZE_HANDLE_DRAG: Color32 = Color32::new(140, 140, 145, 179);

// ── Driver indicator ────────────────────────────────────────────────
pub const DRIVER_ACTIVE: Color32 = Color32::new(20, 166, 191, 255);
pub const DRIVER_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);

// ── Envelope ────────────────────────────────────────────────────────
pub const ENVELOPE_ACTIVE: Color32 = Color32::new(191, 115, 20, 255);
pub const ENVELOPE_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);
pub const ENVELOPE_CARD_BG: Color32 = Color32::new(23, 23, 24, 255);

// ── Param group ─────────────────────────────────────────────────────
pub const PARAM_GROUP_BG: Color32 = Color32::new(46, 46, 48, 255);

// ── Grid lines ──────────────────────────────────────────────────────
pub const GRID_BAR_LINE: Color32 = Color32::new(107, 107, 112, 128);
pub const GRID_BEAT_LINE: Color32 = Color32::new(82, 82, 87, 77);
pub const GRID_SUBDIVISION_LINE: Color32 = Color32::new(71, 71, 77, 38);
pub const GRID_SIXTEENTH_LINE: Color32 = Color32::new(71, 71, 77, 20);

// ── Layer palette ───────────────────────────────────────────────────
pub const LAYER_PALETTE: [Color32; 8] = [
    Color32::new(100, 148, 210, 220), // Slate blue
    Color32::new(100, 180, 145, 220), // Sage green
    Color32::new(200, 160, 100, 220), // Warm amber
    Color32::new(175, 110, 158, 220), // Dusty rose
    Color32::new(138, 138, 198, 220), // Soft violet
    Color32::new(195, 128, 108, 220), // Terracotta
    Color32::new(100, 185, 182, 220), // Muted teal
    Color32::new(188, 182, 108, 220), // Olive gold
];

// ── Tempo lane ──────────────────────────────────────────────────────
pub const TEMPO_LINE: Color32 = Color32::new(64, 199, 199, 166);
pub const TEMPO_POINT: Color32 = Color32::new(230, 230, 235, 242);

// ── Monitor / Export ────────────────────────────────────────────────
pub const MONITOR_ACTIVE: Color32 = Color32::new(51, 115, 71, 255);
pub const EXPORT_ACTIVE: Color32 = Color32::new(184, 56, 56, 255);

// ── Mute/Solo buttons ───────────────────────────────────────────────
pub const MUTE_BTN_ACTIVE: Color32 = Color32::new(199, 102, 56, 255);
pub const SOLO_BTN_ACTIVE: Color32 = Color32::new(217, 191, 64, 255);
pub const MUTE_SOLO_BTN_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);

// ── Ruler ───────────────────────────────────────────────────────────
pub const RULER_BG: Color32 = Color32::new(102, 102, 102, 255);

// ── Dropdown item states ────────────────────────────────────────────
pub const DROPDOWN_ITEM_SELECTED: Color32 = Color32::new(45, 65, 95, 255);
pub const DROPDOWN_CHECK_COLOR: Color32 = Color32::new(100, 180, 255, 255);
pub const DROPDOWN_BORDER: Color32 = Color32::new(58, 58, 62, 255);

// ── Clip chrome ─────────────────────────────────────────────────────
pub const LOOP_ON_COLOR: Color32 = Color32::new(50, 100, 180, 255);
pub const LOOP_OFF_COLOR: Color32 = Color32::new(45, 45, 48, 255);
pub const BPM_BTN_COLOR: Color32 = Color32::new(40, 40, 42, 255);
pub const BPM_BTN_HOVER: Color32 = Color32::new(50, 50, 55, 255);
pub const GEN_TYPE_COLOR: Color32 = Color32::new(100, 199, 140, 255);
pub const GEN_TYPE_HOVER: Color32 = Color32::new(40, 40, 44, 255);

// ── Master chrome ───────────────────────────────────────────────────
pub const EXIT_PATH_BG: Color32 = Color32::new(48, 48, 51, 255);
pub const EXIT_PATH_HOVER: Color32 = Color32::new(58, 58, 63, 255);
pub const EXIT_PATH_PRESSED: Color32 = Color32::new(40, 40, 43, 255);

// ── Overview strip ──────────────────────────────────────────────────
pub const OVERVIEW_BG: Color32 = Color32::new(15, 15, 17, 255);
pub const OVERVIEW_VIEWPORT: Color32 = Color32::new(89, 148, 235, 64);
pub const OVERVIEW_PLAYHEAD: Color32 = Color32::new(217, 64, 56, 255);
pub const EXPORT_MARKER_COLOR: Color32 = Color32::new(77, 141, 235, 255);
pub const EXPORT_RANGE_HIGHLIGHT: Color32 = Color32::new(77, 140, 235, 31);

// ── Bitmap Panel Common ─────────────────────────────────────────────
pub const TRANSPARENT: Color32 = Color32::new(0, 0, 0, 0);
pub const HOVER_OVERLAY: Color32 = Color32::new(255, 255, 255, 15);
pub const PRESS_OVERLAY: Color32 = Color32::new(255, 255, 255, 25);
pub const PANEL_BG_DARK: Color32 = Color32::new(26, 26, 27, 255);
pub const TEXT_PRIMARY_C32: Color32 = Color32::new(224, 224, 230, 255);
pub const TEXT_WHITE_C32: Color32 = Color32::new(255, 255, 255, 255);
pub const TEXT_LIGHT_C32: Color32 = Color32::new(220, 220, 225, 255);
pub const TEXT_DIMMED_C32: Color32 = Color32::new(158, 158, 163, 255);
pub const DIVIDER_C32: Color32 = Color32::new(56, 56, 60, 255);

// ── Bitmap Slider Palette ───────────────────────────────────────────
pub const SLIDER_TRACK_C32: Color32 = Color32::new(40, 40, 42, 255);
pub const SLIDER_TRACK_HOVER_C32: Color32 = Color32::new(48, 48, 52, 255);
pub const SLIDER_TRACK_PRESSED_C32: Color32 = Color32::new(36, 36, 38, 255);
pub const SLIDER_FILL_C32: Color32 = Color32::new(50, 70, 100, 120);
pub const SLIDER_THUMB_C32: Color32 = Color32::new(180, 200, 230, 255);
pub const SLIDER_TEXT_C32: Color32 = Color32::new(190, 190, 195, 255);

// ── Bitmap Toggle / Accent ──────────────────────────────────────────
pub const ACCENT_BLUE_C32: Color32 = Color32::new(89, 148, 235, 255);
pub const ACCENT_BLUE_HOVER_C32: Color32 = Color32::new(109, 168, 255, 255);
pub const ACCENT_BLUE_PRESS_C32: Color32 = Color32::new(69, 128, 215, 255);
pub const BUTTON_INACTIVE_C32: Color32 = Color32::new(59, 59, 61, 255);
pub const BUTTON_INACTIVE_HOVER_C32: Color32 = Color32::new(74, 74, 76, 255);
pub const BUTTON_INACTIVE_PRESS_C32: Color32 = Color32::new(49, 49, 51, 255);

// ── Bitmap Driver / Envelope Indicators ─────────────────────────────
pub const DRIVER_ACTIVE_C32: Color32 = Color32::new(20, 166, 191, 255);
pub const DRIVER_ACTIVE_HOVER_C32: Color32 = Color32::new(40, 186, 211, 255);
pub const DRIVER_ACTIVE_PRESS_C32: Color32 = Color32::new(10, 146, 171, 255);
pub const DRIVER_INACTIVE_C32: Color32 = Color32::new(72, 72, 78, 255);
pub const DRIVER_INACTIVE_HOVER_C32: Color32 = Color32::new(87, 87, 93, 255);
pub const DRIVER_INACTIVE_PRESS_C32: Color32 = Color32::new(62, 62, 68, 255);
pub const ENVELOPE_ACTIVE_C32: Color32 = Color32::new(191, 115, 20, 255);
pub const ENVELOPE_ACTIVE_HOVER_C32: Color32 = Color32::new(211, 135, 40, 255);
pub const ENVELOPE_ACTIVE_PRESS_C32: Color32 = Color32::new(171, 95, 10, 255);

// ── Bitmap Config Drawer ────────────────────────────────────────────
pub const CONFIG_BG_C32: Color32 = Color32::new(30, 30, 32, 255);
pub const CONFIG_BTN_INACTIVE_C32: Color32 = Color32::new(44, 44, 48, 255);
pub const CONFIG_BTN_HOVER_C32: Color32 = Color32::new(54, 54, 58, 255);
pub const CONFIG_BTN_PRESSED_C32: Color32 = Color32::new(38, 38, 42, 255);

// ── Bitmap Envelope Slider ──────────────────────────────────────────
pub const ENV_TRACK_C32: Color32 = Color32::new(44, 44, 48, 255);
pub const ENV_FILL_C32: Color32 = Color32::new(100, 70, 30, 120);
pub const ENV_THUMB_C32: Color32 = Color32::new(230, 180, 100, 255);
pub const ENV_TRACK_HOVER_C32: Color32 = Color32::new(52, 52, 56, 255);
pub const ENV_TRACK_PRESSED_C32: Color32 = Color32::new(40, 40, 44, 255);

// ── Bitmap Trim / Target Handles ────────────────────────────────────
pub const TRIM_FILL_C32: Color32 = Color32::new(20, 166, 191, 38);
pub const TRIM_BAR_HOVER_C32: Color32 = Color32::new(40, 186, 211, 255);
pub const TARGET_BAR_HOVER_C32: Color32 = Color32::new(211, 135, 40, 255);

// ── Bitmap Effect Card ──────────────────────────────────────────────
pub const EFFECT_CARD_INNER_BG_C32: Color32 = Color32::new(19, 19, 20, 255);
pub const CARD_BORDER_C32: Color32 = Color32::new(46, 46, 49, 255);
pub const DRAG_HANDLE_BG_C32: Color32 = Color32::new(38, 38, 42, 255);
pub const DRAG_HANDLE_HOVER_BG_C32: Color32 = Color32::new(52, 52, 56, 255);

// ── Panel-specific colors ────────────────────────────────────────────

// Header panel
pub const HEADER_BUTTON_DIM: Color32 = Color32::new(71, 71, 74, 255);
pub const HEADER_BUTTON_HOVER: Color32 = Color32::new(90, 90, 94, 255);
pub const HEADER_BUTTON_PRESSED: Color32 = Color32::new(55, 55, 58, 255);
pub const HEADER_BUTTON_ACTIVE: Color32 = Color32::new(89, 173, 232, 255);
pub const HEADER_BUTTON_ACTIVE_HOVER: Color32 = Color32::new(110, 190, 240, 255);
pub const HEADER_BUTTON_ACTIVE_PRESSED: Color32 = Color32::new(70, 150, 210, 255);
pub const HEADER_PROGRESS_FILL: Color32 = Color32::new(89, 173, 232, 255);

// Footer panel
pub const FOOTER_BTN_HOVER: Color32 = Color32::new(75, 75, 79, 255);
pub const FOOTER_BTN_PRESSED: Color32 = Color32::new(50, 50, 53, 255);

// Transport panel
pub const TRANSPORT_BUTTON_HOVER: Color32 = Color32::new(78, 78, 82, 255);
pub const TRANSPORT_SAVE_DIRTY_BG: Color32 = Color32::new(82, 68, 48, 255);
pub const TRANSPORT_BPM_FIELD_HOVER: Color32 = Color32::new(50, 50, 53, 255);

// Inspector panel
pub const SCROLLBAR_TRACK_C32: Color32 = Color32::new(30, 30, 32, 180);
pub const SCROLLBAR_THUMB_C32: Color32 = Color32::new(90, 90, 95, 200);
pub const SCROLLBAR_THUMB_HOVER_C32: Color32 = Color32::new(110, 110, 115, 220);
pub const ADD_EFFECT_BTN_BG_C32: Color32 = Color32::new(40, 45, 50, 255);
pub const ADD_EFFECT_BTN_HOVER_C32: Color32 = Color32::new(55, 65, 75, 255);
pub const ADD_EFFECT_BTN_TEXT_C32: Color32 = Color32::new(130, 170, 210, 255);

// Layer header panel
pub const LAYER_DRAG_SOURCE_DIM: Color32 = Color32::new(22, 22, 24, 255);
pub const LAYER_INSERT_LINE: Color32 = Color32::new(100, 180, 255, 255);
pub const LAYER_ROW_BG: Color32 = Color32::new(40, 40, 42, 255);
pub const LAYER_ROW_HOVER_BG: Color32 = Color32::new(50, 50, 53, 255);
pub const LAYER_ROW_PRESSED_BG: Color32 = Color32::new(35, 35, 37, 255);
pub const LAYER_CHEVRON_HOVER: Color32 = Color32::new(255, 255, 255, 15);
pub const LAYER_CHEVRON_PRESSED: Color32 = Color32::new(255, 255, 255, 8);

// Viewport panel
pub const CLIP_LABEL_BG: Color32 = Color32::new(20, 20, 22, 255);
pub const CLIP_LABEL_BG_HOVER: Color32 = Color32::new(20, 20, 22, 220);

// Dropdown panel
pub const DROPDOWN_SCRIM: Color32 = Color32::new(0, 0, 0, 1);

// ── Interaction thresholds ──────────────────────────────────────────
pub const DRAG_THRESHOLD_PX: f32 = 4.0;
pub const DOUBLE_CLICK_TIME_SEC: f32 = 0.3;
pub const DRAG_EDGE_ZONE_PX: f32 = 72.0;
pub const DRAG_SCROLL_SPEED_PX_PER_SEC: f32 = 900.0;
pub const TRIM_HANDLE_THRESHOLD_PX: f32 = 8.0;
pub const TRIM_HANDLE_MIN_CLIP_WIDTH_PX: f32 = 16.0;
pub const RESIZE_EDGE_PX: f32 = 4.0;

// ── Split ratio ────────────────────────────────────────────────────
pub const DEFAULT_TIMELINE_SPLIT_RATIO: f32 = 0.30;
pub const MIN_TIMELINE_SPLIT_RATIO: f32 = 0.15;
pub const MAX_TIMELINE_SPLIT_RATIO: f32 = 0.70;

// ── Waveform / stem lane heights ───────────────────────────────────
pub const WAVEFORM_LANE_HEIGHT: f32 = 56.0;
pub const STEM_LANE_HEIGHT: f32 = 56.0;

// ── Waveform lane colors (from UIConstants.cs lines 218-255, 300-301) ──
pub const WAVEFORM_LANE_BG: Color32 = Color32::new(28, 28, 28, 255); // Color(0.11, 0.11, 0.11, 1)
pub const WAVEFORM_BTN_NORMAL: Color32 = Color32::new(48, 48, 51, 230); // Color(0.19, 0.19, 0.20, 0.9)
pub const WAVEFORM_BTN_HIGHLIGHTED: Color32 = Color32::new(77, 97, 122, 255); // Color(0.30, 0.38, 0.48, 1)
pub const WAVEFORM_BTN_PRESSED: Color32 = Color32::new(64, 115, 140, 255); // Color(0.25, 0.45, 0.55, 1)
pub const WAVEFORM_REMOVE_HIGHLIGHTED: Color32 = Color32::new(107, 46, 46, 255); // Color(0.42, 0.18, 0.18, 1)
pub const WAVEFORM_REMOVE_PRESSED: Color32 = Color32::new(140, 38, 38, 255); // Color(0.55, 0.15, 0.15, 1)
pub const WAVEFORM_EXPAND_HIGHLIGHTED: Color32 = Color32::new(89, 115, 140, 255); // Color(0.35, 0.45, 0.55, 1)
pub const WAVEFORM_EXPAND_PRESSED: Color32 = Color32::new(64, 140, 166, 255); // Color(0.25, 0.55, 0.65, 1)

// ── Stem lane background colors (subtle per-stem tints, UIConstants.cs lines 247-250) ──
pub const STEM_LANE_BG_DRUMS: Color32 = Color32::new(29, 26, 26, 255); // Color(0.115, 0.10, 0.10, 1)
pub const STEM_LANE_BG_BASS: Color32 = Color32::new(26, 28, 26, 255); // Color(0.10, 0.11, 0.10, 1)
pub const STEM_LANE_BG_OTHER: Color32 = Color32::new(26, 26, 29, 255); // Color(0.10, 0.10, 0.115, 1)
pub const STEM_LANE_BG_VOCALS: Color32 = Color32::new(29, 26, 29, 255); // Color(0.115, 0.10, 0.115, 1)

// ── Spectral waveform palette (WaveformRenderer.cs lines 37-40) ──
pub const SPEC_SUB: Color32 = Color32::new(180, 40, 40, 255);
pub const SPEC_LOW: Color32 = Color32::new(230, 140, 50, 255);
pub const SPEC_MID: Color32 = Color32::new(200, 230, 180, 255);
pub const SPEC_HIGH: Color32 = Color32::new(80, 180, 255, 255);
pub const WAVEFORM_CENTER_LINE: Color32 = Color32::new(60, 60, 60, 80);

// ── Insert cursor marker ───────────────────────────────────────────
pub const INSERT_CURSOR_RULER_MARKER_SIZE: f32 = 6.0;

// ── Layout constants ────────────────────────────────────────────────
pub const TRANSPORT_BAR_HEIGHT: f32 = 36.0;
pub const HEADER_HEIGHT: f32 = 40.0;
pub const FOOTER_HEIGHT: f32 = 29.0;
pub const TRACK_HEIGHT: f32 = 140.0;
pub const COLLAPSED_TRACK_HEIGHT: f32 = 48.0;
pub const COLLAPSED_GEN_TRACK_HEIGHT: f32 = 62.0;
pub const RULER_HEIGHT: f32 = 40.0;
pub const LAYER_CONTROLS_WIDTH: f32 = 200.0;
pub const PLAYHEAD_WIDTH: f32 = 2.0;
pub const CLIP_MIN_WIDTH: f32 = 10.0;
pub const CLIP_VERTICAL_PAD: f32 = 12.0;
pub const OVERVIEW_STRIP_HEIGHT: f32 = 16.0;
pub const MIN_INSPECTOR_WIDTH: f32 = 196.0;
pub const MAX_INSPECTOR_WIDTH: f32 = 500.0;
pub const DEFAULT_INSPECTOR_WIDTH: f32 = 500.0;
pub const INSPECTOR_RESIZE_HANDLE_WIDTH: f32 = 6.0;
pub const INSPECTOR_GAP: f32 = 4.0;

// ── Group layout ────────────────────────────────────────────────────
pub const GROUP_CHILD_INDENT_PX: f32 = 20.0;
pub const COLLAPSED_GROUP_TRACK_HEIGHT: f32 = 70.0;
pub const GROUP_ACCENT_BAR_WIDTH: f32 = 5.0;
pub const GROUP_BOTTOM_BORDER_HEIGHT: f32 = 2.0;

// ── Spacing scale ───────────────────────────────────────────────────
pub const SPACE_XS: f32 = 2.0;
pub const SPACE_S: f32 = 4.0;
pub const SPACE_M: f32 = 8.0;
pub const SPACE_L: f32 = 12.0;

// ── Corner radii ────────────────────────────────────────────────────
pub const BUTTON_RADIUS: f32 = 3.0;
pub const CARD_RADIUS: f32 = 4.0;
pub const SMALL_RADIUS: f32 = 2.0;
pub const POPUP_RADIUS: f32 = 6.0;

// ── Font sizes ──────────────────────────────────────────────────────
pub const FONT_CAPTION: u16 = 8;
pub const FONT_SMALL: u16 = 9;
pub const FONT_BODY: u16 = 10;
pub const FONT_LABEL: u16 = 11;
pub const FONT_SUBHEADING: u16 = 12;
pub const FONT_HEADING: u16 = 14;
pub const FONT_TITLE: u16 = 16;

// ── Zoom levels (pixels per beat) ───────────────────────────────────
pub const ZOOM_LEVELS: [f32; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 80.0, 120.0, 200.0, 400.0];
pub const DEFAULT_ZOOM_INDEX: usize = 7; // 120 pixels/beat

// ── Scroll ──────────────────────────────────────────────────────────
pub const SCROLL_SENSITIVITY: f32 = 5.0;
pub const BITMAP_SCROLL_SPEED: f32 = 12.5;

// ── Layer control panel layout ──────────────────────────────────────
pub const LAYER_CTRL_PADDING: f32 = 5.0;
pub const LAYER_CTRL_CHEVRON_WIDTH: f32 = 18.0;
pub const LAYER_CTRL_DRAG_HANDLE_WIDTH: f32 = 18.0;
pub const LAYER_CTRL_NAME_ROW_HEIGHT: f32 = 18.0;
pub const LAYER_CTRL_ROW_STEP: f32 = 23.0;
pub const LAYER_CTRL_MUTE_SOLO_BTN_WIDTH: f32 = 28.0;
pub const LAYER_CTRL_BTN_HEIGHT: f32 = 18.0;
pub const LAYER_CTRL_INFO_ROW_HEIGHT: f32 = 14.0;
pub const LAYER_CTRL_SEPARATOR_HEIGHT: f32 = 1.0;
pub const LAYER_CTRL_RIGHT_GUTTER: f32 = 10.0;
pub const LAYER_CTRL_TOP_ROW_GAP: f32 = 2.0;
pub const LAYER_CTRL_FOLDER_BTN_WIDTH: f32 = 42.0;
pub const LAYER_CTRL_NEW_CLIP_BTN_WIDTH: f32 = 62.0;
pub const LAYER_CTRL_ADD_GEN_CLIP_BTN_WIDTH: f32 = 50.0;
pub const LAYER_CTRL_GEN_TYPE_ROW_HEIGHT: f32 = 14.0;
pub const LAYER_CTRL_MIDI_LABEL_WIDTH: f32 = 30.0;
pub const LAYER_CTRL_CHANNEL_LABEL_WIDTH: f32 = 20.0;
pub const LAYER_CTRL_SMALL_FONT_SIZE: u16 = 9;
pub const LAYER_CTRL_NAME_FONT_SIZE: u16 = 11;
pub const LAYER_CTRL_HANDLE_FONT_SIZE: u16 = 14;
