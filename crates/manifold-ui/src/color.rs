use crate::node::Color32;

// ── Contrast text color ────────────────────────────────────────────
// Perceptual brightness using W3C luminance blended with max channel
// to account for the Helmholtz-Kohlrausch effect: saturated colors
// (red, purple, blue) appear brighter than their luminance suggests.
// Threshold 0.45 matches Ableton's aggressive "black unless very dark" style.
pub const TEXT_ON_DARK: Color32 = Color32::new(230, 230, 230, 255);
pub const TEXT_ON_BRIGHT: Color32 = Color32::new(0, 0, 0, 255);

// ── Colour brightness helpers ──────────────────────────────────────
// The single home for the "brighten/darken a colour for hover/selected/pressed"
// idiom. It was hand-rolled identically in clip/layer/transport chrome and inlined
// in the marker, swatch, and clip-bitmap paths.

/// Lighten by adding `amount` to each RGB channel (saturating); alpha unchanged.
pub fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_add(amount),
        c.g.saturating_add(amount),
        c.b.saturating_add(amount),
        c.a,
    )
}

/// Darken by subtracting `amount` from each RGB channel (saturating); alpha unchanged.
pub fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_sub(amount),
        c.g.saturating_sub(amount),
        c.b.saturating_sub(amount),
        c.a,
    )
}

/// Multiply each RGB channel by `factor` (clamped to 0..=255); alpha unchanged.
/// A *proportional* darken/brighten — unlike `darken`/`lighten`, which add or
/// subtract a flat amount, this scales, so "32% darker" (`factor = 0.68`) reads
/// uniformly across a bright neon header and a deep one. Used to derive a
/// header-chip surface from the layer's own identity colour.
pub fn scale_rgb(c: Color32, factor: f32) -> Color32 {
    let s = |x: u8| (x as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color32::new(s(c.r), s(c.g), s(c.b), c.a)
}

/// Perceptual luminance (Rec. 709) in 0..1. A header is "light" above ~0.55,
/// where a tonal (darkened) chip on it needs a faint dark hairline to re-seat;
/// on a dark header the darkened chip separates on its own.
pub fn relative_luminance(c: Color32) -> f32 {
    0.2126 * (c.r as f32 / 255.0) + 0.7152 * (c.g as f32 / 255.0) + 0.0722 * (c.b as f32 / 255.0)
}

/// Same colour at a new alpha. The one place to derive a translucent variant of
/// a colour that is only known at runtime (a layer's contrast text faded to a
/// secondary label, a hue at a wash alpha), so call sites don't hand-roll a raw
/// `Color32::new(c.r, c.g, c.b, a)`.
pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::new(c.r, c.g, c.b, a)
}

/// Linear interpolation between two colours, `t` clamped to 0..1 (alpha mixed too).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::new(
        lerp(a.r, b.r),
        lerp(a.g, b.g),
        lerp(a.b, b.b),
        lerp(a.a, b.a),
    )
}

pub fn contrast_text_color(bg: Color32) -> Color32 {
    let r = bg.r as f32 / 255.0;
    let g = bg.g as f32 / 255.0;
    let b = bg.b as f32 / 255.0;
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let max_channel = r.max(g).max(b);
    let perceived = luminance * 0.6 + max_channel * 0.4;
    if perceived > 0.45 {
        TEXT_ON_BRIGHT
    } else {
        TEXT_ON_DARK
    }
}

// ════════════════════════════════════════════════════════════════════
// DESIGN TOKENS — single source of truth for the UI's look.
//
// The semantic constants below (PANEL_BG, INSPECTOR_BG, DROPDOWN_BG, …)
// map ONTO these tokens. Edit a token here and every surface that consumes
// it shifts together — that is the whole point: no more per-panel greys
// drifting apart.
//
// Grouping comes from FILL LEVEL, not boxes: a section is BG_2 sitting on
// BG_1; a control is BG_3 sitting on BG_2. Each ramp step is ~9–11 apart so
// the layers read as distinct (the old palette had ~4 greys colliding at
// 26–27, so nothing separated). (+1–2 blue channel for screen neutrality.)
//
// Keep it dark. "High contrast" for a live tool means clearly DISTINCT
// LEVELS, not a bright UI — a bright UI is fatiguing on stage and glows in a
// dark room. Distinct steps + bold accent, still dark.
//
// Spacing / radius / font scales are tokens too — they live further down
// (search "Spacing scale", "Corner radii", "Font sizes").
// ════════════════════════════════════════════════════════════════════

// ── Grey ramp (elevation) ───────────────────────────────────────────
// §A contrast pass: the range is spread wider (deeper void, lighter raised
// control) so the levels read as distinct on a dark stage without the UI going
// bright — deeper black gives raised surfaces somewhere to separate to.
pub const BG_0: Color32 = Color32::new(9, 9, 11, 255); // app / void
pub const BG_1: Color32 = Color32::new(23, 23, 25, 255); // panel
pub const BG_2: Color32 = Color32::new(33, 33, 36, 255); // card / section (on a panel)
pub const BG_3: Color32 = Color32::new(48, 48, 52, 255); // control / input (on a section)
// Hover = one notch up, pressed = one notch down. Same deltas everywhere.
pub const BG_1_HOVER: Color32 = Color32::new(31, 31, 33, 255);
pub const BG_2_HOVER: Color32 = Color32::new(42, 42, 45, 255);
pub const BG_2_PRESSED: Color32 = Color32::new(28, 28, 30, 255);
pub const BG_3_HOVER: Color32 = Color32::new(57, 57, 62, 255);
pub const BG_3_PRESSED: Color32 = Color32::new(41, 41, 45, 255);

// ── Dividers (two roles, one value each) ────────────────────────────
// Was five near-duplicate line colours hand-picked per panel. Now:
//   DIVIDER — subtle hairline BETWEEN groups (chrome, dropdown items,
//             section breaks). A line between things, not a box around them.
//   GROOVE  — dark inset line separating stacked tracks/rows in the
//             timeline + layer panel (reads as void showing through).
pub const DIVIDER: Color32 = Color32::new(64, 64, 68, 255); // §A: more visible
pub const GROOVE: Color32 = Color32::new(8, 8, 10, 255); // §A: tracks the deeper void
// One element-outline hairline. §17: was 5 near-duplicate greys (RACK 56 /
// CARD 46 / CARD_C32 55 / DROPDOWN 58); collapsed to one. In-panel grouping
// should lean on fill level (the BG ramp), not boxes — this is the subtle
// edge for surfaces that still want an outline + the floating-element border.
// (The purple-tinted generator-card border stays its own identity tint.)
pub const BORDER: Color32 = Color32::new(64, 64, 68, 255); // §A: more visible element outline

// ── Elevation shadow (§17) ──────────────────────────────────────────
// One soft step under FLOATING surfaces only (dropdowns, popovers, modals).
// A lift, not a glow: dark + low alpha. In-panel grouping stays fill-level.
pub const SHADOW: Color32 = Color32::new(0, 0, 0, 110);
pub const SHADOW_BLUR: f32 = 18.0;
pub const SHADOW_OFFSET_Y: f32 = 4.0;

/// no shadows anywhere for now — dark-on-dark shadows read
/// as smudge, not elevation. Flip to re-enable; call sites are gated, the
/// draw_shadow primitive stays.
pub const SHADOWS_ENABLED: bool = false;

// ── Semantic colour ramp (§15) ──────────────────────────────────────
// One definition per role-hue, three steps each (idle · base · active).
// The state colours below ALIAS onto these, so the same red/green/amber
// means the same thing in every widget — the chromatic counterpart to the
// grey ramp. On a dark stage under coloured wash, inconsistent hue+
// brightness washes out; consistent steps read.
//
// Deliberately NOT folded onto this ramp (they must stay distinct on a
// single widget, or are an identity palette): the modulation indicators
// (DRIVER cyan / ENVELOPE orange / AUDIO green / ABL purple — co-drawn on
// one slider), the sync-source colours (LINK / MIDI / Ableton), the marker
// and layer-colour palettes, and the M/S/L/A layer quartet. §15:
// "consistent steps, not artificial collapse." Values are the §15.2 table —
// tune on the running app (the warm trio red/amber/orange must stay apart).
pub const RED_IDLE: Color32 = Color32::new(107, 38, 38, 255);
pub const RED_BASE: Color32 = Color32::new(184, 56, 56, 255);
pub const RED_ACTIVE: Color32 = Color32::new(217, 64, 56, 255);
pub const GREEN_IDLE: Color32 = Color32::new(51, 107, 61, 255);
pub const GREEN_BASE: Color32 = Color32::new(64, 158, 89, 255);
pub const GREEN_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
pub const AMBER_IDLE: Color32 = Color32::new(156, 128, 40, 255);
pub const AMBER_BASE: Color32 = Color32::new(204, 166, 38, 255);
pub const AMBER_ACTIVE: Color32 = Color32::new(217, 191, 64, 255);
pub const ORANGE_IDLE: Color32 = Color32::new(140, 82, 30, 255);
pub const ORANGE_BASE: Color32 = Color32::new(199, 102, 56, 255);
pub const ORANGE_ACTIVE: Color32 = Color32::new(209, 115, 56, 255);
pub const BLUE_IDLE: Color32 = Color32::new(77, 122, 199, 255);
pub const BLUE_BASE: Color32 = Color32::new(89, 148, 235, 255);
pub const BLUE_ACTIVE: Color32 = Color32::new(120, 170, 245, 255);
pub const CYAN_IDLE: Color32 = Color32::new(40, 120, 140, 255);
pub const CYAN_BASE: Color32 = Color32::new(20, 166, 191, 255);
pub const CYAN_ACTIVE: Color32 = Color32::new(64, 200, 224, 255);
pub const PURPLE_IDLE: Color32 = Color32::new(90, 72, 120, 255);
pub const PURPLE_BASE: Color32 = Color32::new(115, 115, 191, 255);
pub const PURPLE_ACTIVE: Color32 = Color32::new(150, 130, 210, 255);

// All UI color constants ported from UIConstants.cs.
//
// PALETTE: "Studio"
// Foundation: Neutral-cool grays (+0.005 blue shift for screen neutrality)
// Primary accent: Clean blue (0.35, 0.58, 0.92) — selection, focus, interactive
// Semantic: Desaturated green/gold/coral — status only
// Text: Off-white primary (0.88) — reduces eye strain over long sessions
// Elevation: 4-step grey ramp (BG_0..BG_3) above — see DESIGN TOKENS

// ── Panel / Background ──────────────────────────────────────────────
// Mapped onto the grey ramp (BG_0..BG_3). Chrome bars (header / transport /
// footer / HUD) keep their own near-black level — they're the deliberate
// dark frame around the app, not part of the elevation ramp.
pub const PANEL_BG: Color32 = Color32::new(22, 22, 24, 245); // BG_1 @ a245
pub const TRACK_BG: Color32 = BG_2; // timeline lane
pub const TRACK_BG_ALT: Color32 = BG_1; // alternating lane
// The focused/selected lane's own background — a muted navy, distinct from the
// grey zebra so the selected layer reads as selected at a glance (not just a
// brighter stripe). Dark enough to sit behind clips without competing. Shares
// the blue family of `SELECTED_LAYER_RING` / the selection accent bar.
pub const TRACK_BG_SELECTED: Color32 = Color32::new(30, 42, 64, 255);
pub const INSPECTOR_BG: Color32 = BG_1;
pub const INSPECTOR_BG_FOCUSED: Color32 = BG_1_HOVER;
pub const HEADER_BG: Color32 = Color32::new(16, 16, 16, 255); // chrome
pub const CONTROL_BG: Color32 = BG_2;
pub const INPUT_FIELD_BG: Color32 = BG_3;
pub const INPUT_FIELD_BG_ALT: Color32 = BG_2;
pub const DARK_BG: Color32 = BG_0;
pub const SCROLLBAR_BG: Color32 = Color32::new(18, 18, 19, 128);
pub const DROPDOWN_BG: Color32 = BG_2;
pub const DROPDOWN_ITEM_BG: Color32 = BG_3;
pub const DROPDOWN_TEMPLATE_BG: Color32 = BG_2;
pub const PICKER_BG: Color32 = Color32::new(14, 14, 15, 247); // overlay scrim
pub const PROGRESS_BAR_BG: Color32 = Color32::new(31, 31, 33, 242); // BG_2 @ a242
pub const OVERLAY_BG: Color32 = Color32::new(13, 13, 14, 237); // BG_0 @ a237
pub const HUD_BG: Color32 = Color32::new(10, 10, 11, 255); // HUD (below void)

// ── Accent colors ─────────────────────────────────────────────────
pub const ACCENT_BLUE: Color32 = BLUE_BASE;
pub const ACCENT_BLUE_SLIDER: Color32 = Color32::new(89, 148, 235, 204);
pub const ACCENT_BLUE_DIM: Color32 = Color32::new(77, 122, 199, 64);
pub const ACCENT_BLUE_SELECTION: Color32 = Color32::new(89, 148, 235, 102);
pub const PLAYHEAD_RED: Color32 = RED_ACTIVE;
pub const INSERT_CURSOR_BLUE: Color32 = Color32::new(89, 148, 242, 230);
pub const PROGRESS_FILL_BLUE: Color32 = Color32::new(89, 173, 235, 255);

// ── Controls ────────────────────────────────────────────────────────
pub const BUTTON_INACTIVE: Color32 = Color32::new(59, 59, 61, 255);
pub const BUTTON_DIM: Color32 = Color32::new(71, 71, 74, 255);
pub const BUTTON_HIGHLIGHTED: Color32 = Color32::new(87, 87, 89, 255);
pub const BUTTON_PRESSED: Color32 = Color32::new(46, 46, 48, 255);
pub const SEPARATOR_COLOR: Color32 = GROOVE; // track groove
pub const TRACK_SEPARATOR_HEIGHT: f32 = 2.0;
pub const GROUP_SEPARATOR_HEIGHT: f32 = 3.0;
pub const GROUP_SEPARATOR_COLOR: Color32 = GROOVE; // group groove (heavier via height, not colour)
pub const DIVIDER_COLOR: Color32 = DIVIDER;
pub const HANDLE_BG: Color32 = Color32::new(59, 59, 61, 255);
pub const CHEVRON_COLOR: Color32 = Color32::new(102, 102, 107, 179);
pub const SCROLLBAR_HANDLE: Color32 = Color32::new(89, 89, 94, 204);
pub const SLIDER_BG: Color32 = BG_2;
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

// ── GPU clip body styling (§24 5b) ──────────────────────────────────
// Clips render as GPU SDF rounded rects (body gradient + border + lift). These
// are the *default* values — the look is tuned by eye on the running app in the
// Phase-6 taste pass, not fixed here. Keep them in one place so that tuning is a
// one-line edit, never a renderer change.
//
/// Corner radius of a clip body. Subtle — a tile, not a pill.
pub const CLIP_RADIUS: f32 = 4.0;
/// Border on a normal (unselected) clip. Low-alpha dark edge so adjacent clips
/// read as distinct tiles without the old blue-on-every-clip busyness.
pub const CLIP_BORDER_NORMAL: Color32 = Color32::new(12, 12, 14, 140);
pub const CLIP_BORDER_NORMAL_WIDTH: f32 = 1.0;
/// Border on the selected clip. §E: the bright `SELECTED_LAYER_RING` (not the
/// accent blue) so it reads on a blue layer too — unified with the layer-header
/// selection ring; a blue border vanished on a blue clip ("muted on muted").
pub const CLIP_BORDER_SELECTED: Color32 = SELECTED_LAYER_RING;
pub const CLIP_BORDER_SELECTED_WIDTH: f32 = 2.0;
/// Vertical body gradient: the top edge is lightened by this many 0-255 steps
/// over the base colour, fading to the base at the bottom. Gives the body a
/// soft top-lit roundness. 0 = flat.
pub const CLIP_GRADIENT_LIGHTEN: u8 = 14;
/// Soft drop-shadow under the *selected* clip (lift-on-select, §17). Subtler
/// than the overlay-panel shadow — a clip rises a little, it doesn't float.
pub const CLIP_SHADOW: Color32 = Color32::new(0, 0, 0, 90);
pub const CLIP_SHADOW_BLUR: f32 = 6.0;
pub const CLIP_SHADOW_OFFSET_Y: f32 = 2.0;
/// Clip name label: dark text on a light body, light text on a dark one (chosen
/// by body luminance so the label reads on any layer colour). Min width below
/// which the label is dropped (too narrow to be legible).
pub const CLIP_LABEL_ON_LIGHT: Color32 = Color32::new(18, 18, 20, 235);
pub const CLIP_LABEL_ON_DARK: Color32 = Color32::new(238, 238, 242, 235);
pub const CLIP_LABEL_MIN_WIDTH: f32 = 30.0;
/// Left inset of the label inside the clip body.
pub const CLIP_LABEL_PAD_X: f32 = 6.0;

// ── Clip name-strip band (§E / §K15) ────────────────────────────────
// Premiere/FCP anatomy: a content PREVIEW on top + a solid layer-coloured NAME
// STRIP on the bottom (the mockup's `.clip .body` + `.clip .strip`). The strip
// carries the identity colour and the name; the preview area is a darker well of
// the same hue (it becomes the thumbnail once §F lands). Below a minimum clip
// height the band is dropped — the clip is just a solid identity bar + name
// (collapsed lanes), matching the mockup's collapsed-row clip.
pub const CLIP_STRIP_HEIGHT: f32 = 16.0;
/// Minimum clip height that still carries a name strip. Down to this height the
/// strip is *proportional* (≤ `CLIP_STRIP_HEIGHT`) so even a collapsed-lane clip
/// keeps a solid band the name reads on (the thumbnail reserves it) rather than a
/// name floating over the preview. Below this the clip is too short for any band.
pub const CLIP_STRIP_MIN_CLIP_HEIGHT: f32 = 22.0;
/// Margin (logical px) the thumbnail is inset from the preview-well edges on the
/// top/left/right, plus the gap it leaves above the name strip. The darker well
/// then frames the thumbnail as a dedicated panel rather than the image bleeding
/// to the clip border.
pub const CLIP_THUMB_INSET: f32 = 4.0;
/// The preview well = the identity colour scaled toward black by this factor
/// (hue-preserving), standing in for the thumbnail until §F populates it. Keeps
/// the clip's identity readable while making the strip read as a distinct band.
/// Tuned so the well is clearly darker than the strip without going muddy.
pub const CLIP_PREVIEW_WELL_SCALE: f32 = 0.5;

// ── Group layer structural colors ───────────────────────────────────
pub const COLLAPSED_GROUP_OVERLAY_BG: Color32 = Color32::new(20, 20, 28, 255);
pub const DEFAULT_GROUP_ACCENT: Color32 = PURPLE_BASE;
pub const GROUP_BOTTOM_BORDER: Color32 = Color32::new(97, 97, 148, 153);

// ── Text colors ─────────────────────────────────────────────────────
// §A contrast pass: brighter at every tier so secondary/faint labels actually
// read on the dark chrome (Peter's "faint labels you can't read"), still clearly
// stepped below TEXT_NORMAL.
pub const TEXT_NORMAL: Color32 = Color32::new(230, 230, 235, 255);
pub const TEXT_DIMMED: Color32 = Color32::new(178, 178, 184, 255);
pub const TEXT_SUBTLE: Color32 = Color32::new(130, 130, 136, 255);
pub const TEXT_FAINT: Color32 = Color32::new(104, 104, 110, 255);
pub const PLACEHOLDER_TEXT: Color32 = Color32::new(107, 107, 112, 153);
pub const TEXT_NEAR_WHITE: Color32 = Color32::new(209, 209, 214, 255);
pub const DROPDOWN_INACTIVE_TEXT: Color32 = Color32::new(173, 173, 179, 255);

// ── Status colors ───────────────────────────────────────────────────
pub const STATUS_GOOD: Color32 = GREEN_ACTIVE;
pub const STATUS_WARNING: Color32 = AMBER_ACTIVE;
pub const STATUS_BAD: Color32 = RED_ACTIVE;
pub const STATUS_NEUTRAL: Color32 = Color32::new(184, 184, 189, 255);
pub const STATUS_ACTIVE: Color32 = ORANGE_ACTIVE;
pub const STATUS_OFF: Color32 = Color32::new(89, 89, 94, 255);
pub const STATUS_DOT_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);
pub const STATUS_DOT_GREEN: Color32 = GREEN_ACTIVE;
pub const STATUS_DOT_YELLOW: Color32 = AMBER_BASE;

// ── Transport colors ────────────────────────────────────────────────
pub const PLAY_GREEN: Color32 = GREEN_IDLE;
pub const PLAY_ACTIVE: Color32 = GREEN_ACTIVE;
pub const PAUSED_YELLOW: Color32 = AMBER_BASE;
pub const STOP_RED: Color32 = RED_BASE;
pub const RECORD_RED: Color32 = RED_IDLE;
pub const RECORD_ACTIVE: Color32 = RED_ACTIVE;
/// §19 record pulse: while recording, the Record button breathes between these
/// two reds (a smooth sine, not a hard blink — the one functional motion in the
/// UI, an Ableton-style "recording now" cue that reads on stage without
/// strobing). They bracket the button's static active red (180,40,40).
pub const RECORD_PULSE_DIM: Color32 = Color32::new(150, 34, 34, 255);
pub const RECORD_PULSE_BRIGHT: Color32 = Color32::new(216, 60, 56, 255);
pub const SAVE_FLASH_GREEN: Color32 = GREEN_BASE;
pub const TRANSPORT_FIELD_BG: Color32 = Color32::new(40, 40, 42, 255);
pub const BPM_RESET_ACTIVE: Color32 = GREEN_IDLE;
pub const BPM_CLEAR_ACTIVE: Color32 = RED_BASE;
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

// ── Mute / Solo / LED / Analysis ────────────────────────────────────
pub const MUTED_COLOR: Color32 = Color32::new(255, 0, 0, 255);
pub const SOLO_COLOR: Color32 = Color32::new(3, 127, 252, 255);
pub const LED_COLOR: Color32 = Color32::new(0, 200, 80, 255);
/// Audio "analysis-only" output state: silent to master, still feeding the send.
/// Teal "listening" accent, distinct from mute (red) / solo (blue) / LED (green).
pub const ANALYSIS_COLOR: Color32 = Color32::new(0, 178, 170, 255);

// ── Effect rack ─────────────────────────────────────────────────────
pub const RACK_BORDER: Color32 = BORDER;
pub const RACK_BG: Color32 = BG_2;
pub const CARD_BORDER: Color32 = BORDER;
pub const RACK_HANDLE_BG: Color32 = Color32::new(37, 37, 43, 255);
pub const RACK_HANDLE_TEXT: Color32 = Color32::new(122, 128, 158, 255);
pub const EFFECT_HEADER_NAME: Color32 = Color32::new(184, 199, 235, 255);
pub const EFFECT_CARD_INNER_BG: Color32 = BG_0; // dark well, recessed in the card
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
pub const SELECTED_BORDER: Color32 = BLUE_BASE;
/// Selected-LAYER focus ring. A layer header's fill IS its identity colour, so
/// selection can't be a fill: a tint (`lighten`) vanishes against the colour, and
/// a blue ring vanishes on a blue layer. A bright near-white ring reads on every
/// hue and pairs with a small lift — the one distinct "this layer is selected"
/// signal on a coloured header. See `docs/TIMELINE_UI_REDESIGN.md` §H.
pub const SELECTED_LAYER_RING: Color32 = Color32::new(232, 240, 255, 255);
pub const SELECTED_LAYER_RING_WIDTH: f32 = 2.0;

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
pub const PARAM_GROUP_BG: Color32 = BG_3;

// ── Grid lines ──────────────────────────────────────────────────────
pub const GRID_BAR_LINE: Color32 = Color32::new(107, 107, 112, 128);
pub const GRID_BEAT_LINE: Color32 = Color32::new(82, 82, 87, 77);
pub const GRID_SUBDIVISION_LINE: Color32 = Color32::new(71, 71, 77, 38);
pub const GRID_SIXTEENTH_LINE: Color32 = Color32::new(71, 71, 77, 20);

// ── Layer color picker palette ──────────────────────────────────────
// 7 columns × 10 rows = 70 high-contrast colors. Modeled after Ableton's
// color grid: each column is a hue family, rows go from light → saturated → dark.
pub const COLOR_GRID_COLS: usize = 7;
pub const COLOR_GRID_ROWS: usize = 10;
pub const COLOR_GRID: [Color32; 70] = [
    // Row 0 — pastels (light, desaturated)
    Color32::new(255, 148, 148, 255), // light red
    Color32::new(255, 192, 120, 255), // light orange
    Color32::new(255, 235, 120, 255), // light yellow
    Color32::new(148, 235, 148, 255), // light green
    Color32::new(130, 220, 235, 255), // light cyan
    Color32::new(148, 168, 255, 255), // light blue
    Color32::new(210, 148, 255, 255), // light purple
    // Row 1 — warm vivids
    Color32::new(255, 105, 105, 255), // coral red
    Color32::new(255, 160, 70, 255),  // warm orange
    Color32::new(255, 220, 50, 255),  // golden yellow
    Color32::new(100, 220, 100, 255), // spring green
    Color32::new(70, 200, 220, 255),  // turquoise
    Color32::new(100, 140, 255, 255), // cornflower
    Color32::new(185, 105, 255, 255), // lavender
    // Row 2 — saturated brights
    Color32::new(255, 50, 50, 255),  // bright red
    Color32::new(255, 128, 0, 255),  // bright orange
    Color32::new(255, 200, 0, 255),  // bright yellow
    Color32::new(50, 200, 50, 255),  // bright green
    Color32::new(0, 180, 210, 255),  // bright cyan
    Color32::new(60, 110, 255, 255), // bright blue
    Color32::new(160, 60, 255, 255), // bright purple
    // Row 3 — pure saturated
    Color32::new(230, 25, 25, 255),  // pure red
    Color32::new(230, 105, 0, 255),  // pure orange
    Color32::new(230, 180, 0, 255),  // amber
    Color32::new(25, 180, 25, 255),  // pure green
    Color32::new(0, 155, 190, 255),  // teal
    Color32::new(35, 80, 230, 255),  // pure blue
    Color32::new(135, 35, 230, 255), // pure purple
    // Row 4 — mid-depth
    Color32::new(200, 20, 50, 255),  // crimson
    Color32::new(200, 85, 0, 255),   // burnt orange
    Color32::new(200, 160, 0, 255),  // gold
    Color32::new(20, 155, 40, 255),  // forest green
    Color32::new(0, 130, 165, 255),  // ocean
    Color32::new(25, 60, 200, 255),  // royal blue
    Color32::new(110, 25, 200, 255), // violet
    // Row 5 — deep saturated
    Color32::new(170, 15, 45, 255), // deep crimson
    Color32::new(170, 70, 0, 255),  // rust
    Color32::new(170, 140, 0, 255), // dark gold
    Color32::new(15, 130, 35, 255), // deep green
    Color32::new(0, 105, 140, 255), // deep teal
    Color32::new(20, 45, 170, 255), // navy blue
    Color32::new(90, 20, 170, 255), // deep violet
    // Row 6 — rich darks
    Color32::new(140, 10, 40, 255), // dark red
    Color32::new(140, 55, 0, 255),  // dark orange
    Color32::new(140, 115, 0, 255), // dark amber
    Color32::new(10, 105, 30, 255), // dark green
    Color32::new(0, 85, 115, 255),  // dark cyan
    Color32::new(15, 35, 140, 255), // dark blue
    Color32::new(75, 15, 140, 255), // dark purple
    // Row 7 — warm earth tones
    Color32::new(190, 100, 80, 255),  // terracotta
    Color32::new(175, 130, 80, 255),  // tan
    Color32::new(160, 160, 80, 255),  // olive
    Color32::new(80, 140, 100, 255),  // sage
    Color32::new(80, 130, 145, 255),  // dusty teal
    Color32::new(100, 110, 160, 255), // slate blue
    Color32::new(145, 100, 160, 255), // dusty purple
    // Row 8 — cool grays with hue
    Color32::new(160, 120, 120, 255), // warm gray
    Color32::new(150, 135, 110, 255), // khaki
    Color32::new(140, 140, 110, 255), // sage gray
    Color32::new(110, 140, 120, 255), // green gray
    Color32::new(110, 130, 140, 255), // blue gray
    Color32::new(120, 120, 150, 255), // cool gray
    Color32::new(140, 120, 150, 255), // purple gray
    // Row 9 — near-neutrals
    Color32::new(220, 220, 220, 255), // white
    Color32::new(180, 180, 180, 255), // light gray
    Color32::new(140, 140, 140, 255), // mid gray
    Color32::new(100, 100, 100, 255), // dark gray
    Color32::new(65, 65, 65, 255),    // charcoal
    Color32::new(40, 40, 40, 255),    // near-black
    Color32::new(255, 50, 180, 255),  // hot pink
];

// ── Legacy layer palette (overview strip fallback) ─────────────────
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

// ── Color grid layout constants ────────────────────────────────────
pub const COLOR_SWATCH_SIZE: f32 = 16.0;
pub const COLOR_SWATCH_GAP: f32 = 3.0;

// ── Tempo lane ──────────────────────────────────────────────────────
pub const TEMPO_LINE: Color32 = Color32::new(64, 199, 199, 166);
pub const TEMPO_POINT: Color32 = Color32::new(230, 230, 235, 242);

// ── Monitor / Export ────────────────────────────────────────────────
pub const MONITOR_ACTIVE: Color32 = GREEN_IDLE;
pub const EXPORT_ACTIVE: Color32 = RED_BASE;

// ── Mute/Solo buttons ───────────────────────────────────────────────
pub const MUTE_BTN_ACTIVE: Color32 = ORANGE_BASE;
pub const SOLO_BTN_ACTIVE: Color32 = AMBER_ACTIVE;
pub const MUTE_SOLO_BTN_INACTIVE: Color32 = Color32::new(64, 64, 69, 255);

// ── Ruler ───────────────────────────────────────────────────────────
pub const RULER_BG: Color32 = Color32::new(102, 102, 102, 255);

// ── Dropdown item states ────────────────────────────────────────────
pub const DROPDOWN_ITEM_SELECTED: Color32 = Color32::new(45, 65, 95, 255);
pub const DROPDOWN_CHECK_COLOR: Color32 = Color32::new(100, 180, 255, 255);
pub const DROPDOWN_BORDER: Color32 = BORDER;

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
pub const OVERVIEW_VIEWPORT: Color32 = Color32::new(89, 148, 235, 120);
pub const OVERVIEW_VIEWPORT_BORDER: Color32 = Color32::new(120, 170, 245, 200);
pub const OVERVIEW_PLAYHEAD: Color32 = RED_ACTIVE;
pub const EXPORT_MARKER_COLOR: Color32 = Color32::new(77, 141, 235, 255);
pub const EXPORT_RANGE_HIGHLIGHT: Color32 = Color32::new(77, 140, 235, 31);

// ── Timeline Markers ────────────────────────────────────────────────
pub const MARKER_RED: Color32 = Color32::new(217, 64, 56, 255);
pub const MARKER_ORANGE: Color32 = Color32::new(230, 150, 50, 255);
pub const MARKER_YELLOW: Color32 = Color32::new(230, 210, 60, 255);
pub const MARKER_GREEN: Color32 = Color32::new(80, 190, 80, 255);
pub const MARKER_CYAN: Color32 = Color32::new(70, 200, 210, 255);
pub const MARKER_BLUE: Color32 = Color32::new(89, 148, 235, 255);
pub const MARKER_PURPLE: Color32 = Color32::new(170, 100, 230, 255);
pub const MARKER_WHITE: Color32 = Color32::new(220, 220, 225, 255);
pub const MARKER_LINE_ALPHA: u8 = 128;
pub const MARKER_FLAG_WIDTH: f32 = 8.0;
pub const MARKER_FLAG_HEIGHT: f32 = 14.0;
pub const MARKER_LINE_WIDTH: f32 = 1.0;
pub const MARKER_LABEL_WIDTH: f32 = 60.0;
pub const MARKER_LABEL_HEIGHT: f32 = 12.0;
pub const MARKER_LABEL_BG: Color32 = Color32::new(40, 40, 40, 200);
pub const MARKER_SELECTED_OUTLINE: Color32 = Color32::new(255, 255, 255, 200);

pub fn marker_color_to_color32(color: crate::types::MarkerColor) -> Color32 {
    use crate::types::MarkerColor;
    match color {
        MarkerColor::Red => MARKER_RED,
        MarkerColor::Orange => MARKER_ORANGE,
        MarkerColor::Yellow => MARKER_YELLOW,
        MarkerColor::Green => MARKER_GREEN,
        MarkerColor::Cyan => MARKER_CYAN,
        MarkerColor::Blue => MARKER_BLUE,
        MarkerColor::Purple => MARKER_PURPLE,
        MarkerColor::White => MARKER_WHITE,
    }
}

// ── Bitmap Panel Common ─────────────────────────────────────────────
pub const TRANSPARENT: Color32 = Color32::new(0, 0, 0, 0);
pub const HOVER_OVERLAY: Color32 = Color32::new(255, 255, 255, 15);
pub const PRESS_OVERLAY: Color32 = Color32::new(255, 255, 255, 25);
pub const PANEL_BG_DARK: Color32 = BG_1;
pub const TEXT_PRIMARY_C32: Color32 = Color32::new(230, 230, 235, 255); // §A: synced w/ TEXT_NORMAL
pub const TEXT_WHITE_C32: Color32 = Color32::new(255, 255, 255, 255);
pub const TEXT_LIGHT_C32: Color32 = Color32::new(226, 226, 231, 255); // §A
pub const TEXT_DIMMED_C32: Color32 = Color32::new(178, 178, 184, 255); // §A: synced w/ TEXT_DIMMED
pub const DIVIDER_C32: Color32 = DIVIDER;

// ── Bitmap Slider Palette ───────────────────────────────────────────
// The track is a DARK RECESSED WELL — darker than the card it sits on — so the
// bright fill (the lane hue) reads as light-in-a-groove. This inverts the old
// scheme (track = BG_3, *lighter* than the card), which is what made the slider
// muddy: "grey fill on grey track on grey card." The value box shares this well
// colour, so the number reads as a bright glyph in the same recess.
pub const SLIDER_TRACK_C32: Color32 = Color32::new(12, 14, 19, 255);
pub const SLIDER_TRACK_HOVER_C32: Color32 = Color32::new(18, 20, 27, 255);
pub const SLIDER_TRACK_PRESSED_C32: Color32 = Color32::new(8, 9, 13, 255);
/// Default fill for sliders with no lane identity (master-scope cards, macros).
/// Opaque + bright so it reads on the dark well; layer/generator param cards
/// override this with the selected layer's hue.
pub const SLIDER_FILL_C32: Color32 = Color32::new(89, 148, 235, 255);
pub const SLIDER_THUMB_C32: Color32 = Color32::new(255, 255, 255, 255);
pub const SLIDER_TEXT_C32: Color32 = Color32::new(255, 255, 255, 255);

// ── Bitmap Toggle / Accent ──────────────────────────────────────────
pub const ACCENT_BLUE_C32: Color32 = Color32::new(89, 148, 235, 255);
pub const ACCENT_BLUE_HOVER_C32: Color32 = Color32::new(109, 168, 255, 255);
pub const ACCENT_BLUE_PRESS_C32: Color32 = Color32::new(69, 128, 215, 255);
pub const BUTTON_INACTIVE_C32: Color32 = Color32::new(59, 59, 61, 255);
pub const BUTTON_INACTIVE_HOVER_C32: Color32 = Color32::new(74, 74, 76, 255);
pub const BUTTON_INACTIVE_PRESS_C32: Color32 = Color32::new(49, 49, 51, 255);

// ── Bitmap Driver / Envelope Indicators ─────────────────────────────
pub const DRIVER_ACTIVE_C32: Color32 = Color32::new(20, 166, 191, 255);
// (DRIVER_ACTIVE_HOVER/PRESS removed — config buttons now derive hover/press
//  from DRIVER_ACTIVE via the state-button skin, matching the colored variant.)
pub const DRIVER_INACTIVE_C32: Color32 = Color32::new(72, 72, 78, 255);
pub const DRIVER_INACTIVE_HOVER_C32: Color32 = Color32::new(87, 87, 93, 255);
pub const DRIVER_INACTIVE_PRESS_C32: Color32 = Color32::new(62, 62, 68, 255);
pub const ENVELOPE_ACTIVE_C32: Color32 = Color32::new(191, 115, 20, 255);
pub const ENVELOPE_ACTIVE_HOVER_C32: Color32 = Color32::new(211, 135, 40, 255);
pub const ENVELOPE_ACTIVE_PRESS_C32: Color32 = Color32::new(171, 95, 10, 255);

// ── Bitmap Config Drawer ────────────────────────────────────────────
pub const CONFIG_BG_C32: Color32 = BG_2;
pub const CONFIG_BTN_INACTIVE_C32: Color32 = Color32::new(44, 44, 48, 255);
pub const CONFIG_BTN_HOVER_C32: Color32 = Color32::new(54, 54, 58, 255);
pub const CONFIG_BTN_PRESSED_C32: Color32 = Color32::new(38, 38, 42, 255);

// ── Bitmap Trim / Target Handles ────────────────────────────────────
pub const TRIM_FILL_C32: Color32 = Color32::new(20, 166, 191, 38);
pub const TRIM_BAR_HOVER_C32: Color32 = Color32::new(40, 186, 211, 255);
pub const TARGET_BAR_HOVER_C32: Color32 = Color32::new(211, 135, 40, 255);

// Ableton trim handles — purple tint to distinguish from driver cyan
pub const ABL_TRIM_BAR_C32: Color32 = Color32::new(140, 80, 200, 255);
pub const ABL_TRIM_BAR_HOVER_C32: Color32 = Color32::new(165, 105, 225, 255);
pub const ABL_TRIM_FILL_C32: Color32 = Color32::new(140, 80, 200, 38);
pub const ABL_BADGE_C32: Color32 = Color32::new(140, 80, 200, 255);

// Audio-modulation trim handles + active tint — a clean green, kept distinct
// from the driver's teal so both handle sets read apart when drawn on one
// slider at once. Mirrors the driver/Ableton trim constant trio.
pub const AUDIO_TRIM_BAR_C32: Color32 = Color32::new(72, 199, 116, 255);
pub const AUDIO_TRIM_BAR_HOVER_C32: Color32 = Color32::new(97, 219, 141, 255);
pub const AUDIO_TRIM_FILL_C32: Color32 = Color32::new(72, 199, 116, 38);
/// Pink badge + header tint for effect cards whose per-card graph
/// override (`PresetInstance.graph`) is set. Visually distinct from
/// DRV/ENV/ABL.
pub const MOD_BADGE_C32: Color32 = Color32::new(220, 60, 140, 255);
pub const MOD_HEADER_BG_C32: Color32 = Color32::new(70, 30, 50, 255);

// ── Inspector identity accent ───────────────────────────────────────
// ONE colour themes the inspector — card headers, the selected-card border, the
// active tab. Deliberately NOT the per-layer lane hue: inheriting the lane made
// inspector colour meaningless (every layer a different random hue that encodes
// nothing). One consistent accent instead. Slider fills keep SLIDER_FILL_C32,
// which already resolves to this same blue.
pub const INSPECTOR_ACCENT: Color32 = ACCENT_BLUE_C32;
// Card header fill — a DEEPENED accent (same hue, darker) so WHITE header text
// reads cleanly. The bright accent left auto-contrast text muddy (dark text on a
// mid-bright blue), and the bright bar itself read loud; the deep blue fixes both.
pub const INSPECTOR_HEADER_BG: Color32 = Color32::new(44, 74, 122, 255);

// ── Bitmap Effect Card ──────────────────────────────────────────────
pub const EFFECT_CARD_INNER_BG_C32: Color32 = BG_0; // dark well, recessed in the card
pub const CARD_BORDER_C32: Color32 = BORDER;
/// §19 focus: the edited object lifts one ramp step so it reads first — the
/// inspector card's inner well (BG_0→~BG_1) and the timeline's focused lane
/// (BG_2→~BG_3) get the *same* lift. Hue-preserving (saturating per-channel add),
/// so it works on the effect's neutral well and the generator's purple-tinted
/// one alike.
pub const FOCUS_LIFT_STEP: u8 = 9;
pub const DRAG_HANDLE_BG_C32: Color32 = Color32::new(38, 38, 42, 255);
pub const DRAG_HANDLE_HOVER_BG_C32: Color32 = Color32::new(52, 52, 56, 255);

// ── Bitmap Generator Card (purple-tinted to distinguish from effect cards) ───
pub const GEN_CARD_BORDER_C32: Color32 = Color32::new(58, 45, 70, 255);
pub const GEN_CARD_INNER_BG_C32: Color32 = Color32::new(20, 18, 24, 255);
pub const GEN_CARD_HEADER_BG_C32: Color32 = Color32::new(38, 32, 48, 255);
pub const GEN_CARD_HEADER_HOVER_C32: Color32 = Color32::new(50, 42, 62, 255);
pub const GEN_CARD_HEADER_NAME_C32: Color32 = Color32::new(185, 160, 220, 255);

// ── Panel-specific colors ────────────────────────────────────────────

// Header panel
pub const HEADER_BUTTON_DIM: Color32 = Color32::new(71, 71, 74, 255);
pub const HEADER_BUTTON_HOVER: Color32 = Color32::new(90, 90, 94, 255);
pub const HEADER_BUTTON_PRESSED: Color32 = Color32::new(55, 55, 58, 255);
pub const HEADER_BUTTON_ACTIVE: Color32 = Color32::new(89, 173, 232, 255);
pub const HEADER_BUTTON_ACTIVE_HOVER: Color32 = Color32::new(110, 190, 240, 255);
pub const HEADER_BUTTON_ACTIVE_PRESSED: Color32 = Color32::new(70, 150, 210, 255);
pub const HEADER_PROGRESS_FILL: Color32 = Color32::new(89, 173, 232, 255);

// Graph editor header pills (Save to Library / Save to Project / Push to
// Library — PRESET_LIBRARY_DESIGN D4/D3, P3/P4). Token home per the
// design-token guard (`tests/design_tokens.rs`) — `graph_canvas/mod.rs`
// re-exports this rather than defining its own raw literal.
pub const GRAPH_SAVE_BUTTON_BG: Color32 = Color32::new(74, 96, 138, 230);

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

// ── Header-control chip (§C / §K) ───────────────────────────────────
// A control sitting on a layer header sits on the layer's IDENTITY colour, so
// it must use an opaque NEUTRAL chip — never a tint of the hue (a darken of the
// hue is just darker hue → reads hue-on-hue, the low-contrast trap §C names).
// One dark neutral chip + a white hairline reads cleanly on any identity colour
// (Ableton's pattern). Drives the type badge, M/S/L, blend, and routing-value
// chips. Distinct from BUTTON_DIM (the grey chrome-bar chip) on purpose.
pub const CHIP_BG: Color32 = Color32::new(27, 27, 33, 255);
pub const CHIP_BG_HOVER: Color32 = Color32::new(40, 40, 47, 255);
pub const CHIP_BG_PRESSED: Color32 = Color32::new(20, 20, 25, 255);
/// Hairline edge on a chip — a low-alpha white so the chip separates from the
/// identity colour behind it without a hard line.
pub const CHIP_LINE: Color32 = Color32::new(255, 255, 255, 41);
/// Hairline for a tonal header chip on a *light* identity header (§ layer-header
/// restyle): a faint dark line that re-seats the chip where the old white stroke
/// would have glared. Omitted on dark headers, where the darkened chip separates
/// on its own. ~35% black.
pub const CHIP_LINE_DARK: Color32 = Color32::new(0, 0, 0, 90);
/// Corner radius for header-control chips. The mockup rounds every header chip
/// to 4px; kept distinct from the 2px inspector `SMALL_RADIUS`.
pub const CHIP_RADIUS: f32 = 4.0;
/// Dropdown-caret affordance on a value chip (mockup `.sel::after`): a dim ▼
/// pinned to the chip's right edge. Dimmer + smaller than the value text so it
/// reads as an affordance, not content. Painted by the renderer when
/// `UIStyle::dropdown_caret` is set.
pub const CHIP_CARET: Color32 = Color32::new(255, 255, 255, 150);
/// Caret glyph point size (the value text is `FONT_SMALL` 9px; the caret sits one
/// step down, matching the mockup's 9px-caret-on-10.5px-text proportion).
pub const CHIP_CARET_FONT: u16 = 8;
/// Inset of the caret's right edge from the chip's right edge (the chip's own
/// right padding).
pub const CHIP_CARET_PAD_X: f32 = 7.0;
/// Internal horizontal padding for chip text — the mockup `.sel{padding:2px 7px}`
/// so value/label text sits off the border, symmetric with the caret's right pad.
pub const CHIP_TEXT_INSET_X: f32 = 7.0;
/// Dim leading micro-label on a label/value chip (mockup `.blend b`) — e.g. the
/// "BLEND" before the mode. Same dim as the caret so the chip's two secondary
/// affordances (leading label, trailing caret) read at one weight.
pub const CHIP_PREFIX: Color32 = Color32::new(255, 255, 255, 150);
pub const LAYER_CHEVRON_HOVER: Color32 = Color32::new(255, 255, 255, 15);
pub const LAYER_CHEVRON_PRESSED: Color32 = Color32::new(255, 255, 255, 8);

// Viewport panel
pub const CLIP_LABEL_BG: Color32 = Color32::new(20, 20, 22, 255);
pub const CLIP_LABEL_BG_HOVER: Color32 = Color32::new(20, 20, 22, 220);

// Dropdown panel (lightweight popup — barely dims; see `panels::popup_shell`).
pub const DROPDOWN_SCRIM: Color32 = Color32::new(0, 0, 0, 1);

// Modal popup shell (Ableton picker / browser) — a darker well behind a dimming
// scrim, so the modal pulls focus off the rest of the screen. Hoisted from the
// per-file `BG_BORDER`/`BG_INNER` + inline scrim literals the two pickers each
// carried; one definition now, consumed via `PopupStyle::MODAL`.
pub const MODAL_SCRIM: Color32 = Color32::new(0, 0, 0, 80);
pub const MODAL_BG: Color32 = Color32::new(19, 19, 20, 250);
pub const MODAL_BORDER: Color32 = Color32::new(48, 48, 52, 255);

// Browser popup image cells (PRESET_LIBRARY_DESIGN P6, D7) — a thumbnail-
// filled cell needs translucent (not opaque) hover/press tints so the picture
// stays visible under the interaction feedback, plus a dark caption strip so
// the label reads over arbitrary thumbnail content.
pub const BROWSER_CELL_HOVER_OVER_IMAGE: Color32 = Color32::new(255, 255, 255, 40);
pub const BROWSER_CELL_PRESSED_OVER_IMAGE: Color32 = Color32::new(0, 0, 0, 60);
pub const BROWSER_CELL_CAPTION_BG: Color32 = Color32::new(0, 0, 0, 150);
/// Origin/source badge text on a browser cell (Factory / My Library / …).
pub const BROWSER_CELL_BADGE_TEXT: Color32 = Color32::new(130, 130, 134, 255);

// ── Interaction thresholds ──────────────────────────────────────────
pub const DRAG_THRESHOLD_PX: f32 = 4.0;
pub const DOUBLE_CLICK_TIME_SEC: f32 = 0.3;
/// Max screen-space distance (px) between the two presses of a double-click.
/// A drag further than this is two separate single-clicks, not a double.
/// Equal to `DRAG_THRESHOLD_PX` by design (UI_WIDGET_UNIFICATION P4/I8) — one
/// constant home for every gesture-timing/radius threshold in the app.
pub const DOUBLE_CLICK_RADIUS_PX: f32 = DRAG_THRESHOLD_PX;
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
// The bottom status bar is the counterpart to the transport bar — same height
// so they read as one deliberate top/bottom chrome frame (and their buttons
// match). Locked to TRANSPORT_BAR_HEIGHT rather than a loose literal.
pub const FOOTER_HEIGHT: f32 = TRANSPORT_BAR_HEIGHT;
// ── Track-row height presets (§24 5d) ───────────────────────────────
// One named tier per content density, selected by display *state* — never by
// layer *type* (type is shown by a badge, not by restructuring the header). See
// `coordinate_mapper::TrackHeight`.
/// Expanded: the default track — clip bodies + content + routing form. 200px
/// per the redesign spec §B (Peter: focusing on a single layer is intended).
pub const TRACK_HEIGHT: f32 = 200.0;
/// Collapsed (a.k.a. compact): identity + mix row, no routing. The two-tier
/// system is collapsed↔expanded; 58px gives the name + M/S/L row breathing room.
pub const COLLAPSED_TRACK_HEIGHT: f32 = 58.0;
/// Tall: a roomier track for larger previews. Reserved for a future per-layer
/// tall mode; the preset exists so the height vocabulary is complete.
pub const TALL_TRACK_HEIGHT: f32 = 200.0;

// ── Automation lane strips (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) ──────
// Engaging automation mode grows a content track by one strip per enabled
// lane — `CoordinateMapper::layer_height` is the single place this height
// actually applies (never re-derived elsewhere).
pub const AUTOMATION_LANE_STRIP_HEIGHT: f32 = 28.0;
/// Strip background — a subtle recess so the lane reads as its own row
/// within the taller track, distinct from the routing-form area above it.
pub const AUTOMATION_STRIP_BG: Color32 = Color32::new(20, 20, 22, 255);
/// The breakpoint line + dots, Live's exact affordance: red while live.
pub const AUTOMATION_LINE_COLOR: Color32 = RED_ACTIVE;
/// The breakpoint line + dots when the lane's param is overridden (a live
/// touch latched it) — grayed instead of red, Live's exact affordance.
pub const AUTOMATION_LINE_OVERRIDDEN_COLOR: Color32 = Color32::new(120, 120, 126, 255);
pub const AUTOMATION_LINE_THICKNESS: f32 = 1.6;
pub const AUTOMATION_DOT_RADIUS: f32 = 3.0;
pub const AUTOMATION_LABEL_COLOR: Color32 = TEXT_DIMMED_C32;
pub const AUTOMATION_LABEL_FONT: u16 = FONT_SMALL;

pub const RULER_HEIGHT: f32 = 40.0;
// §K1: header column widened 200→230 to match the mockup grid (room for the
// 18px type-badge chip + name + menu in the identity row without crushing).
pub const LAYER_CONTROLS_WIDTH: f32 = 230.0;
pub const PLAYHEAD_WIDTH: f32 = 2.0;
/// Size of the playhead head marker — a downward triangle at the top of the
/// ruler that makes the "now" position unmissable next to the insert cursor (§24 5e).
pub const PLAYHEAD_HEAD_SIZE: f32 = 13.0;
pub const CLIP_MIN_WIDTH: f32 = 10.0;
// §K14: tighter clip inset (12→6) so clip cards fill more of the lane, matching
// the mockup's `top:6px; bottom:6px`. One token → viewport rects, hit-test, and
// the GPU clip pass stay in agreement.
pub const CLIP_VERTICAL_PAD: f32 = 6.0;
pub const OVERVIEW_STRIP_HEIGHT: f32 = 16.0;
// ── Timeline horizontal scrollbar (§24 5e) ──────────────────────────
/// Height of the reserved scrollbar strip at the bottom of the timeline body.
pub const TIMELINE_SCROLLBAR_HEIGHT: f32 = 11.0;
/// Minimum thumb length so it stays grabbable when the content is very long.
pub const TIMELINE_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// Inset of the thumb within the track (top/bottom + ends) so it reads as a
/// floating pill, not a full-height block.
pub const TIMELINE_SCROLLBAR_THUMB_INSET: f32 = 2.0;
// Floor wide enough that a full param row — label + slider track + value field
// + the D/E modulation buttons — keeps its columns instead of crushing the
// track to nothing. Below this the card is unreadable on stage.
pub const MIN_INSPECTOR_WIDTH: f32 = 232.0;
pub const MAX_INSPECTOR_WIDTH: f32 = 900.0;
pub const DEFAULT_INSPECTOR_WIDTH: f32 = 500.0;
pub const INSPECTOR_RESIZE_HANDLE_WIDTH: f32 = 6.0;
pub const INSPECTOR_GAP: f32 = 4.0;

// ── Audio Setup dock (AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN D1) ──
// The Audio Setup panel is a fold-out column pinned to the inspector's LEFT
// edge; it expands leftward when opened, shrinking preview + timeline (never
// the inspector). 0.0 = closed. Default-open width sized for the panel's
// control rows (device / send / gain / stereo / scope) to read without
// crushing — the min floor keeps the send row's columns intact.
pub const DEFAULT_AUDIO_SETUP_WIDTH: f32 = 460.0;
pub const MIN_AUDIO_SETUP_WIDTH: f32 = 340.0;
pub const MAX_AUDIO_SETUP_WIDTH: f32 = 720.0;

// ── Scene Setup dock (SCENE_SETUP_PANEL_DESIGN D2) ──
// Cloned from the Audio Setup dock above: a fold-out column pinned to the
// inspector's LEFT edge, mutually exclusive with the Audio Setup dock (only
// one of the two utility columns is ever open at once).
pub const DEFAULT_SCENE_SETUP_WIDTH: f32 = 400.0;
pub const MIN_SCENE_SETUP_WIDTH: f32 = 320.0;
pub const MAX_SCENE_SETUP_WIDTH: f32 = 640.0;

// ── Group layout ────────────────────────────────────────────────────
pub const GROUP_CHILD_INDENT_PX: f32 = 20.0;
/// A group row is a structural container header (no clip bodies of its own), so
/// it gets one fixed height in every state — collapsed or expanded. Not a content
/// track, so it sits outside the `TrackHeight` preset tiers.
pub const GROUP_TRACK_HEIGHT: f32 = 70.0;
pub const GROUP_ACCENT_BAR_WIDTH: f32 = 5.0;
pub const GROUP_BOTTOM_BORDER_HEIGHT: f32 = 2.0;

// ── Spacing scale ───────────────────────────────────────────────────
// 4px base rhythm: 4 / 8 / 12 / 16 / 24. XS (2) is a half-step kept only for
// genuinely tight chrome. One rhythm everywhere — no ad-hoc paddings.
pub const SPACE_XS: f32 = 2.0;
pub const SPACE_S: f32 = 4.0;
pub const SPACE_M: f32 = 8.0;
pub const SPACE_L: f32 = 12.0;
pub const SPACE_XL: f32 = 16.0;
pub const SPACE_XXL: f32 = 24.0;

// The single left edge every inspector section-content row aligns to (§14.2
// rule 1), measured from the card/chrome rect. A bordered param card reaches it
// as `1px frame border + SPACE_M`; the border-less chrome panels (master / layer
// / clip) use it directly as their horizontal pad, so chrome controls share one
// column with the cards' param labels instead of staggering 2px-vs-7px.
pub const SECTION_CONTENT_INSET: f32 = SPACE_M + 1.0;

// The one card / section header-row height (§14.2 rule 5). Param cards and the
// master / layer chrome headers all reference this so they share one rhythm
// (was a 27.5-vs-28 half-pixel split).
pub const HEADER_ROW_HEIGHT: f32 = 28.0;

// ── Corner radii ────────────────────────────────────────────────────
// Small + consistent: ~3px controls, ~5px cards. Softens the hard
// rectangles without going consumer-app bubbly.
pub const BUTTON_RADIUS: f32 = 3.0;
pub const CARD_RADIUS: f32 = 5.0;
/// No rounding — a full-bleed rectangle (e.g. timeline rows that tile edge-to-edge
/// so a selection ring covers the whole border).
pub const SQUARE_RADIUS: f32 = 0.0;
pub const SMALL_RADIUS: f32 = 2.0;
pub const POPUP_RADIUS: f32 = 6.0;
/// the dropdown/menu-item hover-or-checked highlight —
/// up from `SMALL_RADIUS` now that unchecked rows are transparent (see
/// `DropdownPanel::build_nodes`) and the highlight needs to read as a
/// distinct rounded chip rather than a flush-edged box.
pub const MENU_ITEM_RADIUS: f32 = 4.0;
// The §14.2-rule-6 hairline exception, realised as a token instead of scattered
// `1.0` literals: thin bars, tracks, fills, and ≤6px overlay handles that read
// crisper left near-square (slider track, progress fill, drop indicator, the
// trim/target/scale bars). A named token so the guard (§16) stays at zero.
pub const HAIRLINE_RADIUS: f32 = 1.0;

// ── Motion ────────────────────────────────────────────────────────────
// The whole motion vocabulary (`UI_CRAFT_AND_MOTION_PLAN.md` D1/D15): three
// durations + one overshoot magnitude. `crate::anim::AnimF32` reads these;
// no widget picks a bespoke duration — three tokens or the system rots.
/// Hover/press feedback — the fastest, most-felt tween in the kit.
pub const MOTION_FAST_MS: f32 = 90.0;
/// Drawers, tab ink, card collapse.
pub const MOTION_MED_MS: f32 = 160.0;
/// Value flash, toast.
pub const MOTION_SLOW_MS: f32 = 240.0;
/// D15 magnetic-snap curve (`Curve::Snap`): back-out overshoot constant.
/// `3.0` peaks the ease at exactly 25% overshoot past the target (verified
/// numerically in `anim::tests::snap_curve_overshoots_by_roughly_25_percent`)
/// — the doc's "≈25% overshoot". Confirmed at P2 entry (Peter's playground
/// default, 25%) — do not retune without a new playground session.
pub const EASE_SNAP_BACK_C1: f32 = 3.0;
/// D15 magnetic-snap radius: screen-space px within which a drag target
/// (clip edge → beat grid, wire end → port) snaps instead of tracking the
/// cursor directly. Confirmed at P2 entry (Peter's playground default, 14px).
pub const MAGNET_RADIUS_PX: f32 = 14.0;

// ── Font sizes ──────────────────────────────────────────────────────
// Semantic scale — all panel font sizes should reference these.
pub const FONT_CAPTION: u16 = 8; // tiny badges, config buttons
pub const FONT_SMALL: u16 = 9; // layer info, perf hud, ruler
pub const FONT_BODY: u16 = 10; // sliders, params, chrome, buttons
pub const FONT_LABEL: u16 = 11; // layer names, footer, search, clip names
pub const FONT_SUBHEADING: u16 = 12; // chrome headings, transport buttons
pub const FONT_HEADING: u16 = 14; // section titles, drag handle
pub const FONT_TITLE: u16 = 16; // top-level headings

pub const FONT_WEIGHT_DEFAULT: crate::node::FontWeight = crate::node::FontWeight::Medium;

// ── Zoom levels (pixels per beat) ───────────────────────────────────
pub const ZOOM_LEVELS: [f32; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 80.0, 120.0, 200.0, 400.0];
pub const DEFAULT_ZOOM_INDEX: usize = 7; // 120 pixels/beat

// ── Scroll ──────────────────────────────────────────────────────────
pub const SCROLL_SENSITIVITY: f32 = 1.0;
pub const BITMAP_SCROLL_SPEED: f32 = 12.5;
/// Continuous cursor-anchored zoom (§24 5e): the zoom multiplier applied per
/// wheel notch. Each notch scales pixels-per-beat by this factor (or its inverse
/// when zooming out), so zoom is smooth instead of jumping between fixed levels.
pub const ZOOM_WHEEL_STEP_PER_NOTCH: f32 = 1.18;

// ── Layer control panel layout ──────────────────────────────────────
pub const LAYER_CTRL_PADDING: f32 = SPACE_M; // mockup edge gutter (8px) — tracks breathe
pub const LAYER_CTRL_CHEVRON_WIDTH: f32 = 18.0;
pub const LAYER_CTRL_DRAG_HANDLE_WIDTH: f32 = 18.0;
/// Square type-badge chip in the layer name row (§24 5d / §K3). 18px to match
/// the mockup badge — a filled chip the glyph sits inside, not a bare glyph.
pub const LAYER_CTRL_TYPE_BADGE_SIZE: f32 = 18.0;
pub const LAYER_CTRL_NAME_ROW_HEIGHT: f32 = 18.0;
pub const LAYER_CTRL_ROW_STEP: f32 = 23.0;
// §K5: M/S/L chips narrowed 28→20 to match the mockup iconbtn (frees width for
// the blend chip + keeps the mix row uncrowded in the 230px column).
pub const LAYER_CTRL_MUTE_SOLO_BTN_WIDTH: f32 = 20.0;
pub const LAYER_CTRL_BTN_HEIGHT: f32 = 18.0;
pub const LAYER_CTRL_INFO_ROW_HEIGHT: f32 = 14.0;
pub const LAYER_CTRL_SEPARATOR_HEIGHT: f32 = 2.0;
pub const LAYER_CTRL_RIGHT_GUTTER: f32 = 10.0;
pub const LAYER_CTRL_TOP_ROW_GAP: f32 = 6.0; // mockup htop gap (chevron→badge→name)
pub const LAYER_CTRL_FOLDER_BTN_WIDTH: f32 = 42.0;
pub const LAYER_CTRL_NEW_CLIP_BTN_WIDTH: f32 = 62.0;
pub const LAYER_CTRL_ADD_GEN_CLIP_BTN_WIDTH: f32 = 50.0;
pub const LAYER_CTRL_GEN_TYPE_ROW_HEIGHT: f32 = 14.0;
pub const LAYER_CTRL_MIDI_LABEL_WIDTH: f32 = 30.0;
pub const LAYER_CTRL_CHANNEL_LABEL_WIDTH: f32 = 20.0;
pub const LAYER_CTRL_SMALL_FONT_SIZE: u16 = 9;
pub const LAYER_CTRL_NAME_FONT_SIZE: u16 = 11;
pub const LAYER_CTRL_HANDLE_FONT_SIZE: u16 = 14;

// ── Text editing (UI_WIDGET_UNIFICATION P5) ────────────────────────
// The `TextEditModel`-backed caret + ranged-selection highlight, drawn by
// every host that embeds the model (`manifold-app`'s `TextInputState` has
// its own equal-valued consts since it's a different crate; `MappingPopover`
// here references these directly, I7's "one editing home" extended to the
// paint colours too).
pub const TEXT_EDIT_SELECT_BG: Color32 = Color32::new(77, 128, 204, 102);
pub const TEXT_EDIT_CARET: Color32 = Color32::new(224, 224, 224, 255);
