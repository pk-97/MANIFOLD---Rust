use crate::{EditingAction, LayerAction, ProjectAction, RootAction};
use super::{PanelAction, ScrubPhase, ScrubValue, ValueRef};
use crate::chrome::{ChromeHost, Pad, Sizing, View, components};
use crate::color::{self, darken, lighten};
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::scroll_container::ScrollContainer;
use crate::tree::UITree;
use manifold_foundation::LayerId;
use crate::types::note_number_to_name;
use std::time::Instant;

// Stable keys for the host-owned top chrome (background + recording controls).
const KEY_BG: u64 = 80_001;
const KEY_RECORD: u64 = 80_002;
const KEY_DEVICE: u64 = 80_003;

// ── Layout constants (from LayerHeaderLayout.cs / UIConstants) ───────

const PAD: f32 = color::LAYER_CTRL_PADDING;
const CHEVRON_W: f32 = color::LAYER_CTRL_CHEVRON_WIDTH;
const HANDLE_W: f32 = color::LAYER_CTRL_DRAG_HANDLE_WIDTH;
const NAME_H: f32 = color::LAYER_CTRL_NAME_ROW_HEIGHT;
const ROW_STEP: f32 = color::LAYER_CTRL_ROW_STEP;
const MS_BTN_W: f32 = color::LAYER_CTRL_MUTE_SOLO_BTN_WIDTH;
const BTN_H: f32 = color::LAYER_CTRL_BTN_HEIGHT;
const SEP_H: f32 = color::LAYER_CTRL_SEPARATOR_HEIGHT;
const RIGHT_GUTTER: f32 = color::LAYER_CTRL_RIGHT_GUTTER;
const TOP_GAP: f32 = color::LAYER_CTRL_TOP_ROW_GAP;
// Widths for the MIDI trigger-mode toggle and per-layer device dropdown
// packed into the existing MIDI / CH rows (no new row, preserves TRACK_HEIGHT).
const MODE_TOGGLE_W: f32 = 32.0;
/// §D routing form: one fixed label column for Folder/MIDI/Channel/Device so the
/// values align in a second column (wide enough for the spelled-out labels).
const LBL_W: f32 = 52.0;
/// Vertical gap between routing rows (Folder/MIDI/Channel/Device) — the mockup
/// `.rform{gap:9px}` airy rhythm. The old 2px read cramped now that the expanded
/// track is 200px. Used for the mix→routing gap too.
const ROUTING_ROW_GAP: f32 = 9.0;
/// P0.5 collapsed-card generator label: a fixed-width dimmed zone carved out
/// of the Name rect's right edge, so the row's total width never grows — the
/// generator name ellipsizes inside this budget instead
/// (`docs/TIMELINE_LAYOUT_P0_SPEC.md` P0.5).
const GEN_LABEL_COLLAPSED_W: f32 = 80.0;
const ACCENT_W: f32 = color::GROUP_ACCENT_BAR_WIDTH;
/// Width of the left-edge selection accent bar. The selected layer is marked by
/// this bright bar (DAW convention) plus a small brighten of its own colour —
/// not a full border box.
const SEL_ACCENT_W: f32 = 3.0;
const CHILD_INDENT: f32 = color::GROUP_CHILD_INDENT_PX;
const BORDER_H: f32 = color::GROUP_BOTTOM_BORDER_HEIGHT;

// ── Panel-specific colors ───────────────────────────────────────────

// Layer background now uses the layer color directly (no tinting into a dark base).
const SEP_COLOR: Color32 = color::SEPARATOR_COLOR;
const ACCENT_COLOR: Color32 = color::DEFAULT_GROUP_ACCENT;
const BORDER_CLR: Color32 = color::GROUP_BOTTOM_BORDER;
// Generator type text now uses contrast_text_color() based on layer bg.

const DRAG_SOURCE_DIM: Color32 = color::LAYER_DRAG_SOURCE_DIM;
const INSERT_LINE_CLR: Color32 = color::LAYER_INSERT_LINE;
const INSERT_LINE_H: f32 = 2.0;

// Timeline header text steps one rung up the type scale for stage legibility
// (Peter: the mockup reads easier — bigger name + chip text). Name 11→12,
// chip/label 9→10, button 10→11. Local to the layer header, not a UI-wide bump.
const NAME_FONT: u16 = color::FONT_SUBHEADING;
const SMALL_FONT: u16 = color::FONT_BODY;
const BTN_FONT: u16 = color::FONT_LABEL;
// §K6: header controls round to the 4px chip radius (the mockup rounds every
// header chip the same), distinct from the 2px inspector `SMALL_RADIUS`.
const LH_BTN_RADIUS: f32 = color::CHIP_RADIUS;
// The M/S/L/A toggles round harder than the dropdown chips — the mockup reads
// them as pills, distinct from the rectangular value/blend chips, so the toggle
// cluster is legible as a group of buttons rather than more dropdowns. 6 on the
// 18px button is rounded-pill, not a full capsule (which would be 9).
const MSL_PILL_RADIUS: f32 = 6.0;
/// Breathing room above and below the mix→routing divider (§K). Wider than the
/// inter-row gap so the rule reads as a deliberate section break between the
/// M/S/L/Blend mix row and the routing form, not just another row line.
const MIX_DIVIDER_PAD: f32 = 10.0;
const MIX_DIVIDER_THICK: f32 = 1.0;
/// §19 record pulse: one breathe (dim → bright → dim) per this many seconds. A
/// calm ~1 Hz cadence — present without strobing.
const RECORD_PULSE_PERIOD_SECS: f32 = 1.1;

// ── Style helpers ───────────────────────────────────────────────────

// Mute / Solo / LED / Analysis are all the same state-button mechanic — filled
// with their identity colour when on, a tonal recess of the header colour when
// off — so they delegate to the local `state_btn` and differ only in the
// carve-out hue. One mechanic, four hues.

fn mute_style(muted: bool, layer_color: Color32) -> UIStyle {
    state_btn(color::MUTED_COLOR, muted, layer_color)
}

fn analysis_style(analysis: bool, layer_color: Color32) -> UIStyle {
    state_btn(color::ANALYSIS_COLOR, analysis, layer_color)
}

fn solo_style(solo: bool, layer_color: Color32) -> UIStyle {
    state_btn(color::SOLO_COLOR, solo, layer_color)
}

fn led_style(led: bool, layer_color: Color32) -> UIStyle {
    state_btn(color::LED_COLOR, led, layer_color)
}

// The layer-header chip look now lives in the shared component kit as the `Tonal`
// surface of the canonical chip grammar (`chrome::components::ChipSurface` +
// `chip_state_style` / `chip_style` / `dropdown_chip_style`). These thin wrappers
// pin the surface to the layer's identity colour and the layer-header font /
// radius / pill metrics; the kit owns the mechanic so a header chip and a neutral
// inspector dropdown are the same control on two surfaces. Call sites unchanged —
// the rendered chips are byte-identical to the pre-kit local helpers.

/// The layer-card flavour of the state-button mechanic: M/S/L/A on an
/// identity-coloured header. Off = a tonal chip (the header colour darkened); on =
/// filled with the caller's M/S/L/A hue. Pilled (§K) — rounder than the dropdown
/// chips so M/S/L/A read as buttons, not more value pickers.
fn state_btn(active_color: Color32, active: bool, layer_color: Color32) -> UIStyle {
    components::chip_state_style(
        components::ChipSurface::Tonal(layer_color),
        active_color,
        active,
        BTN_FONT,
        MSL_PILL_RADIUS,
    )
}

/// A header chip for the non-toggle header controls (blend, MIDI mode): the same
/// tonal surface as `state_btn`'s off state, so every control on the coloured
/// header shares one recessed surface (§C / §K9). The `CHIP_TEXT_INSET_X` keeps
/// left-aligned value/prefix text off the chip edge (mockup `.sel`/`.blend` 7px).
fn chip_button_style(layer_color: Color32) -> UIStyle {
    components::chip_style(
        components::ChipSurface::Tonal(layer_color),
        SMALL_FONT,
        TextAlign::Center,
        LH_BTN_RADIUS,
        color::CHIP_TEXT_INSET_X,
    )
}

/// A routing *value* chip (Folder path, MIDI note, Channel, Device): the tonal
/// header chip, left-aligned, with the renderer-painted dropdown caret pinned to
/// the right edge so values read as "opens a list" — the mockup's `.sel` dropdown
/// (§K13 / §M).
fn value_chip_style(layer_color: Color32) -> UIStyle {
    components::dropdown_chip_style(
        components::ChipSurface::Tonal(layer_color),
        SMALL_FONT,
        LH_BTN_RADIUS,
    )
}

/// The left-edge selection accent bar's fill: the app-wide bright selection
/// colour when selected, transparent otherwise. Toggled in place on selection
/// change, mirroring the clip-selection language.
fn sel_accent_style(selected: bool) -> UIStyle {
    UIStyle {
        bg_color: if selected {
            color::SELECTED_LAYER_RING
        } else {
            Color32::TRANSPARENT
        },
        ..UIStyle::default()
    }
}

/// The recording-controls device label (top chrome, on the dark panel — NOT a
/// header chip). Kept on the neutral row surface so the timeline-chip restyle
/// doesn't bleed into the recording chrome.
fn field_style() -> UIStyle {
    UIStyle {
        bg_color: color::LAYER_ROW_BG,
        hover_bg_color: color::LAYER_ROW_HOVER_BG,
        pressed_bg_color: color::LAYER_ROW_PRESSED_BG,
        text_color: color::TEXT_DIMMED_C32,
        font_size: SMALL_FONT,
        corner_radius: LH_BTN_RADIUS,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

/// A routing *label* (FOLDER / MIDI / CHANNEL / DEVICE): faint, uppercase, in the
/// fixed label column. Faint = the layer's contrast text dropped to ~70% alpha,
/// so it stays legible on any identity hue (a flat white would wash out on a
/// light layer colour) while reading as a secondary label (§K10 / §K12).
fn routing_label_style(text_clr: Color32) -> UIStyle {
    UIStyle {
        // Full contrast — same as the layer name / main control text (Peter,
        // 2026-06-28). The faint 70%-alpha read was too washed against the header.
        text_color: text_clr,
        font_size: SMALL_FONT,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn bg_style(selected: bool, layer_color: Color32) -> UIStyle {
    // Selection = a brighten of the row's OWN identity colour + a left accent bar
    // (the separate `SelectAccent` node), never a border box. The old white focus
    // ring read as a web outline and fought the neon identity; a small lift keeps
    // the hue and the accent bar marks "active" the pro-DAW way (Peter,
    // 2026-06-28).
    let bg = if selected {
        lighten(layer_color, 22)
    } else {
        layer_color
    };
    let hover = lighten(bg, 15);
    let pressed = darken(bg, 10);
    UIStyle {
        bg_color: bg,
        hover_bg_color: hover,
        pressed_bg_color: pressed,
        // Square corners: rows tile edge-to-edge as a full-bleed identity band.
        corner_radius: color::SQUARE_RADIUS,
        ..UIStyle::default()
    }
}

/// The Record button's pulsed red at `t` seconds into recording (§19): a smooth
/// sine breathe between the dim and bright reds, one cycle per
/// `RECORD_PULSE_PERIOD_SECS`. Pure — at `t=0` it sits at the midpoint, peaks
/// bright a quarter-cycle in, troughs dim three-quarters in.
fn record_pulse_color(t: f32) -> Color32 {
    let phase = (t * std::f32::consts::TAU / RECORD_PULSE_PERIOD_SECS).sin() * 0.5 + 0.5;
    color::mix(color::RECORD_PULSE_DIM, color::RECORD_PULSE_BRIGHT, phase)
}

// ── LayerInfo ───────────────────────────────────────────────────────

/// Lightweight snapshot of a layer's state for UI rendering.
/// The app layer fills this from its data model before calling build().
#[derive(Clone)]
pub struct LayerInfo {
    pub name: String,
    pub layer_id: String,
    pub is_collapsed: bool,
    pub is_group: bool,
    pub is_generator: bool,
    /// True for `LayerType::Audio` — drives the audio control set
    /// (Mute / Solo / Gain / Send) instead of video/generator controls.
    pub is_audio: bool,
    pub is_muted: bool,
    pub is_solo: bool,
    /// Audio "analysis-only" output state: silent to master, still feeding its
    /// send. Drives the teal `A` toggle on the audio row. See LAYER_CONTROLS §5.3.
    pub analysis_only: bool,
    pub is_led: bool,
    pub parent_layer_id: Option<String>,
    pub blend_mode: String,
    pub generator_type: Option<String>,
    pub clip_count: usize,
    pub video_folder_path: Option<String>,
    pub source_clip_count: usize,
    pub midi_note: i32,
    pub midi_channel: i32,
    /// None or empty string = any device.
    pub midi_device: Option<String>,
    /// True when layer is in "AllNotes" trigger mode (every NoteOn triggers).
    pub midi_all_notes: bool,
    /// Audio layer gain in dB (audio layers only).
    pub audio_gain_db: f32,
    /// Name of the modulation send this audio layer feeds, if any. `None` shows
    /// "No source".
    pub audio_send_name: Option<String>,
    pub is_selected: bool,
    /// Layer color (auto-assigned or user-set).
    pub color: Color32,
}

// ── LayerControl ────────────────────────────────────────────────────

/// One descriptor per addressable control the layer card can show. The card is
/// laid out, built, and hit-tested by walking these descriptors, so the
/// per-type difference lives in one place (the geometry branches in
/// `compute_layer_row`) instead of being duplicated across layout / build /
/// hit-test.
///
/// Declaration order is the build (z) order: `Background` first (drawn behind
/// everything), then the structural visuals, then the flowing rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
enum LayerControl {
    Background = 0,
    AccentBar,
    Connector,
    BottomBorder,
    /// Left-edge selection highlight bar — drawn over the background + group
    /// accent so a selected layer (incl. a child) shows the bright bar. Decoration
    /// — non-interactive; its colour is toggled per selection.
    SelectAccent,
    Chevron,
    Name,
    DragHandle,
    /// The generator's type name (P0.5, `docs/TIMELINE_LAYOUT_P0_SPEC.md`):
    /// display-only, never a picker. Expanded, it occupies the same row/height
    /// budget `PathLabel`+`Folder` would use for a video layer — one line
    /// under the name, not an extra row. Collapsed, it's a fixed-width dimmed
    /// zone carved out of the Name rect's right edge, ellipsized, never
    /// widening the row. Set only when `LayerInfo.generator_type.is_some()` —
    /// absent for video/audio/group layers.
    GenType,
    Mute,
    Solo,
    Led,
    Blend,
    /// Hairline rule under the mix row separating M/S/L/Blend from the routing
    /// form (§K). Decoration — non-interactive, drawn over the background.
    MixDivider,
    Separator,
    Info,
    Folder,
    PathLabel,
    NewClip,
    MidiLabel,
    MidiInput,
    MidiMode,
    ChLabel,
    ChDropdown,
    DevLabel,
    DevDropdown,
    AddGenClip,
    Gain,
    Send,
    Analysis,
}

const N_CONTROLS: usize = 30;

impl LayerControl {
    /// All controls in declaration (build / z) order.
    const ALL: [LayerControl; N_CONTROLS] = [
        LayerControl::Background,
        LayerControl::AccentBar,
        LayerControl::Connector,
        LayerControl::BottomBorder,
        LayerControl::SelectAccent,
        LayerControl::Chevron,
        LayerControl::Name,
        LayerControl::DragHandle,
        LayerControl::GenType,
        LayerControl::Mute,
        LayerControl::Solo,
        LayerControl::Led,
        LayerControl::Blend,
        LayerControl::MixDivider,
        LayerControl::Separator,
        LayerControl::Info,
        LayerControl::Folder,
        LayerControl::PathLabel,
        LayerControl::NewClip,
        LayerControl::MidiLabel,
        LayerControl::MidiInput,
        LayerControl::MidiMode,
        LayerControl::ChLabel,
        LayerControl::ChDropdown,
        LayerControl::DevLabel,
        LayerControl::DevDropdown,
        LayerControl::AddGenClip,
        LayerControl::Gain,
        LayerControl::Send,
        LayerControl::Analysis,
    ];

    #[inline]
    fn idx(self) -> usize {
        self as usize
    }
}

// ── LayerRowData ────────────────────────────────────────────────────

/// Per-control rects for one layer row, keyed by `LayerControl`. A control is
/// "present" once `set` records a rect for it; absent controls are skipped by
/// build and hit-test.
#[derive(Clone, Copy)]
struct LayerRowData {
    rects: [Rect; N_CONTROLS],
    present: [bool; N_CONTROLS],
}

impl Default for LayerRowData {
    fn default() -> Self {
        Self {
            rects: [Rect::ZERO; N_CONTROLS],
            present: [false; N_CONTROLS],
        }
    }
}

impl LayerRowData {
    #[inline]
    fn set(&mut self, c: LayerControl, r: Rect) {
        self.rects[c.idx()] = r;
        self.present[c.idx()] = true;
    }

    #[inline]
    fn rect(&self, c: LayerControl) -> Rect {
        self.rects[c.idx()]
    }

    #[inline]
    fn has(&self, c: LayerControl) -> bool {
        self.present[c.idx()]
    }
}

/// Compute element rects for one layer row in panel-local coordinates.
///
/// Geometry is the single source of truth for the card layout; build and
/// hit-test consume the resulting descriptor list and never re-derive type.
#[allow(clippy::too_many_arguments)]
fn compute_layer_row(
    y_offset: f32,
    height: f32,
    panel_width: f32,
    is_collapsed: bool,
    is_group: bool,
    is_generator: bool,
    is_audio: bool,
    is_child: bool,
    is_last_child: bool,
    is_group_expanded: bool,
    // P0.5: `LayerInfo.generator_type.is_some()`, decided by the caller (this
    // fn stays content-free — geometry only, per the file's existing
    // discipline of passing structural bools rather than the layer itself).
    has_gen_label: bool,
) -> LayerRowData {
    use LayerControl as C;
    let mut d = LayerRowData::default();
    let w = if panel_width > 0.0 {
        panel_width
    } else {
        color::LAYER_CONTROLS_WIDTH
    };

    let left_indent = if is_child { CHILD_INDENT } else { 0.0 };
    let pad = PAD + left_indent;
    // Right-anchored controls sit against the card's right edge, which stays at `w`
    // regardless of indent (only the card's LEFT edge moves for a child row) — so right
    // margins use plain `PAD`, never the indented `pad`, or a child row's right-anchored
    // controls (drag handle, blend dropdown, routing form) double-pay the indent. BUG-049.
    let right_pad = PAD;
    let mut y = y_offset + PAD;

    // A child layer insets its identity card to the right by `left_indent`,
    // leaving a gutter that reveals the dark panel backdrop and the group accent
    // spine — the mockup's nested-card look. The card (Background) and every
    // per-row visual that rides on it (selection accent, bottom border,
    // separator) start at `card_x`; only the group AccentBar stays pinned to the
    // panel edge as the spine that runs down the children. For a top-level layer
    // `card_x` is 0, so the card is full-bleed exactly as before.
    let card_x = left_indent;
    let card_w = (w - card_x).max(1.0);
    d.set(C::Background, Rect::new(card_x, y_offset, card_w, height));

    // ── Group visuals ──
    if is_child {
        d.set(C::AccentBar, Rect::new(0.0, y_offset, ACCENT_W, height));
    }
    if is_group && is_group_expanded {
        d.set(
            C::Connector,
            Rect::new(0.0, y_offset + height * 0.5, ACCENT_W, height * 0.5),
        );
    }
    if is_child && is_last_child {
        d.set(
            C::BottomBorder,
            Rect::new(card_x, y_offset + height - BORDER_H, card_w, BORDER_H),
        );
    }
    // Selection accent — a thin bar on the card's left edge, always laid out so
    // the node exists for in-place restyle; its colour is transparent until the
    // layer is selected.
    d.set(C::SelectAccent, Rect::new(card_x, y_offset, SEL_ACCENT_W, height));

    // ── Top row: Chevron | Name | DragHandle ──
    let chevron_w = CHEVRON_W;
    d.set(C::Chevron, Rect::new(pad, y, CHEVRON_W, BTN_H));

    // The type badge (* / ▶ glyph) was removed (Peter, 2026-06-28): the name now
    // starts right after the chevron and reclaims the badge's slot.
    let name_left = pad + chevron_w + if chevron_w > 0.0 { TOP_GAP } else { 0.0 };
    let handle_x = w - right_pad - HANDLE_W - 8.0;
    // P0.5 collapsed generator label: carve a fixed-width dimmed zone out of
    // the Name rect's right edge (before DragHandle) instead of adding a row —
    // the row's total width is unchanged, Name just gives up some of its own
    // budget. Expanded rows are unaffected (Name keeps its full width; the
    // generator name gets its own line in the routing-form slot below).
    if is_collapsed && has_gen_label {
        let label_x = handle_x - TOP_GAP - GEN_LABEL_COLLAPSED_W;
        let name_w = (label_x - TOP_GAP - name_left).max(20.0);
        d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
        d.set(
            C::GenType,
            Rect::new(label_x, y, GEN_LABEL_COLLAPSED_W, NAME_H),
        );
    } else {
        let name_w = (handle_x - name_left - TOP_GAP).max(20.0);
        d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
    }
    d.set(C::DragHandle, Rect::new(handle_x, y, HANDLE_W, BTN_H));

    y += ROW_STEP;

    // ── Button row: M | S | [L | BlendMode] ──
    // Audio layers carry only Mute / Solo here, then a Gain row and a Send row;
    // they have no LED output, blend mode, folder, clip, or MIDI controls.
    // §B mix row: M | S | L pills with a 6px gap — a touch more air than the old
    // 5px so the rounded toggles read as a breathing cluster, not jammed.
    const MSL_GAP: f32 = 6.0;
    let mut btn_x = pad;
    d.set(C::Mute, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + MSL_GAP;
    d.set(C::Solo, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + MSL_GAP;

    if is_audio {
        return compute_audio_row(
            d, y_offset, height, w, card_x, pad, right_pad, btn_x, y, is_collapsed,
        );
    }

    d.set(C::Led, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + MSL_GAP;

    let dd_w = (w - btn_x - right_pad - RIGHT_GUTTER).max(20.0);
    d.set(C::Blend, Rect::new(btn_x, y, dd_w, BTN_H));

    y += BTN_H;

    // ── Collapsed non-group: skip detail controls ──
    let sep_h = if is_group {
        color::GROUP_SEPARATOR_HEIGHT
    } else {
        SEP_H
    };
    let has_expanded_controls = !is_collapsed || is_group;
    if !has_expanded_controls {
        d.set(
            C::Separator,
            Rect::new(card_x, y_offset + height - sep_h, card_w, sep_h),
        );
        return d;
    }

    // §D routing form — aligned [label | value] rows (expanded-only). A fixed
    // label column (LBL_W) puts every value at the same x; Channel and Device get
    // their own spelled-out rows instead of sharing one cramped line. Groups have
    // no routing.
    if !is_group {
        let right_edge = w - right_pad - RIGHT_GUTTER;
        // §K divider: a contrast-aware rule separating the M/S/L/Blend mix row from
        // the routing form — the mockup's clean section break. MIX_DIVIDER_PAD of
        // breathing room above and below it (more than the inter-row gap) so it
        // reads as a deliberate break, not another row line.
        let div_y = (y + MIX_DIVIDER_PAD).round();
        d.set(
            C::MixDivider,
            Rect::new(pad, div_y, (right_edge - pad).max(1.0), MIX_DIVIDER_THICK),
        );
        y = div_y + MIX_DIVIDER_THICK + MIX_DIVIDER_PAD;
        let val_x = pad + LBL_W + 6.0;
        let val_w = (right_edge - val_x).max(20.0);
        let mode_x = right_edge - MODE_TOGGLE_W;

        // FOLDER label | folder-path value chip — video layers only (generators
        // have no source folder). §K11: the static label sits in the label column
        // (PathLabel), the interactive picker chip in the value column (Folder).
        if !is_generator {
            d.set(C::PathLabel, Rect::new(pad, y, LBL_W, BTN_H));
            d.set(C::Folder, Rect::new(val_x, y, val_w, BTN_H));
            y += BTN_H + ROUTING_ROW_GAP;
        } else if has_gen_label {
            // P0.5: the generator name occupies the exact row/height budget
            // FOLDER would use for a video layer — one line under the name,
            // not an extra row. Full-width (no separate label column): the
            // brief asks for "the generator name", not a label+value pair.
            d.set(C::GenType, Rect::new(pad, y, (right_edge - pad).max(20.0), BTN_H));
            y += BTN_H + ROUTING_ROW_GAP;
        }
        // MIDI | note input + trigger-mode toggle.
        d.set(C::MidiLabel, Rect::new(pad, y, LBL_W, BTN_H));
        d.set(
            C::MidiInput,
            Rect::new(val_x, y, (mode_x - 4.0 - val_x).max(10.0), BTN_H),
        );
        d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
        y += BTN_H + ROUTING_ROW_GAP;
        // Channel | dropdown.
        d.set(C::ChLabel, Rect::new(pad, y, LBL_W, BTN_H));
        d.set(C::ChDropdown, Rect::new(val_x, y, val_w, BTN_H));
        y += BTN_H + ROUTING_ROW_GAP;
        // Device | dropdown.
        d.set(C::DevLabel, Rect::new(pad, y, LBL_W, BTN_H));
        d.set(C::DevDropdown, Rect::new(val_x, y, val_w, BTN_H));
    }

    let _ = y; // suppress unused
    d.set(
        C::Separator,
        Rect::new(card_x, y_offset + height - sep_h, card_w, sep_h),
    );
    d
}

/// Audio-layer controls: Mute / Solo are already placed by `compute_layer_row`;
/// this places Analysis on the same button row, then branches on collapse
/// state (`docs/TIMELINE_LAYOUT_P0_SPEC.md` D4):
/// - Collapsed: the same collapsed chrome every other layer type gets — name
///   row + M|S|A row + separator, no Gain/Send. Audio's never-collapse
///   exception is gone; the mapper's state-only `TrackHeight::Collapsed` now
///   actually bounds this card's content.
/// - Expanded: three rows total. Gain takes the slot video cards give Blend
///   — it joins the M|S|A button row instead of stacking below it — and Send
///   is the third (and last) row.
fn compute_audio_row(
    mut d: LayerRowData,
    y_offset: f32,
    height: f32,
    w: f32,
    card_x: f32,
    pad: f32,
    right_pad: f32,
    btn_x: f32,
    y_buttons: f32,
    is_collapsed: bool,
) -> LayerRowData {
    use LayerControl as C;
    let card_w = (w - card_x).max(1.0);
    // Analysis-only toggle on the button row after Mute/Solo (M | S | A): silent to
    // master, still feeding the send. See `docs/LAYER_CONTROLS_DESIGN.md` §5.3.
    d.set(C::Analysis, Rect::new(btn_x, y_buttons, MS_BTN_W, BTN_H));

    if is_collapsed {
        d.set(
            C::Separator,
            Rect::new(card_x, y_offset + height - SEP_H, card_w, SEP_H),
        );
        return d;
    }

    // Gain: dB slider filling the rest of the button row (Blend's slot for
    // video), after Mute | Solo | Analysis.
    let right_edge = w - right_pad - RIGHT_GUTTER;
    let gain_x = btn_x + MS_BTN_W + 6.0;
    d.set(
        C::Gain,
        Rect::new(gain_x, y_buttons, (right_edge - gain_x).max(20.0), BTN_H),
    );

    // Send: modulation-send dropdown, the third (and last) row.
    let send_y = y_buttons + BTN_H;
    d.set(
        C::Send,
        Rect::new(pad, send_y, (right_edge - pad).max(20.0), BTN_H),
    );

    d.set(
        C::Separator,
        Rect::new(card_x, y_offset + height - SEP_H, card_w, SEP_H),
    );
    d
}

// ── LayerRowIds ─────────────────────────────────────────────────────

/// Node IDs for one layer row, keyed by `LayerControl`. Absent controls are
/// `None`. Replaces the old per-field struct so adding a control is one enum
/// variant plus one build arm, not a parallel edit across four structures.
#[derive(Clone, Copy)]
struct LayerRowIds {
    id: [Option<NodeId>; N_CONTROLS],
    /// The per-row `ClipRegion` node (subregion-scissor-invariant, D4) that
    /// every control in this row is parented under. Stored so the in-place
    /// vertical-scroll fast-path (`try_update_vertical_scroll`) can shift the
    /// clip rect in lockstep with the controls — without it, the controls
    /// slide out from under a stationary clip and get cut/greyed mid-scroll.
    clip: Option<NodeId>,
}

impl Default for LayerRowIds {
    fn default() -> Self {
        Self {
            id: [None; N_CONTROLS],
            clip: None,
        }
    }
}

impl LayerRowIds {
    #[inline]
    fn id(&self, c: LayerControl) -> Option<NodeId> {
        self.id[c.idx()]
    }

    #[inline]
    fn set(&mut self, c: LayerControl, v: NodeId) {
        self.id[c.idx()] = Some(v);
    }

    /// Iterate all present node IDs in this row.
    fn for_each_id(&self, mut f: impl FnMut(NodeId)) {
        for id in self.id.iter().flatten() {
            f(*id);
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn folder_path_text(path: &Option<String>, source_count: usize) -> String {
    match path {
        Some(p) if !p.is_empty() => {
            let folder = p
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("");
            format!("{}/ ({})", folder, source_count)
        }
        _ => "None".into(),
    }
}

fn info_text(layer: &LayerInfo, all_layers: &[LayerInfo]) -> String {
    if layer.is_group {
        let child_count = all_layers
            .iter()
            .filter(|l| l.parent_layer_id.as_deref() == Some(&layer.layer_id))
            .count();
        format!("{} children", child_count)
    } else {
        format!("{} clips", layer.clip_count)
    }
}

/// Offset a panel-local rect to screen space.
fn screen(r: Rect, origin: Vec2) -> Rect {
    Rect::new(r.x + origin.x, r.y + origin.y, r.width, r.height)
}

// ── Gain (dB) mapping ───────────────────────────────────────────────
// Shared by the audio-layer Gain slider; the same range will back master and
// clip gain once those reuse the widget.

const GAIN_DB_MIN: f32 = -60.0;
const GAIN_DB_MAX: f32 = 12.0;

/// Map a dB value to the slider's 0..1 normalized position. (The reverse,
/// norm→dB, is handled inside `SliderDragState`, which is configured with the
/// dB range directly.)
fn gain_db_to_norm(db: f32) -> f32 {
    ((db - GAIN_DB_MIN) / (GAIN_DB_MAX - GAIN_DB_MIN)).clamp(0.0, 1.0)
}

/// Human-readable value text for the gain slider (mixer style: signed dB, or
/// "-inf" at the floor).
fn gain_db_text(db: f32) -> String {
    if db <= GAIN_DB_MIN {
        "-inf".to_string()
    } else {
        format!("{:+.1} dB", db)
    }
}

// ── LayerHeaderPanel ────────────────────────────────────────────────

pub struct LayerHeaderPanel {
    layers: Vec<LayerInfo>,
    rows: Vec<LayerRowIds>,

    // Drag-reorder state. drag_source/drag_target/pending_drag_layer are
    // layer-row indices (into self.rows / self.layers), not tree node ids;
    // they keep the -1 "none" sentinel.
    drag_source: i32,
    drag_target: i32,
    insert_indicator_id: Option<NodeId>,
    add_layer_btn: Option<NodeId>,
    // Saved during PointerDown on drag handle so DragBegin can find the
    // correct layer even after a tree rebuild has invalidated node IDs.
    pending_drag_layer: i32,

    // Cached state for dirty-checking
    cached_mute: Vec<bool>,
    cached_solo: Vec<bool>,
    cached_led: Vec<bool>,
    cached_selected: Vec<bool>,
    cached_colors: Vec<Color32>,

    // ── Mute chip motion (UI_CRAFT_AND_MOTION_PLAN.md P1 demonstration) ──
    // Background hover/press colour tween for the Mute chip, one per row.
    // Colour-only per Peter's rule (2026-07-14, BUG-150): animations never
    // move hit geometry, only how a node looks.
    mute_motion: Vec<components::ChipMotion>,
    /// Wall-clock anchor for this frame's `dt_ms` — the same self-timed
    /// pattern `tick_record_pulse` uses (`recording_since.elapsed()`), just
    /// measuring a delta instead of a since-start duration.
    motion_last_tick: Instant,

    // Active layer (pushed from app layer each frame)
    active_layer: Option<LayerId>,
    cached_active_layer: Option<LayerId>,
    // Pending multi-select active flags (applied in update())
    pending_active_layers: Option<Vec<bool>>,

    // ── Live recording controls (in spacer area above layers) ──
    record_btn_id: Option<NodeId>,
    /// When recording started — drives the §19 record-button breathe. `None`
    /// when not recording, so the pulse costs nothing while stopped.
    recording_since: Option<Instant>,
    audio_device_label_id: Option<NodeId>,
    recording_active: bool,
    audio_device_name: String,
    /// BUG-084/BUG-086 instrument: video (pool-exhaustion) + audio
    /// (native-encoder-backpressure) frames dropped so far this recording,
    /// summed. 0 whenever not recording (the content thread reports 0 once
    /// the session is gone). Folded into the Record button's label so a
    /// dropping recording is visible without a separate widget.
    recording_dropped_total: u32,

    /// Host for the declarative top chrome (background + recording controls).
    /// The per-layer scroll rows are still built imperatively below it.
    host: ChromeHost,

    // Screen-space origin of the layer controls panel
    panel_origin: Vec2,
    panel_width: f32,

    // Scroll container used ONLY for the clip-region node (`begin()` /
    // `clip_node_id()`). It does NOT own the scroll offset — the viewport
    // does (D2, `docs/TIMELINE_LAYOUT_P0_SPEC.md`); `build()`/
    // `try_update_vertical_scroll()` take the viewport's `scroll_y_px` as a
    // parameter instead of storing a second copy here.
    scroll: ScrollContainer,

    // Per-row gain slider drag state (audio layers). Reuses the shared
    // SliderDragState machine — same as the inspector/master sliders.
    gain_sliders: Vec<crate::slider::SliderDragState>,
    /// Per-row gain slider right-click reset, built alongside `gain_sliders`
    /// (BUG-061 follow-through) and replayed in `register_intents`.
    gain_slider_resets: Vec<Option<PanelAction>>,
    // Index of the layer whose gain slider is mid-drag, or -1.
    active_gain_drag: i32,

    // Cache tracking
    cache_first_node: usize,
    cache_node_count: usize,
}

impl LayerHeaderPanel {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            rows: Vec::new(),
            drag_source: -1,
            drag_target: -1,
            insert_indicator_id: None,
            add_layer_btn: None,
            pending_drag_layer: -1,
            cached_mute: Vec::new(),
            cached_solo: Vec::new(),
            cached_led: Vec::new(),
            cached_selected: Vec::new(),
            cached_colors: Vec::new(),
            mute_motion: Vec::new(),
            motion_last_tick: Instant::now(),
            active_layer: None,
            cached_active_layer: None,
            pending_active_layers: None,
            record_btn_id: None,
            recording_since: None,
            audio_device_label_id: None,
            recording_active: false,
            audio_device_name: "No audio input".into(),
            recording_dropped_total: 0,
            host: ChromeHost::new(),
            panel_origin: Vec2::ZERO,
            panel_width: 0.0,
            scroll: ScrollContainer::new(),
            gain_sliders: Vec::new(),
            gain_slider_resets: Vec::new(),
            active_gain_drag: -1,
            cache_first_node: usize::MAX,
            cache_node_count: 0,
        }
    }

    /// Set the layer data snapshot. Must be called before build().
    pub fn set_layers(&mut self, layers: Vec<LayerInfo>) {
        self.layers = layers;
    }

    /// Number of layers in the current build.
    pub fn layer_count(&self) -> usize {
        self.rows.len()
    }

    /// Get layer info by index (for context menu filtering).
    pub fn layer_info(&self, index: usize) -> Option<&LayerInfo> {
        self.layers.get(index)
    }

    /// Current row index of the layer with this id, in this panel's snapshot.
    /// The right-click handler resolves a stable `LayerId` back to an index for
    /// the still-positional context-menu actions it opens synchronously.
    pub fn index_of_layer(&self, id: &LayerId) -> Option<usize> {
        self.layers
            .iter()
            .position(|l| LayerId::new(&l.layer_id) == *id)
    }

    /// Set the active (focused) layer by LayerId. Applied in update() via dirty-check.
    pub fn set_active_layer(&mut self, layer_id: Option<LayerId>) {
        self.active_layer = layer_id;
    }

    /// Set per-layer active state from UIState.is_layer_active().
    /// Multiple layers can be active simultaneously (region, multi-select).
    /// Falls back to single active_layer if active_layers is empty.
    pub fn set_active_layers(&mut self, active_layers: &[bool]) {
        // Find the first active layer as the primary — resolve index to LayerId
        let first_active_idx = active_layers.iter().position(|&a| a);
        self.active_layer = first_active_idx
            .and_then(|i| self.layers.get(i))
            .map(|l| LayerId::new(l.layer_id.clone()));
        // Store full multi-select state for visual update in update()
        self.pending_active_layers = Some(active_layers.to_vec());
    }

    // ── Live recording ───────────────────────────────────────────

    pub fn set_recording_active(&mut self, tree: &mut UITree, active: bool) {
        if self.recording_active == active {
            return;
        }
        self.recording_active = active;
        // Start/stop the §19 breathe clock. `tick_record_pulse` (per-frame) takes
        // over the button bg while recording; clearing it here lets the static
        // active/inactive style below stand once stopped.
        self.recording_since = active.then(Instant::now);
        if !active {
            // A stopped recording's drop count is stale the instant the
            // session ends — the next recording starts clean.
            self.recording_dropped_total = 0;
        }
        if let Some(id) = self.record_btn_id {
            tree.set_style(id, self.record_btn_style());
            tree.set_text(id, &self.record_btn_label());
        }
    }

    /// BUG-084/BUG-086 — surface the drop counters (video pool exhaustion +
    /// audio encoder backpressure) on the Record button's label while
    /// recording. Dirty-checked so a steady-state recording (the common
    /// case: 0 dropped) costs nothing beyond the initial call.
    pub fn set_recording_drops(&mut self, tree: &mut UITree, video_dropped: u32, audio_dropped: u32) {
        let total = video_dropped + audio_dropped;
        if self.recording_dropped_total == total {
            return;
        }
        self.recording_dropped_total = total;
        if let Some(id) = self.record_btn_id {
            tree.set_text(id, &self.record_btn_label());
        }
    }

    /// The Record button's current label: base verb plus a drop-count
    /// warning suffix once anything has dropped this recording.
    fn record_btn_label(&self) -> String {
        if !self.recording_active {
            return "Record Live".to_string();
        }
        if self.recording_dropped_total > 0 {
            format!("Stop Recording \u{26A0} {} dropped", self.recording_dropped_total)
        } else {
            "Stop Recording".to_string()
        }
    }

    /// Breathe the Record button red while recording (§19 — the one functional
    /// motion). Driven by the per-frame `update()` tick + elapsed time, so it
    /// needs no animation subsystem; recording is never the idle state, so the
    /// app is already redrawing and a stopped recorder costs nothing here.
    fn tick_record_pulse(&self, tree: &mut UITree) {
        if !self.recording_active {
            return;
        }
        let (Some(since), Some(id)) = (self.recording_since, self.record_btn_id) else {
            return;
        };
        let bg = record_pulse_color(since.elapsed().as_secs_f32());
        tree.set_style(
            id,
            UIStyle {
                bg_color: bg,
                ..self.record_btn_style()
            },
        );
    }

    /// Kit-chip hover/press tween for the Mute chip
    /// (`UI_CRAFT_AND_MOTION_PLAN.md` P1's "kit chip hover/press" demonstration
    /// surface): background blends smoothly toward hover/press instead of the
    /// renderer's instant flag-driven jump, and the chip drops 1px while held.
    /// Rides the same per-frame `update()` tick as `tick_record_pulse` above —
    /// no new redraw-scheduling mechanism. Reads each row's live `UIFlags`
    /// (set by `input.rs` on pointer move/down) to know which way to tween;
    /// settled + idle rows are skipped entirely (zero per-frame cost once the
    /// pointer has moved on).
    fn tick_mute_motion(&mut self, tree: &mut UITree, dt_ms: f32) {
        for i in 0..self.rows.len() {
            let Some(mute_id) = self.rows[i].id(LayerControl::Mute) else {
                continue;
            };
            let Some(flags) = tree.get_node(mute_id).map(|n| n.flags) else {
                continue;
            };
            let hovered = flags.contains(UIFlags::HOVERED);
            let pressed = flags.contains(UIFlags::PRESSED);
            let motion = &mut self.mute_motion[i];

            // Nothing to do: idle, unhovered/unpressed, AND already at the
            // neutral baseline — ticking would be a pure no-op. Checked
            // BEFORE ticking (not after), using `is_at_rest` (not
            // `is_animating`): a control that just released from a full
            // press is "settled" (not mid-flight) but still displaced at
            // press=1.0, and must keep getting ticked back down to 0 before
            // this can skip. Getting this ordering/predicate wrong leaves
            // the last mid-animation value painted forever once flags clear.
            if !hovered && !pressed && motion.is_at_rest() {
                continue;
            }
            motion.tick(dt_ms, hovered, pressed);

            let muted = self.cached_mute.get(i).copied().unwrap_or(false);
            let layer_color = self.cached_colors.get(i).copied().unwrap_or(Color32::TRANSPARENT);
            let target = mute_style(muted, layer_color);
            let blended =
                motion.blend(target.bg_color, target.hover_bg_color, target.pressed_bg_color);
            // Point every colour field the renderer might read at the SAME
            // blended value, so its own instant HOVERED/PRESSED flag branch
            // (`ui_renderer.rs`) paints exactly what we just computed
            // regardless of which branch fires this frame.
            tree.set_style(
                mute_id,
                UIStyle {
                    bg_color: blended,
                    hover_bg_color: blended,
                    pressed_bg_color: blended,
                    ..target
                },
            );

        }
    }

    pub fn set_audio_device_name(&mut self, tree: &mut UITree, name: &str) {
        self.audio_device_name = name.into();
        if let Some(id) = self.audio_device_label_id {
            tree.set_text(id, name);
        }
    }

    /// The host-owned top chrome: the full-area background plus the two stacked
    /// recording-control buttons in the spacer above the layer rows. The rows
    /// themselves stay imperative (a scroll body of dragged per-layer widgets).
    fn top_chrome_view(&self) -> View {
        const REC_PAD: f32 = color::SPACE_S; // §14.4: 6 → 4
        const REC_BTN_H: f32 = 22.0;
        const REC_LABEL_H: f32 = 16.0;
        View::panel()
            .fill()
            .bg(color::CONTROL_BG)
            .key(KEY_BG)
            .pad(Pad::all(REC_PAD))
            .child(
                View::column(4.0)
                    .fill_w()
                    .child(
                        View::button(self.record_btn_label())
                            .fill_w()
                            .h(Sizing::Fixed(REC_BTN_H))
                            .style(self.record_btn_style())
                            .inert()
                            .key(KEY_RECORD),
                    )
                    .child(
                        View::button(self.audio_device_name.as_str())
                            .fill_w()
                            .h(Sizing::Fixed(REC_LABEL_H))
                            .style(field_style())
                            .inert()
                            .key(KEY_DEVICE),
                    ),
            )
    }

    fn record_btn_style(&self) -> UIStyle {
        if self.recording_active {
            UIStyle {
                bg_color: Color32::new(180, 40, 40, 255),
                hover_bg_color: Color32::new(200, 50, 50, 255),
                pressed_bg_color: Color32::new(160, 30, 30, 255),
                text_color: color::TEXT_WHITE_C32,
                font_size: BTN_FONT,
                corner_radius: LH_BTN_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        } else {
            UIStyle {
                bg_color: color::BUTTON_INACTIVE_C32,
                hover_bg_color: color::HEADER_BUTTON_HOVER,
                pressed_bg_color: color::HEADER_BUTTON_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: BTN_FONT,
                corner_radius: LH_BTN_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        }
    }

    // ── Accessors ───────────────────────────────────────────────────

    pub fn blend_mode_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows
            .get(index)
            .and_then(|r| r.id(LayerControl::Blend))
    }

    pub fn midi_channel_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows
            .get(index)
            .and_then(|r| r.id(LayerControl::ChDropdown))
    }

    pub fn midi_input_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows
            .get(index)
            .and_then(|r| r.id(LayerControl::MidiInput))
    }

    pub fn midi_device_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows
            .get(index)
            .and_then(|r| r.id(LayerControl::DevDropdown))
    }

    pub fn midi_mode_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows
            .get(index)
            .and_then(|r| r.id(LayerControl::MidiMode))
    }

    pub fn name_node_id(&self, index: usize) -> Option<NodeId> {
        self.rows.get(index).and_then(|r| r.id(LayerControl::Name))
    }

    pub fn get_node_bounds(&self, tree: &UITree, node_id: Option<NodeId>) -> Rect {
        match node_id {
            Some(id) => tree.get_bounds(id),
            None => Rect::ZERO,
        }
    }

    // ── Push-based setters ──────────────────────────────────────────

    pub fn set_mute_state(&mut self, tree: &mut UITree, index: usize, muted: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_mute.get_mut(index) {
                if *cached == muted {
                    return;
                }
                *cached = muted;
            }
            if let Some(mute_id) = row.id(LayerControl::Mute) {
                let lc = self.cached_colors.get(index).copied().unwrap_or(Color32::TRANSPARENT);
                tree.set_style(mute_id, mute_style(muted, lc));
            }
        }
    }

    pub fn set_solo_state(&mut self, tree: &mut UITree, index: usize, solo: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_solo.get_mut(index) {
                if *cached == solo {
                    return;
                }
                *cached = solo;
            }
            if let Some(solo_id) = row.id(LayerControl::Solo) {
                let lc = self.cached_colors.get(index).copied().unwrap_or(Color32::TRANSPARENT);
                tree.set_style(solo_id, solo_style(solo, lc));
            }
        }
    }

    pub fn set_led_state(&mut self, tree: &mut UITree, index: usize, led: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_led.get_mut(index) {
                if *cached == led {
                    return;
                }
                *cached = led;
            }
            if let Some(led_id) = row.id(LayerControl::Led) {
                let lc = self.cached_colors.get(index).copied().unwrap_or(Color32::TRANSPARENT);
                tree.set_style(led_id, led_style(led, lc));
            }
        }
    }

    pub fn set_selection(&mut self, tree: &mut UITree, index: usize, selected: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_selected.get_mut(index) {
                if *cached == selected {
                    return;
                }
                *cached = selected;
            }
            if let Some(bg_id) = row.id(LayerControl::Background) {
                let layer_color = self
                    .cached_colors
                    .get(index)
                    .copied()
                    .unwrap_or(Color32::TRANSPARENT);
                tree.set_style(bg_id, bg_style(selected, layer_color));
            }
            // Toggle the left-edge selection accent bar in place.
            if let Some(accent_id) = row.id(LayerControl::SelectAccent) {
                tree.set_style(accent_id, sel_accent_style(selected));
            }
        }
    }

    pub fn set_layer_name(&mut self, tree: &mut UITree, index: usize, name: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::Name) {
                tree.set_text(id, name);
            }
    }

    pub fn set_blend_mode_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        // §M: the "BLEND" micro-label is a style prefix (`prefix_label`) painted by
        // the renderer — the live-refresh text is just the bare mode value.
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::Blend) {
                tree.set_text(id, text);
            }
    }

    pub fn set_midi_note_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        // §M: the caret is a style flag (`dropdown_caret`), painted by the
        // renderer — the live-refresh text is just the value, no glyph to re-append.
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::MidiInput) {
                tree.set_text(id, text);
            }
    }

    pub fn set_midi_channel_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::ChDropdown) {
                tree.set_text(id, text);
            }
    }

    pub fn set_midi_device_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::DevDropdown) {
                tree.set_text(id, text);
            }
    }

    pub fn set_midi_mode_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        // MidiMode is a toggle, not a dropdown — no caret (see the build arm).
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::MidiMode) {
                tree.set_text(id, text);
            }
    }

    pub fn set_info_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::Info) {
                tree.set_text(id, text);
            }
    }

    // ── Drag-reorder (separate from Panel trait — needs &mut UITree) ──

    /// Returns true if a layer drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag_source >= 0
    }

    // ── Gain slider drag (audio layers) ───────────────────────────────

    /// True while an audio-layer gain slider is mid-drag.
    pub fn is_gain_dragging(&self) -> bool {
        self.active_gain_drag >= 0
    }

    /// Try to begin a gain-slider drag on `node_id` at screen x `pos_x`.
    /// Returns Snapshot + Changed actions if it hit an audio layer's gain track.
    pub fn try_begin_gain_drag(&mut self, node_id: NodeId, pos_x: f32) -> Vec<PanelAction> {
        for i in 0..self.gain_sliders.len() {
            if let Some(val) = self.gain_sliders[i].try_start_drag(node_id, pos_x) {
                self.active_gain_drag = i as i32;
                let Some(lid) = self.layer_id_at(i) else {
                    return Vec::new();
                };
                return vec![
                    PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Begin),
                    PanelAction::Scrub(ValueRef::LayerAudioGain(lid), ScrubPhase::Move(ScrubValue::Scalar(val))),
                ];
            }
        }
        Vec::new()
    }

    /// Continue the active gain drag; updates the slider visual and returns the
    /// new dB value as a Changed action.
    pub fn handle_gain_drag(&mut self, tree: &mut UITree, pos_x: f32) -> Vec<PanelAction> {
        if self.active_gain_drag < 0 {
            return Vec::new();
        }
        let i = self.active_gain_drag as usize;
        if let Some(val) = self.gain_sliders[i].apply_drag(pos_x, tree, &gain_db_text)
            && let Some(lid) = self.layer_id_at(i)
        {
            return vec![PanelAction::Scrub(ValueRef::LayerAudioGain(lid), ScrubPhase::Move(ScrubValue::Scalar(val)))];
        }
        Vec::new()
    }

    /// End the active gain drag; returns a Commit action for one undo step.
    pub fn handle_gain_drag_end(&mut self) -> Vec<PanelAction> {
        if self.active_gain_drag < 0 {
            return Vec::new();
        }
        let i = self.active_gain_drag as usize;
        self.active_gain_drag = -1;
        if self.gain_sliders[i].end_drag()
            && let Some(lid) = self.layer_id_at(i)
        {
            return vec![PanelAction::Scrub(ValueRef::LayerAudioGain(lid), ScrubPhase::Commit)];
        }
        Vec::new()
    }

    /// Call when a drag begins on a layer header node.
    /// Returns PanelAction if the drag starts on a drag handle. `node_id` is
    /// `Option` (D9, `docs/DRAG_CAPTURE_DESIGN.md`): a `None` means the
    /// pressed node died before the drag threshold crossed — the existing
    /// `pending_drag_layer` fallback below already covers exactly this case
    /// (it was built for a rebuild invalidating the id), so `None` just skips
    /// straight to it instead of attempting an exact match.
    pub fn handle_drag_begin(
        &mut self,
        tree: &mut UITree,
        node_id: Option<NodeId>,
    ) -> Vec<PanelAction> {
        // Try exact node_id match first (works when no rebuild happened since PointerDown).
        let mut matched_index: Option<usize> = None;
        if let Some(node_id) = node_id {
            for (i, row) in self.rows.iter().enumerate() {
                if row.id(LayerControl::DragHandle) == Some(node_id) {
                    matched_index = Some(i);
                    break;
                }
            }
        }
        // Fallback: if a tree rebuild invalidated node IDs between PointerDown and
        // DragBegin, use the layer index saved during PointerDown.
        if matched_index.is_none() && self.pending_drag_layer >= 0 {
            let idx = self.pending_drag_layer as usize;
            if idx < self.rows.len() {
                matched_index = Some(idx);
            }
        }
        self.pending_drag_layer = -1;

        if let Some(i) = matched_index {
            self.drag_source = i as i32;
            self.drag_target = i as i32;
            if let Some(row) = self.rows.get(i)
                && let Some(bg_id) = row.id(LayerControl::Background) {
                    tree.set_style(
                        bg_id,
                        UIStyle {
                            bg_color: DRAG_SOURCE_DIM,
                            ..UIStyle::default()
                        },
                    );
                }
            return vec![PanelAction::Layer(LayerAction::LayerDragStarted(i))];
        }
        self.drag_source = -1;
        Vec::new()
    }

    /// Call during an active drag with the current pointer position (screen space).
    /// `mapper` is the same `CoordinateMapper` the viewport draws lanes from — the
    /// drag target must land on the same row the lanes show, so it's queried live
    /// here rather than read from a copy (see `docs/TIMELINE_LAYOUT_P0_SPEC.md` D1).
    pub fn handle_drag(
        &mut self,
        tree: &mut UITree,
        screen_pos: Vec2,
        mapper: &CoordinateMapper,
    ) -> Vec<PanelAction> {
        if self.drag_source < 0 {
            return Vec::new();
        }

        // Convert screen pos to panel-local Y
        let local_y = screen_pos.y - self.panel_origin.y;

        // Find target layer based on Y position
        let mut target = -1i32;
        for i in 0..self.layers.len() {
            let height = mapper.get_layer_height(i);
            if height <= 0.0 {
                continue;
            }
            let y_offset = mapper.get_layer_y_offset(i);
            if local_y >= y_offset && local_y < y_offset + height {
                target = i as i32;
                break;
            }
        }

        if target < 0 {
            target = if local_y < 0.0 {
                0
            } else {
                (self.layers.len() as i32 - 1).max(0)
            };
        }

        if target != self.drag_target {
            self.drag_target = target;
            self.update_insert_indicator(tree, mapper);
            return vec![PanelAction::Layer(LayerAction::LayerDragMoved(
                self.drag_source as usize,
                target as usize,
            ))];
        }
        Vec::new()
    }

    /// Call when a drag ends.
    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.drag_source < 0 {
            return Vec::new();
        }

        let source = self.drag_source as usize;
        let target = self.drag_target as usize;
        self.drag_source = -1;
        self.drag_target = -1;

        self.hide_insert_indicator(tree);

        // Restore source layer appearance
        if let Some(row) = self.rows.get(source)
            && let Some(bg_id) = row.id(LayerControl::Background) {
                let selected = self.cached_selected.get(source).copied().unwrap_or(false);
                let layer_color = self
                    .cached_colors
                    .get(source)
                    .copied()
                    .unwrap_or(Color32::TRANSPARENT);
                tree.set_style(bg_id, bg_style(selected, layer_color));
            }

        if source != target {
            vec![PanelAction::Layer(LayerAction::LayerDragEnded(source, target))]
        } else {
            Vec::new()
        }
    }

    // ── Drag visual helpers ─────────────────────────────────────────

    fn update_insert_indicator(&self, tree: &mut UITree, mapper: &CoordinateMapper) {
        let Some(indicator_id) = self.insert_indicator_id else {
            return;
        };

        let idx = self.drag_target as usize;
        let y = if self.drag_target <= self.drag_source {
            mapper.get_layer_y_offset(idx)
        } else {
            mapper.get_layer_y_offset(idx) + mapper.get_layer_height(idx)
        };

        let screen_y = self.panel_origin.y + y - INSERT_LINE_H * 0.5;
        tree.set_bounds(
            indicator_id,
            Rect::new(
                self.panel_origin.x,
                screen_y,
                self.panel_width,
                INSERT_LINE_H,
            ),
        );
        tree.set_style(
            indicator_id,
            UIStyle {
                bg_color: INSERT_LINE_CLR,
                ..UIStyle::default()
            },
        );
    }

    fn hide_insert_indicator(&self, tree: &mut UITree) {
        let Some(indicator_id) = self.insert_indicator_id else {
            return;
        };
        tree.set_bounds(indicator_id, Rect::new(0.0, -10.0, 0.0, 0.0));
        tree.set_style(
            indicator_id,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                ..UIStyle::default()
            },
        );
    }

    // ── Build helpers ───────────────────────────────────────────────

    fn build_layer_row(
        &mut self,
        tree: &mut UITree,
        index: usize,
        layer: &LayerInfo,
        row: LayerRowData,
        origin: Vec2,
        clip_parent: Option<NodeId>,
    ) {
        use LayerControl as C;
        let s = |r: Rect| screen(r, origin);
        // Contrast text color for readability on the layer's background.
        let text_clr = color::contrast_text_color(layer.color);
        let mut ids = LayerRowIds::default();

        // Per-row content clip (subregion-scissor-invariant, D4): bounds every
        // control in this row to the row's own vertical extent, full panel
        // width — not just the card rect — so group gutter visuals
        // (AccentBar/Connector at x=0, outside `card_x` for a child layer)
        // stay visible while a future content overflow is truncated inside
        // its own row instead of bleeding into the neighbour above/below.
        // Nested clip regions intersect (verified: `ui_renderer.rs` push/pop
        // clip, `tree.rs` hit-test clip-ancestor walk), so this composes
        // safely with the panel-wide scroll clip already at `clip_parent`.
        let bg_rect = row.rect(C::Background);
        let full_row_width = bg_rect.x + bg_rect.width;
        let row_clip_rect = s(Rect::new(0.0, bg_rect.y, full_row_width, bg_rect.height));
        let row_clip = tree.add_node(
            clip_parent,
            row_clip_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        );
        ids.clip = Some(row_clip);
        let clip_parent = Some(row_clip);

        // One build arm per control kind, shared by every layer type. Walk the
        // descriptors in declaration (z) order; absent controls are skipped.
        for &c in &C::ALL {
            if !row.has(c) {
                continue;
            }
            let r = s(row.rect(c));
            let node: NodeId = match c {
                C::Background => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    bg_style(layer.is_selected, layer.color),
                    "",
                ),
                C::AccentBar | C::Connector => tree.add_panel(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    UIStyle {
                        bg_color: ACCENT_COLOR,
                        ..UIStyle::default()
                    },
                ),
                C::BottomBorder => tree.add_panel(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    UIStyle {
                        bg_color: BORDER_CLR,
                        ..UIStyle::default()
                    },
                ),
                C::Chevron => {
                    let chev = if layer.is_collapsed {
                        "\u{25B6}"
                    } else {
                        "\u{25BC}"
                    };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            bg_color: Color32::TRANSPARENT,
                            hover_bg_color: color::BUTTON_HIGHLIGHTED,
                            pressed_bg_color: color::BUTTON_PRESSED,
                            text_color: text_clr,
                            font_size: SMALL_FONT,
                            corner_radius: color::SMALL_RADIUS,
                            text_align: TextAlign::Center,
                            ..UIStyle::default()
                        },
                        chev,
                    )
                }
                C::SelectAccent => tree.add_panel(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    sel_accent_style(layer.is_selected),
                ),
                C::Name => {
                    // `add_button` never clips/ellipsizes its text to the node's
                    // own width — it draws the full string and only the row-wide
                    // ClipRegion cuts it off, at the row's right edge, not at
                    // this control's own rect. On a collapsed child row (indent
                    // narrows the card, and `compute_layer_row` further carves
                    // GEN_LABEL_COLLAPSED_W off the Name rect's right edge for
                    // GenType) a long name's unclipped text bled straight into
                    // the GenType label drawn after it. Truncate against the
                    // tree's measurer (glyph-accurate in the app, per
                    // `UITree::set_text_measure`) so a name that genuinely fits
                    // passes through byte-identical and only a true overflow
                    // ellipsizes — GenType then starts in genuinely empty space
                    // instead of hoping the name happened to be short. The
                    // budget is the distance to the next control's x, not the
                    // bare rect width: `compute_layer_row` ends the Name rect
                    // TOP_GAP before GenType/DragHandle as a breathing gap, and
                    // a name may run into that gap (as short names always have)
                    // — the collision boundary is the neighbour's ink, never
                    // the gap.
                    let name = crate::text::truncate_with_ellipsis(
                        tree.measurer(),
                        &layer.name,
                        NAME_FONT,
                        color::FONT_WEIGHT_DEFAULT,
                        r.width + TOP_GAP,
                    );
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            bg_color: Color32::TRANSPARENT,
                            hover_bg_color: color::LAYER_CHEVRON_HOVER,
                            pressed_bg_color: color::LAYER_CHEVRON_PRESSED,
                            // §K16: a selected layer's name brightens to pure white
                            // (paired with the focus ring), so the selected layer
                            // reads first; otherwise the identity-contrast colour.
                            text_color: if layer.is_selected {
                                color::TEXT_WHITE_C32
                            } else {
                                text_clr
                            },
                            font_size: NAME_FONT,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                        &name,
                    )
                }
                C::DragHandle => {
                    // Hamburger icon drawn as 3 horizontal bars. No pill: the grab
                    // handle isn't a live-show action, so it recedes — a bare glyph
                    // in a muted header tint, with only a faint hover/press overlay
                    // for the affordance (Peter, 2026-06-28).
                    let handle = tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            bg_color: Color32::TRANSPARENT,
                            hover_bg_color: color::HOVER_OVERLAY,
                            pressed_bg_color: color::PRESS_OVERLAY,
                            corner_radius: LH_BTN_RADIUS,
                            ..UIStyle::default()
                        },
                        "",
                    );
                    let bar_w: f32 = 10.0;
                    let bar_h: f32 = 1.5;
                    let bar_x = r.x + (r.width - bar_w) * 0.5;
                    let bar_style = UIStyle {
                        // ~60% of the layer's contrast text colour — visible but
                        // recessive on any identity hue.
                        bg_color: color::with_alpha(text_clr, 150),
                        ..UIStyle::default()
                    };
                    for i in 0..3 {
                        let bar_y = r.y + 4.5 + i as f32 * 4.0;
                        tree.add_panel(Some(handle), bar_x, bar_y, bar_w, bar_h, bar_style);
                    }
                    handle
                }
                // P0.5 (`docs/TIMELINE_LAYOUT_P0_SPEC.md`): display-only generator
                // name, never a picker. Two mutually-exclusive placements share
                // this one control (compute_layer_row never sets both in the
                // same row): collapsed → a dimmed, ellipsized label to the right
                // of Name; expanded → a full-contrast line in the same row/height
                // slot a video layer's FOLDER row would use.
                C::GenType => {
                    let gen_text = layer.generator_type.as_deref().unwrap_or("");
                    if layer.is_collapsed {
                        let truncated =
                            crate::draw::elide_to_width(gen_text, SMALL_FONT as f32, r.width);
                        tree.add_label(
                            clip_parent,
                            r.x,
                            r.y,
                            r.width,
                            r.height,
                            &truncated,
                            UIStyle {
                                // Dimmed — ~55% of the contrast text colour, same
                                // recede-into-the-header treatment as DragHandle's
                                // bars, so it reads as secondary info next to the
                                // bright layer name.
                                text_color: color::with_alpha(text_clr, 140),
                                font_size: SMALL_FONT,
                                text_align: TextAlign::Left,
                                ..UIStyle::default()
                            },
                        )
                    } else {
                        tree.add_label(
                            clip_parent,
                            r.x,
                            r.y,
                            r.width,
                            r.height,
                            gen_text,
                            routing_label_style(text_clr),
                        )
                    }
                }
                C::Mute => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    mute_style(layer.is_muted, layer.color),
                    "M",
                ),
                C::Solo => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    solo_style(layer.is_solo, layer.color),
                    "S",
                ),
                C::Analysis => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    analysis_style(layer.analysis_only, layer.color),
                    "A",
                ),
                C::Led => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    led_style(layer.is_led, layer.color),
                    "L",
                ),
                C::Blend => {
                    // §M: a dim "BLEND" micro-label prefixes the mode (the mockup's
                    // `<b>BLEND</b> Normal`). The label is a renderer-painted prefix
                    // (`prefix_label`/`prefix_color`), not baked into the value
                    // string — so the value reads bright and the label reads dim.
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            // Centre the whole "BLEND <mode>" block in the chip.
                            text_align: TextAlign::Center,
                            prefix_label: Some("BLEND"),
                            prefix_color: color::CHIP_PREFIX,
                            ..chip_button_style(layer.color)
                        },
                        &layer.blend_mode,
                    )
                }
                C::MixDivider => tree.add_panel(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    UIStyle {
                        // Contrast-keyed off the layer's own text colour so the
                        // rule reads on any identity hue. ~50% alpha — clearly
                        // visible as a section break without being a hard line.
                        bg_color: color::with_alpha(text_clr, 128),
                        ..UIStyle::default()
                    },
                ),
                C::Separator => {
                    let sep_color = if layer.is_group {
                        color::GROUP_SEPARATOR_COLOR
                    } else {
                        SEP_COLOR
                    };
                    tree.add_panel(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            bg_color: sep_color,
                            ..UIStyle::default()
                        },
                    )
                }
                C::Info => {
                    let info = info_text(layer, &self.layers);
                    tree.add_label(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        &info,
                        UIStyle {
                            text_color: text_clr,
                            font_size: SMALL_FONT,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    )
                }
                // §K11: the interactive element is the VALUE in the value column
                // (a dropdown chip showing the folder path + caret), the static
                // "FOLDER" label sits in the label column. `FolderClicked` stays
                // wired to `C::Folder`, now correctly the value chip.
                C::Folder => {
                    let path_text =
                        folder_path_text(&layer.video_folder_path, layer.source_clip_count);
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        value_chip_style(layer.color),
                        &path_text,
                    )
                }
                C::PathLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "FOLDER",
                    routing_label_style(text_clr),
                ),
                C::NewClip => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    chip_button_style(layer.color),
                    "+ new clip",
                ),
                C::MidiLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "MIDI",
                    routing_label_style(text_clr),
                ),
                C::MidiInput => {
                    let midi_text = if layer.midi_all_notes {
                        "—".to_string()
                    } else {
                        note_number_to_name(layer.midi_note)
                    };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        value_chip_style(layer.color),
                        &midi_text,
                    )
                }
                C::MidiMode => {
                    // A toggle (Note↔All on click), not a dropdown — so it carries
                    // NO caret (a caret would promise a list) and stays a plain chip.
                    let mode_text = if layer.midi_all_notes { "All" } else { "Note" };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        chip_button_style(layer.color),
                        mode_text,
                    )
                }
                C::ChLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "CHANNEL",
                    routing_label_style(text_clr),
                ),
                C::ChDropdown => {
                    let ch_text = if layer.midi_channel < 0 {
                        "All".to_string()
                    } else {
                        format!("{}", layer.midi_channel + 1)
                    };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        value_chip_style(layer.color),
                        &ch_text,
                    )
                }
                C::DevLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "DEVICE",
                    routing_label_style(text_clr),
                ),
                C::DevDropdown => {
                    let dev_text: &str = match layer.midi_device.as_deref() {
                        None | Some("") => "All",
                        Some(name) => name,
                    };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        value_chip_style(layer.color),
                        dev_text,
                    )
                }
                C::AddGenClip => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    chip_button_style(layer.color),
                    "+ Clip",
                ),
                C::Gain => {
                    // Reusable dB slider; its `track` node is the drag target we
                    // address as the Gain control. The full node set is handed to
                    // a SliderDragState so drag updates reuse the shared machine.
                    let norm = gain_db_to_norm(layer.audio_gain_db);
                    let value_text = gain_db_text(layer.audio_gain_db);
                    let lid = LayerId::new(&layer.layer_id);
                    let reset = PanelAction::slider_reset(
                        PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Begin),
                        PanelAction::Scrub(ValueRef::LayerAudioGain(lid.clone()), ScrubPhase::Move(ScrubValue::Scalar(0.0))),
                        PanelAction::Scrub(ValueRef::LayerAudioGain(lid), ScrubPhase::Commit),
                    );
                    let built = crate::slider::BitmapSlider::build(
                        tree,
                        clip_parent,
                        r,
                        None,
                        norm,
                        &value_text,
                        &crate::slider::SliderColors::default_slider(),
                        SMALL_FONT,
                        0.0,
                        gain_db_to_norm(0.0),
                        reset,
                        None,
                    );
                    let track = built.ids.track;
                    let mut gs = crate::slider::SliderDragState::with_range(
                        GAIN_DB_MIN,
                        GAIN_DB_MAX,
                        false,
                    );
                    gs.set_ids(built.ids);
                    self.gain_sliders[index] = gs;
                    self.gain_slider_resets[index] = Some(built.reset);
                    track
                }
                C::Send => {
                    let send_text = layer.audio_send_name.as_deref().unwrap_or("No source");
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        value_chip_style(layer.color),
                        send_text,
                    )
                }
            };
            // Naming pass (UI_AUTOMATION_DESIGN.md D8/§3, high-value points only):
            // row identity comes from the selector's `under_text` ancestor query,
            // not a per-row name string — every mute/solo chip across every layer
            // shares the same static name.
            if let Some(name) = match c {
                LayerControl::Mute => Some("layer_header.mute"),
                LayerControl::Solo => Some("layer_header.solo"),
                _ => None,
            } {
                tree.set_name(node, name);
            }
            ids.set(c, node);
        }

        self.rows[index] = ids;
    }

    /// Stable `LayerId` for row `i`, resolved against this panel's own layer
    /// snapshot — the exact list every row was built from, so it can't be stale
    /// relative to the row index. Actions carry this id, not `i`, so a bridge
    /// resolving against a newer model snapshot never hits the wrong layer.
    fn layer_id_at(&self, i: usize) -> Option<LayerId> {
        self.layers.get(i).map(|l| LayerId::new(&l.layer_id))
    }

    fn handle_click(&self, node_id: NodeId, modifiers: crate::input::Modifiers) -> Vec<PanelAction> {
        // Record Live button
        if self.record_btn_id == Some(node_id) {
            return vec![PanelAction::Project(ProjectAction::ToggleLiveRecording)];
        }
        // Audio input device dropdown
        if self.audio_device_label_id == Some(node_id) {
            return vec![PanelAction::Project(ProjectAction::SelectAudioInputDevice)];
        }
        // Add Layer button
        if self.add_layer_btn == Some(node_id) {
            return vec![PanelAction::Layer(LayerAction::AddLayerClicked)];
        }
        use LayerControl as C;
        for (i, row) in self.rows.iter().enumerate() {
            for &c in &C::ALL {
                if row.id(c) != Some(node_id) {
                    continue;
                }
                let Some(lid) = self.layer_id_at(i) else {
                    return Vec::new();
                };
                return match c {
                    C::Mute => vec![PanelAction::Layer(LayerAction::ToggleMute(lid))],
                    C::Solo => vec![PanelAction::Layer(LayerAction::ToggleSolo(lid))],
                    C::Analysis => vec![PanelAction::Layer(LayerAction::ToggleAnalysisOnly(lid))],
                    C::Led => vec![PanelAction::Layer(LayerAction::ToggleLed(lid))],
                    C::Chevron => vec![PanelAction::Layer(LayerAction::ChevronClicked(lid))],
                    C::Blend => vec![PanelAction::Layer(LayerAction::BlendModeClicked(lid))],
                    C::Folder => vec![PanelAction::Layer(LayerAction::FolderClicked(lid))],
                    C::NewClip => vec![PanelAction::Layer(LayerAction::NewClipClicked(lid))],
                    C::AddGenClip => vec![PanelAction::Layer(LayerAction::AddGenClipClicked(lid))],
                    C::MidiInput => vec![PanelAction::Layer(LayerAction::MidiInputClicked(lid))],
                    C::MidiMode => vec![PanelAction::Project(ProjectAction::MidiTriggerModeClicked(lid))],
                    C::ChDropdown => vec![PanelAction::Layer(LayerAction::MidiChannelClicked(lid))],
                    C::DevDropdown => vec![PanelAction::Layer(LayerAction::MidiDeviceClicked(lid))],
                    C::Send => vec![PanelAction::Root(RootAction::AudioSendClicked(lid))],
                    C::Name | C::Background | C::DragHandle => {
                        vec![PanelAction::Layer(LayerAction::LayerClicked(lid, modifiers))]
                    }
                    // Labels, separators, accent visuals, and the gain track
                    // (drag, not click) have no click action.
                    _ => Vec::new(),
                };
            }
        }
        Vec::new()
    }

    fn handle_double_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        for (i, row) in self.rows.iter().enumerate() {
            if row.id(LayerControl::Name) == Some(node_id)
                && let Some(lid) = self.layer_id_at(i)
            {
                return vec![PanelAction::Layer(LayerAction::LayerDoubleClicked(lid))];
            }
        }
        Vec::new()
    }

    /// Node-intent dispatch for right-click: a right-click anywhere in layer
    /// row `i` opens that layer's context menu. Every node of the row is
    /// registered (the row's `bg` plus all controls are flat siblings under the
    /// shared scroll-clip node, so there's no single container to fold up to),
    /// which reproduces the old whole-row positional behaviour through the
    /// registry — and the hit test now scopes it to this panel for free, so the
    /// former manual X-bounds guard is gone. See `docs/NODE_INTENT_DISPATCH.md`.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        for (i, row) in self.rows.iter().enumerate() {
            let Some(lid) = self.layer_id_at(i) else {
                continue;
            };
            row.for_each_id(|id| {
                intents.on(
                    id,
                    crate::intent::Gesture::RightClick,
                    PanelAction::Editing(EditingAction::LayerHeaderRightClicked(lid.clone())),
                );
            });
            // Gain slider track: right-click resets to unity (0 dB) instead of
            // opening the row's context menu — registered AFTER the whole-row
            // loop above so this more specific intent wins (BUG-061; the gain
            // slider never had a reset gesture before this). The reset was
            // built alongside the slider (`build_layer_row`'s `C::Gain` arm);
            // replayed here rather than re-derived.
            if let (Some(ids), Some(reset)) =
                (self.gain_sliders[i].ids(), self.gain_slider_resets.get(i).and_then(|r| r.as_ref()))
            {
                crate::slider::BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }
    }

    // ── Update-in-place (vertical scroll) ─────────────────────

    /// Try to update layer row Y positions in-place for vertical scroll.
    /// Computes the Y delta from the new scroll offset and shifts all node
    /// positions. Returns `true` if successful, `false` if full rebuild needed.
    pub fn try_update_vertical_scroll(
        &mut self,
        tree: &mut UITree,
        layout: &ScreenLayout,
        scroll_y_px: f32,
    ) -> bool {
        // Guard: must have been built
        if self.rows.is_empty() || self.rows.len() != self.layers.len() {
            return false;
        }

        let lc = layout.layer_controls();
        let header_spacer = layout.track_header_height();
        let new_origin_y = lc.y + header_spacer - scroll_y_px;
        let delta_y = new_origin_y - self.panel_origin.y;

        // Nothing changed
        if delta_y.abs() < 0.001 {
            return true;
        }

        // Update all node positions (and their children) by delta_y. Each
        // row's clip rect (`row.clip`) moves with its controls — it is a
        // sibling-of-content anchor, not a parent whose offset cascades here,
        // so it needs its own self-only shift (mirrors the insert-indicator
        // shift below). Missing this is what let controls scroll out from
        // under a stationary clip and render greyed/cut (BUG-025).
        for row in &self.rows {
            row.for_each_id(|id| {
                tree.offset_node_and_children(id, delta_y);
            });
            if let Some(clip_id) = row.clip {
                let mut bounds = tree.get_bounds(clip_id);
                bounds.y += delta_y;
                tree.set_bounds(clip_id, bounds);
            }
        }

        // Also shift insert indicator
        if let Some(indicator_id) = self.insert_indicator_id {
            let mut bounds = tree.get_bounds(indicator_id);
            bounds.y += delta_y;
            tree.set_bounds(indicator_id, bounds);
        }

        self.panel_origin.y = new_origin_y;
        true
    }
}

impl Default for LayerHeaderPanel {
    fn default() -> Self {
        Self::new()
    }
}

// LayerHeaderPanel deliberately does NOT implement the shared `Panel` trait
// (`super::Panel`, `panels/mod.rs`): `build()` here needs a `&CoordinateMapper`
// and the shared scroll offset, which `Panel::build`'s fixed
// `(&mut self, tree, layout)` signature can't carry. `Panel` is never used via
// `dyn Panel` or a generic bound anywhere in the codebase (verified
// 2026-07-04), so this type-local divergence has zero effect on any other
// panel or call site. `node_range()` below reproduces the trait's default
// method — the one piece of `Panel` this type's callers still rely on
// (`ui_root.rs`'s panel-cache-info sweep).
impl LayerHeaderPanel {
    /// Build all nodes for this panel into the tree.
    ///
    /// `mapper` is the same `CoordinateMapper` the viewport builds lanes
    /// from — Y offsets and heights are queried from it at draw time instead
    /// of being copied into `LayerInfo`, so headers and lanes cannot disagree
    /// (`docs/TIMELINE_LAYOUT_P0_SPEC.md` D1). `scroll_y_px` is the viewport's
    /// scroll position, the sole owner (D2) — the header no longer keeps its
    /// own copy.
    pub fn build(
        &mut self,
        tree: &mut UITree,
        layout: &ScreenLayout,
        mapper: &CoordinateMapper,
        scroll_y_px: f32,
    ) {
        self.cache_first_node = tree.count();

        let lc = layout.layer_controls();
        // Offset layer rows down by the header stack (overview strip + ruler + waveform lanes)
        // so they align vertically with the track content area. `track_header_height()`
        // is the single source for this offset — the viewport reads the same value, so
        // the two cannot diverge.
        let header_spacer = layout.track_header_height();
        self.panel_origin = Vec2::new(lc.x, lc.y + header_spacer - scroll_y_px);
        self.panel_width = lc.width;

        // ── Top chrome (full-area background + recording controls) on the host.
        // The background prevents compositor blit bleed-through; the rows below
        // are still built imperatively into the scroll region.
        let chrome = self.top_chrome_view();
        self.host.build(tree, &chrome, lc);
        self.record_btn_id = self.host.node_id_for_key(KEY_RECORD);
        self.audio_device_label_id = self.host.node_id_for_key(KEY_DEVICE);

        // Clip region for scrollable layer rows — prevents overflow into header/footer.
        let clip_top = lc.y + header_spacer;
        let clip_height = (lc.height - header_spacer).max(0.0);
        self.scroll
            .begin(tree, Rect::new(lc.x, clip_top, lc.width, clip_height));
        let clip_parent: Option<NodeId> = self.scroll.clip_node_id();

        let layer_count = self.layers.len();
        self.rows.clear();
        self.rows.resize(layer_count, LayerRowIds::default());
        // Gain slider node IDs are invalidated by the rebuild; reset and
        // re-populate per audio row in build_layer_row.
        self.active_gain_drag = -1;
        self.gain_sliders.clear();
        self.gain_sliders
            .resize_with(layer_count, crate::slider::SliderDragState::default);
        self.gain_slider_resets.clear();
        self.gain_slider_resets.resize_with(layer_count, || None);
        // Only resize cached state vectors if layer count changed —
        // preserve existing values to keep dirty-check logic correct.
        self.cached_mute.resize(layer_count, false);
        self.cached_solo.resize(layer_count, false);
        self.cached_led.resize(layer_count, false);
        self.cached_selected.resize(layer_count, false);
        self.cached_colors.resize(layer_count, Color32::TRANSPARENT);
        self.mute_motion.resize(layer_count, components::ChipMotion::new());

        // Swap layers out to avoid borrow conflict in build_layer_row
        // (takes O(1), avoids cloning the entire Vec)
        let layers_snapshot = std::mem::take(&mut self.layers);

        for i in 0..layer_count {
            let layer = &layers_snapshot[i];
            let height = mapper.get_layer_height(i);
            if height <= 0.0 {
                continue;
            }
            let y_offset = mapper.get_layer_y_offset(i);

            // Build ALL rows (including off-screen) for update-in-place.
            // The CLIPS_CHILDREN clip region handles visual clipping, and
            // the renderer early-outs for nodes outside the clip bounds.

            let is_child = layer.parent_layer_id.is_some();
            let is_last_child = if is_child {
                if i + 1 < layer_count {
                    layers_snapshot[i + 1].parent_layer_id != layer.parent_layer_id
                } else {
                    true
                }
            } else {
                false
            };

            let row = compute_layer_row(
                y_offset,
                height,
                lc.width,
                layer.is_collapsed,
                layer.is_group,
                layer.is_generator,
                layer.is_audio,
                is_child,
                is_last_child,
                layer.is_group && !layer.is_collapsed,
                layer.generator_type.is_some(),
            );

            self.build_layer_row(tree, i, layer, row, self.panel_origin, clip_parent);
            self.cached_mute[i] = layer.is_muted;
            self.cached_solo[i] = layer.is_solo;
            self.cached_led[i] = layer.is_led;
            self.cached_selected[i] = layer.is_selected;
            self.cached_colors[i] = layer.color;
        }

        // Swap layers back
        self.layers = layers_snapshot;

        // Insert indicator (hidden off-screen)
        self.insert_indicator_id = Some(tree.add_panel(
            clip_parent,
            lc.x,
            lc.y - 10.0,
            lc.width,
            INSERT_LINE_H,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                ..UIStyle::default()
            },
        ));

        // No "+ Add Layer" button — layers are added via right-click context menu
        self.add_layer_btn = None;

        self.cache_node_count = tree.count() - self.cache_first_node;
    }

    pub fn update(&mut self, tree: &mut UITree) {
        // §19: breathe the Record button while recording. First, so a selection
        // change (which early-returns below) never skips a pulse frame.
        self.tick_record_pulse(tree);

        // P1 motion foundation: Mute chip hover/press tween. Same "first, so
        // an early return below never skips a frame" reasoning as the record
        // pulse above.
        let dt_ms = self.motion_last_tick.elapsed().as_secs_f32() * 1000.0;
        self.motion_last_tick = Instant::now();
        self.tick_mute_motion(tree, dt_ms);

        // Multi-select: apply pending active layer flags
        if let Some(flags) = self.pending_active_layers.take() {
            for (i, &active) in flags.iter().enumerate() {
                self.set_selection(tree, i, active);
            }
            self.cached_active_layer = self.active_layer.clone();
            return;
        }

        // Single active layer fallback (dirty-check)
        if self.active_layer != self.cached_active_layer {
            let old = self.cached_active_layer.clone();
            let new = self.active_layer.clone();
            self.cached_active_layer = new.clone();

            // Resolve LayerId → index for tree updates
            let old_idx = old.and_then(|id| self.layers.iter().position(|l| l.layer_id == *id));
            let new_idx = new.and_then(|id| self.layers.iter().position(|l| l.layer_id == *id));

            // Deselect old active layer
            if let Some(idx) = old_idx {
                self.set_selection(tree, idx, false);
            }
            // Select new active layer
            if let Some(idx) = new_idx {
                self.set_selection(tree, idx, true);
            }
        }
    }

    pub fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click {
                node_id, modifiers, ..
            } => {
                self.pending_drag_layer = -1;
                self.handle_click(*node_id, *modifiers)
            }
            UIEvent::DoubleClick { node_id, .. } => self.handle_double_click(*node_id),
            // RightClick is handled by node-intent dispatch (register_intents).
            // PointerDown on drag handle → save index for DragBegin fallback.
            // Do NOT return LayerClicked here: that triggers a structural rebuild
            // which invalidates node IDs before DragBegin fires, breaking drag.
            // Selection happens on Click (release) instead — acceptable for drag handles.
            UIEvent::PointerDown { node_id, pos, .. } => {
                // Audio-layer gain slider: begin drag if the press hit its track.
                let gain = self.try_begin_gain_drag(*node_id, pos.x);
                if !gain.is_empty() {
                    return gain;
                }
                for (i, row) in self.rows.iter().enumerate() {
                    if row.id(LayerControl::DragHandle) == Some(*node_id) {
                        self.pending_drag_layer = i as i32;
                        return Vec::new();
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn first_node(&self) -> usize {
        self.cache_first_node
    }
    pub fn node_count(&self) -> usize {
        self.cache_node_count
    }

    /// Node range as (start, end). Reproduces `Panel::node_range`'s default
    /// body — this type no longer implements `Panel` (see the comment above
    /// `impl LayerHeaderPanel` at the top of this block).
    pub fn node_range(&self) -> (usize, usize) {
        let first = self.first_node();
        if first == usize::MAX {
            return (0, 0);
        }
        (first, first + self.node_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LayerType;
    use crate::view::UiLayer;

    fn make_video_layer(name: &str) -> LayerInfo {
        LayerInfo {
            name: name.into(),
            layer_id: name.into(),
            is_collapsed: false,
            is_group: false,
            is_generator: false,
            is_audio: false,
            is_muted: false,
            is_solo: false,
            analysis_only: false,
            is_led: false,
            parent_layer_id: None,
            blend_mode: "Normal".into(),
            generator_type: None,
            clip_count: 5,
            video_folder_path: None,
            source_clip_count: 0,
            midi_note: -1,
            midi_channel: -1,
            midi_device: None,
            midi_all_notes: false,
            audio_gain_db: 0.0,
            audio_send_name: None,
            is_selected: false,
            color: Color32::new(100, 148, 210, 220),
        }
    }

    fn make_audio_layer(name: &str) -> LayerInfo {
        LayerInfo {
            is_audio: true,
            audio_send_name: Some("Drums".into()),
            ..make_video_layer(name)
        }
    }

    fn make_gen_layer(name: &str) -> LayerInfo {
        LayerInfo {
            is_generator: true,
            generator_type: Some("Plasma".into()),
            ..make_video_layer(name)
        }
    }

    fn make_group_layer(name: &str) -> LayerInfo {
        LayerInfo {
            is_group: true,
            ..make_video_layer(name)
        }
    }

    /// Build the real `CoordinateMapper` a test's `LayerInfo` fixture would
    /// produce in the live app — the single Y-layout authority both the
    /// header panel and viewport read (D1). Tests no longer hand-author
    /// `y_offset`/`height`; they derive it from the same structural fields
    /// (`is_collapsed`, `is_group`, `parent_layer_id`) `sync_project_data`
    /// feeds the mapper in production.
    fn mapper_for(layers: &[LayerInfo]) -> CoordinateMapper {
        let ui_layers: Vec<UiLayer> = layers
            .iter()
            .map(|l| UiLayer {
                layer_id: LayerId::new(&l.layer_id),
                parent_layer_id: l.parent_layer_id.as_deref().map(LayerId::new),
                layer_type: if l.is_group {
                    LayerType::Group
                } else if l.is_generator {
                    LayerType::Generator
                } else if l.is_audio {
                    LayerType::Audio
                } else {
                    LayerType::Video
                },
                is_collapsed: l.is_collapsed,
                automation_lane_count: 0,
            })
            .collect();
        let mut mapper = CoordinateMapper::new();
        mapper.rebuild_y_layout(&ui_layers);
        mapper
    }

    #[test]
    fn record_pulse_breathes_between_dim_and_bright() {
        // Midpoint at t=0 (sin 0 = 0 → phase 0.5).
        assert_eq!(
            record_pulse_color(0.0),
            color::mix(color::RECORD_PULSE_DIM, color::RECORD_PULSE_BRIGHT, 0.5)
        );
        // Peak bright a quarter-cycle in, trough dim three-quarters in.
        assert_eq!(
            record_pulse_color(RECORD_PULSE_PERIOD_SECS * 0.25),
            color::RECORD_PULSE_BRIGHT
        );
        assert_eq!(
            record_pulse_color(RECORD_PULSE_PERIOD_SECS * 0.75),
            color::RECORD_PULSE_DIM
        );
        // Always bounded by the two endpoints, never strobing past them.
        for k in 0..50 {
            let c = record_pulse_color(k as f32 * 0.05);
            assert!(c.r >= color::RECORD_PULSE_DIM.r && c.r <= color::RECORD_PULSE_BRIGHT.r);
        }
    }

    #[test]
    fn build_layer_header() {
        let mut tree = UITree::new();
        // Use tall screen so all 3 layers (y=0..420) fit in timeline body.
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();

        let layers = vec![
            make_video_layer("Layer 1"),
            make_video_layer("Layer 2"),
            make_gen_layer("Gen Layer"),
        ];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);

        panel.build(&mut tree, &layout, &mapper, 0.0);

        assert_eq!(panel.layer_count(), 3);
        // All layers should have bg, name, mute, solo, blend_mode
        for i in 0..3 {
            assert!(panel.rows[i].id(LayerControl::Background).is_some(), "layer {} bg", i);
            assert!(panel.rows[i].id(LayerControl::Name).is_some(), "layer {} name", i);
            assert!(panel.rows[i].id(LayerControl::Mute).is_some(), "layer {} mute", i);
            assert!(panel.rows[i].id(LayerControl::Solo).is_some(), "layer {} solo", i);
            assert!(panel.rows[i].id(LayerControl::Blend).is_some(), "layer {} blend", i);
        }
        // Video layers should have folder routing.
        assert!(panel.rows[0].id(LayerControl::Folder).is_some());
        // §C: clip-count line + "+ clip" / "+ new clip" buttons are removed.
        assert_eq!(panel.rows[0].id(LayerControl::Info), None);
        assert_eq!(panel.rows[0].id(LayerControl::NewClip), None);
        assert_eq!(panel.rows[2].id(LayerControl::AddGenClip), None);
        // Insert indicator
        assert!(panel.insert_indicator_id.is_some());
    }

    /// BUG-084/BUG-086 — the Record button's label surfaces the drop count
    /// while recording, and clears it on stop so a fresh recording starts
    /// clean. This is the UI consumer BUG-084 was missing (the content
    /// thread computed `recording_dropped_frames` every tick but nothing
    /// ever read it).
    #[test]
    fn recording_drops_surface_on_record_button_label() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();
        panel.build(&mut tree, &layout, &mapper_for(&[]), 0.0);

        let id = panel.record_btn_id.expect("record button built");
        assert_eq!(tree.get_node(id).unwrap().text.as_deref(), Some("Record Live"));

        panel.set_recording_active(&mut tree, true);
        assert_eq!(tree.get_node(id).unwrap().text.as_deref(), Some("Stop Recording"));

        // Video pool-exhaustion drops + native audio-backpressure drops sum
        // into one label so a performer sees either kind.
        panel.set_recording_drops(&mut tree, 3, 2);
        assert_eq!(
            tree.get_node(id).unwrap().text.as_deref(),
            Some("Stop Recording \u{26A0} 5 dropped")
        );

        // Stopping clears the counter — a new recording starts clean.
        panel.set_recording_active(&mut tree, false);
        assert_eq!(tree.get_node(id).unwrap().text.as_deref(), Some("Record Live"));
    }

    /// BUG-025 regression: the in-place vertical-scroll fast-path must shift
    /// each row's clip rect in lockstep with its controls. Before the fix the
    /// clip stayed put while the controls moved, so controls slid out from
    /// under a stationary clip and rendered greyed/cut mid-scroll.
    #[test]
    fn in_place_scroll_moves_row_clip_with_its_controls() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("Layer 1"), make_video_layer("Layer 2")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let clip0 = panel.rows[0].clip.expect("row 0 has a clip node");
        let mute0 = panel.rows[0].id(LayerControl::Mute).expect("row 0 has a mute control");
        let clip_y0 = tree.get_bounds(clip0).y;
        let mute_y0 = tree.get_bounds(mute0).y;

        // Scroll down 50px; in-place path shifts everything by -50.
        assert!(panel.try_update_vertical_scroll(&mut tree, &layout, 50.0));

        let clip_dy = tree.get_bounds(clip0).y - clip_y0;
        let mute_dy = tree.get_bounds(mute0).y - mute_y0;
        assert!((mute_dy - -50.0).abs() < 0.01, "control shifted by {mute_dy}, expected -50");
        assert!(
            (clip_dy - mute_dy).abs() < 0.01,
            "clip shifted by {clip_dy} but its controls by {mute_dy} — clip must track content"
        );
    }

    #[test]
    fn handle_click_mute_solo() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Mute).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], PanelAction::Layer(LayerAction::ToggleMute(id)) if *id == LayerId::new("L1")));

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Solo).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], PanelAction::Layer(LayerAction::ToggleSolo(id)) if *id == LayerId::new("L1")));
    }

    #[test]
    fn intent_resolves_right_click_anywhere_in_row_to_layer_menu() {
        use crate::intent::{Gesture, IntentRegistry};
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L0"), make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        // A right-click on any control in a row resolves to that layer's menu —
        // the name, mute, and solo controls all map to the same layer index.
        for ctrl in [LayerControl::Name, LayerControl::Mute, LayerControl::Solo] {
            let node = panel.rows[1].id(ctrl);
            let action = intents.resolve(&tree, node, Gesture::RightClick);
            assert!(
                matches!(&action, Some(PanelAction::Editing(EditingAction::LayerHeaderRightClicked(id))) if *id == LayerId::new("L1")),
                "node {node:?} ({ctrl:?}) should resolve to layer 1's menu, got {action:?}"
            );
        }
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Chevron).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert!(matches!(&a[0], PanelAction::Layer(LayerAction::ChevronClicked(id)) if *id == LayerId::new("L1")));
    }

    #[test]
    fn set_mute_state_updates() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        tree.clear_dirty();
        panel.set_mute_state(&mut tree, 0, true);
        assert!(tree.has_dirty());

        // Calling again with same state should not dirty
        tree.clear_dirty();
        panel.set_mute_state(&mut tree, 0, true);
        assert!(!tree.has_dirty());
    }

    #[test]
    fn mute_chip_hover_tweens_background_only_bounds_never_move() {
        // Colour-only motion, per Peter's rule (2026-07-14, BUG-150):
        // animations may change how a node looks, never where it is. The
        // Mute chip's background eases toward hover/press instead of
        // jump-cutting; its bounds (draw + hit geometry) must stay fixed
        // throughout.
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let mute_id = panel.rows[0].id(LayerControl::Mute).expect("mute chip built");
        let rest_bg = tree.get_node(mute_id).unwrap().style.bg_color;
        let rest_bounds = tree.get_bounds(mute_id);

        // Hover in: background eases partway, not an instant jump.
        tree.set_flag(mute_id, UIFlags::HOVERED);
        panel.tick_mute_motion(&mut tree, 45.0); // halfway through MOTION_FAST (90ms)
        let mid_bg = tree.get_node(mute_id).unwrap().style.bg_color;
        assert_ne!(mid_bg, rest_bg, "background should have moved partway toward hover");
        assert_eq!(tree.get_bounds(mute_id), rest_bounds, "bounds must not move on hover");

        panel.tick_mute_motion(&mut tree, 45.0); // finishes the hover-in tween

        // Press: colour settles fully pressed, bounds still untouched.
        tree.set_flag(mute_id, UIFlags::PRESSED);
        panel.tick_mute_motion(&mut tree, 90.0); // full MOTION_FAST press-in
        assert_eq!(tree.get_bounds(mute_id), rest_bounds, "bounds must not move on press");

        // Release: eases back, no permanent drift in colour, bounds fixed throughout.
        tree.clear_flag(mute_id, UIFlags::PRESSED);
        tree.clear_flag(mute_id, UIFlags::HOVERED);
        panel.tick_mute_motion(&mut tree, 90.0);
        panel.tick_mute_motion(&mut tree, 90.0);
        assert_eq!(
            tree.get_node(mute_id).unwrap().style.bg_color,
            rest_bg,
            "background returns exactly to rest"
        );
        assert_eq!(tree.get_bounds(mute_id), rest_bounds, "bounds never moved, start to finish");
    }

    /// BUG-150 regression: scrolling the layer list must not leave the Mute
    /// chip's hit geometry stale. Before the fix, `tick_mute_motion` snapped
    /// the chip's bounds back to a build-time-cached Y (`mute_base_y`) on the
    /// first hover after any scroll, desyncing draw + hit bounds from the
    /// row's true (scrolled) position — the exact sequence that made mute
    /// require two clicks on stage.
    #[test]
    fn mute_chip_bounds_stay_in_row_after_scroll_then_hover() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("Layer 1"), make_video_layer("Layer 2")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let mute_id = panel.rows[0].id(LayerControl::Mute).expect("row 0 has a mute control");
        let row_bounds_before = panel.rows[0]
            .clip
            .map(|clip_id| tree.get_bounds(clip_id))
            .expect("row 0 has a clip");

        // Scroll the layer list — the fast path shifts every row (and its
        // clip) by delta_y in lockstep.
        assert!(panel.try_update_vertical_scroll(&mut tree, &layout, 50.0));
        let row_bounds_after_scroll = tree.get_bounds(panel.rows[0].clip.unwrap());
        let scrolled_mute_y = tree.get_bounds(mute_id).y;
        assert!(
            (row_bounds_after_scroll.y - (row_bounds_before.y - 50.0)).abs() < 0.01,
            "sanity: row actually moved by the scroll"
        );

        // Hover the chip immediately after scrolling — this is the exact
        // BUG-150 trigger (first tick of the press/hover tween post-scroll).
        tree.set_flag(mute_id, UIFlags::HOVERED);
        panel.tick_mute_motion(&mut tree, 45.0);

        let mute_bounds = tree.get_bounds(mute_id);
        assert!(
            (mute_bounds.y - scrolled_mute_y).abs() < 0.01,
            "mute chip's Y must stay at the scrolled position, not snap back to a stale build-time Y: \
             got {}, expected {}",
            mute_bounds.y,
            scrolled_mute_y
        );
        assert!(
            mute_bounds.y >= row_bounds_after_scroll.y
                && mute_bounds.y + mute_bounds.height <= row_bounds_after_scroll.y + row_bounds_after_scroll.height,
            "mute chip bounds must stay inside its row's clip after scroll+hover: \
             chip y={} h={}, row y={} h={}",
            mute_bounds.y,
            mute_bounds.height,
            row_bounds_after_scroll.y,
            row_bounds_after_scroll.height
        );
    }

    #[test]
    fn build_with_group() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();

        let mut child = make_video_layer("Child");
        child.parent_layer_id = Some("Group".into());

        let layers = vec![make_group_layer("Group"), child];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);

        panel.build(&mut tree, &layout, &mapper, 0.0);

        // Group has connector
        assert!(panel.rows[0].id(LayerControl::Connector).is_some());
        // Child has accent bar and bottom border (last child)
        assert!(panel.rows[1].id(LayerControl::AccentBar).is_some());
        assert!(panel.rows[1].id(LayerControl::BottomBorder).is_some());
    }

    #[test]
    fn folder_path_extraction() {
        assert_eq!(folder_path_text(&None, 0), "None");
        assert_eq!(folder_path_text(&Some(String::new()), 0), "None");
        assert_eq!(
            folder_path_text(&Some("/Users/test/Videos/Drums/".into()), 12),
            "Drums/ (12)"
        );
        assert_eq!(
            folder_path_text(&Some("C:\\Videos\\Synth".into()), 5),
            "Synth/ (5)"
        );
    }

    #[test]
    fn collapsed_layer_has_no_expanded_controls() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();

        let mut layer = make_video_layer("Collapsed");
        layer.is_collapsed = true;

        let layers = vec![layer];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        // Collapsed layer should NOT have folder, new_clip, midi controls
        assert_eq!(panel.rows[0].id(LayerControl::Folder), None);
        assert_eq!(panel.rows[0].id(LayerControl::NewClip), None);
        assert_eq!(panel.rows[0].id(LayerControl::MidiInput), None);
        assert_eq!(panel.rows[0].id(LayerControl::ChDropdown), None);
        // But should still have mute/solo/blend
        assert!(panel.rows[0].id(LayerControl::Mute).is_some());
        assert!(panel.rows[0].id(LayerControl::Solo).is_some());
        assert!(panel.rows[0].id(LayerControl::Blend).is_some());
    }

    #[test]
    fn handle_double_click_name() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("L1")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let event = UIEvent::DoubleClick {
            node_id: panel.rows[0].id(LayerControl::Name).unwrap(),
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        };
        let a = panel.handle_event(&event, &tree);
        assert_eq!(a.len(), 1);
        assert!(matches!(&a[0], PanelAction::Layer(LayerAction::LayerDoubleClicked(id)) if *id == LayerId::new("L1")));
    }

    #[test]
    fn accessors_out_of_range() {
        let panel = LayerHeaderPanel::new();
        assert_eq!(panel.blend_mode_node_id(0), None);
        assert_eq!(panel.midi_channel_node_id(99), None);
        assert_eq!(panel.name_node_id(0), None);
    }

    // ── Layout equivalence gate ─────────────────────────────────────
    //
    // Frozen, independent copy of the card geometry captured at the
    // descriptor refactor. The live `compute_layer_row` is asserted equal to
    // this oracle rect-for-rect for every layer type. If a future edit drifts
    // the live geometry, this frozen copy disagrees and the gate fails —
    // exactly the "descriptor layout equals old compute_layer_row" guard the
    // design calls for.
    #[allow(clippy::too_many_arguments)]
    fn oracle_row(
        y_offset: f32,
        height: f32,
        panel_width: f32,
        is_collapsed: bool,
        is_group: bool,
        is_generator: bool,
        is_audio: bool,
        is_child: bool,
        is_last_child: bool,
        is_group_expanded: bool,
        has_gen_label: bool,
    ) -> LayerRowData {
        use LayerControl as C;
        let mut d = LayerRowData::default();
        let w = if panel_width > 0.0 {
            panel_width
        } else {
            color::LAYER_CONTROLS_WIDTH
        };
        let left_indent = if is_child { CHILD_INDENT } else { 0.0 };
        let pad = PAD + left_indent;
        let right_pad = PAD;
        let mut y = y_offset + PAD;
        let card_x = left_indent;
        let card_w = (w - card_x).max(1.0);
        d.set(C::Background, Rect::new(card_x, y_offset, card_w, height));
        if is_child {
            d.set(C::AccentBar, Rect::new(0.0, y_offset, ACCENT_W, height));
        }
        if is_group && is_group_expanded {
            d.set(
                C::Connector,
                Rect::new(0.0, y_offset + height * 0.5, ACCENT_W, height * 0.5),
            );
        }
        if is_child && is_last_child {
            d.set(
                C::BottomBorder,
                Rect::new(card_x, y_offset + height - BORDER_H, card_w, BORDER_H),
            );
        }
        d.set(C::SelectAccent, Rect::new(card_x, y_offset, SEL_ACCENT_W, height));
        let chevron_w = CHEVRON_W;
        d.set(C::Chevron, Rect::new(pad, y, CHEVRON_W, BTN_H));
        let name_left = pad + chevron_w + if chevron_w > 0.0 { TOP_GAP } else { 0.0 };
        let handle_x = w - right_pad - HANDLE_W - 8.0;
        if is_collapsed && has_gen_label {
            let label_x = handle_x - TOP_GAP - GEN_LABEL_COLLAPSED_W;
            let name_w = (label_x - TOP_GAP - name_left).max(20.0);
            d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
            d.set(
                C::GenType,
                Rect::new(label_x, y, GEN_LABEL_COLLAPSED_W, NAME_H),
            );
        } else {
            let name_w = (handle_x - name_left - TOP_GAP).max(20.0);
            d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
        }
        d.set(C::DragHandle, Rect::new(handle_x, y, HANDLE_W, BTN_H));
        y += ROW_STEP;
        let mut btn_x = pad;
        d.set(C::Mute, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 6.0;
        d.set(C::Solo, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 6.0;
        if is_audio {
            d.set(C::Analysis, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
            if is_collapsed {
                d.set(
                    C::Separator,
                    Rect::new(card_x, y_offset + height - SEP_H, (w - card_x).max(1.0), SEP_H),
                );
                return d;
            }
            let right_edge = w - right_pad - RIGHT_GUTTER;
            let gain_x = btn_x + MS_BTN_W + 6.0;
            d.set(
                C::Gain,
                Rect::new(gain_x, y, (right_edge - gain_x).max(20.0), BTN_H),
            );
            let send_y = y + BTN_H;
            d.set(
                C::Send,
                Rect::new(pad, send_y, (right_edge - pad).max(20.0), BTN_H),
            );
            d.set(
                C::Separator,
                Rect::new(card_x, y_offset + height - SEP_H, (w - card_x).max(1.0), SEP_H),
            );
            return d;
        }
        d.set(C::Led, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 6.0;
        let dd_w = (w - btn_x - right_pad - RIGHT_GUTTER).max(20.0);
        d.set(C::Blend, Rect::new(btn_x, y, dd_w, BTN_H));
        y += BTN_H;
        let sep_h = if is_group {
            color::GROUP_SEPARATOR_HEIGHT
        } else {
            SEP_H
        };
        if is_collapsed && !is_group {
            d.set(
                C::Separator,
                Rect::new(card_x, y_offset + height - sep_h, card_w, sep_h),
            );
            return d;
        }
        // §D routing form — aligned [label | value] rows (mirrors compute_layer_row
        // rect-for-rect; the equivalence gate enforces it).
        if !is_group {
            let right_edge = w - right_pad - RIGHT_GUTTER;
            let div_y = (y + MIX_DIVIDER_PAD).round();
            d.set(
                C::MixDivider,
                Rect::new(pad, div_y, (right_edge - pad).max(1.0), MIX_DIVIDER_THICK),
            );
            y = div_y + MIX_DIVIDER_THICK + MIX_DIVIDER_PAD;
            let val_x = pad + LBL_W + 6.0;
            let val_w = (right_edge - val_x).max(20.0);
            let mode_x = right_edge - MODE_TOGGLE_W;
            if !is_generator {
                d.set(C::PathLabel, Rect::new(pad, y, LBL_W, BTN_H));
                d.set(C::Folder, Rect::new(val_x, y, val_w, BTN_H));
                y += BTN_H + ROUTING_ROW_GAP;
            } else if has_gen_label {
                d.set(C::GenType, Rect::new(pad, y, (right_edge - pad).max(20.0), BTN_H));
                y += BTN_H + ROUTING_ROW_GAP;
            }
            d.set(C::MidiLabel, Rect::new(pad, y, LBL_W, BTN_H));
            d.set(
                C::MidiInput,
                Rect::new(val_x, y, (mode_x - 4.0 - val_x).max(10.0), BTN_H),
            );
            d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
            y += BTN_H + ROUTING_ROW_GAP;
            d.set(C::ChLabel, Rect::new(pad, y, LBL_W, BTN_H));
            d.set(C::ChDropdown, Rect::new(val_x, y, val_w, BTN_H));
            y += BTN_H + ROUTING_ROW_GAP;
            d.set(C::DevLabel, Rect::new(pad, y, LBL_W, BTN_H));
            d.set(C::DevDropdown, Rect::new(val_x, y, val_w, BTN_H));
        }
        let _ = y;
        d.set(
            C::Separator,
            Rect::new(card_x, y_offset + height - sep_h, card_w, sep_h),
        );
        d
    }

    fn assert_row_eq(a: &LayerRowData, b: &LayerRowData, case: &str) {
        for &c in &LayerControl::ALL {
            assert_eq!(a.has(c), b.has(c), "{case}: presence of {c:?}");
            if a.has(c) {
                assert_eq!(a.rect(c), b.rect(c), "{case}: rect of {c:?}");
            }
        }
    }

    #[test]
    fn layout_matches_frozen_oracle() {
        // (is_collapsed, is_group, is_generator, is_audio, is_child, is_last, is_grp_exp, label)
        let cases = [
            (false, false, false, false, false, false, false, "video"),
            (false, false, true, false, false, false, false, "generator"),
            (false, true, false, false, false, false, true, "group"),
            (true, false, false, false, false, false, false, "collapsed-video"),
            (true, false, true, false, false, false, false, "collapsed-gen"),
            (false, false, false, false, true, true, false, "child-last"),
            (false, false, false, true, false, false, false, "audio"),
            (true, false, false, true, false, false, false, "collapsed-audio"),
        ];
        for (coll, grp, genr, aud, child, last, gexp, label) in cases {
            // Synthetic equivalence check, not the gating test — `genr` doubles
            // as `has_gen_label` here since both fns receive the identical
            // value either way.
            let live = compute_layer_row(0.0, 140.0, 300.0, coll, grp, genr, aud, child, last, gexp, genr);
            let oracle = oracle_row(0.0, 140.0, 300.0, coll, grp, genr, aud, child, last, gexp, genr);
            assert_row_eq(&live, &oracle, label);
        }
    }

    #[test]
    fn audio_card_controls() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_audio_layer("Drums In")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        // Audio layers expose Mute / Solo / Gain / Send …
        assert!(panel.rows[0].id(LayerControl::Mute).is_some());
        assert!(panel.rows[0].id(LayerControl::Solo).is_some());
        assert!(panel.rows[0].id(LayerControl::Gain).is_some());
        assert!(panel.rows[0].id(LayerControl::Send).is_some());
        // … and none of the video/generator/LED/blend controls.
        assert_eq!(panel.rows[0].id(LayerControl::Led), None);
        assert_eq!(panel.rows[0].id(LayerControl::Blend), None);
        assert_eq!(panel.rows[0].id(LayerControl::Folder), None);
        assert_eq!(panel.rows[0].id(LayerControl::MidiInput), None);

        // Send is click-routable to its picker.
        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Send).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert!(
            matches!(a.as_slice(), [PanelAction::Root(RootAction::AudioSendClicked(id))] if *id == LayerId::new("Drums In"))
        );
    }

    #[test]
    fn gain_track_right_click_resolves_to_slider_reset_at_unity_not_the_row_menu() {
        // BUG-061: the gain track never had a reset gesture before this — a
        // right-click on it fell through to the whole-row LayerHeaderRightClicked
        // context menu (register_intents registers that on every row node,
        // gain track included). The gain-specific registration must be added
        // AFTER that loop so it wins for this one node.
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_audio_layer("Drums In")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let gain_track = panel.gain_sliders[0].track_id().unwrap();
        match reg.resolve(&tree, Some(gain_track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) => {
                assert!(matches!(
                    *changed,
                    PanelAction::Scrub(ValueRef::LayerAudioGain(ref id), ScrubPhase::Move(ScrubValue::Scalar(v)))
                        if *id == LayerId::new("Drums In") && v.abs() < f32::EPSILON
                ));
            }
            other => panic!("expected SliderReset, got {other:?}"),
        }
    }

    // ── P0.5 gate: generator label, both states ───────────────────────
    //
    // `docs/TIMELINE_LAYOUT_P0_SPEC.md` P0.5: a display-only generator-name
    // label renders iff `LayerInfo.generator_type.is_some()` — present for a
    // generator layer, absent for a video layer — in both the collapsed and
    // expanded row geometry.

    #[test]
    fn generator_label_present_iff_generator_type_is_some() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        let layers = vec![make_video_layer("Video"), make_gen_layer("Gen")];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        assert_eq!(
            panel.rows[0].id(LayerControl::GenType),
            None,
            "video layer (generator_type: None) must not get a generator label"
        );
        assert!(
            panel.rows[1].id(LayerControl::GenType).is_some(),
            "generator layer (generator_type: Some) must get its label"
        );
    }

    #[test]
    fn generator_label_present_iff_generator_type_is_some_collapsed() {
        // Same gate, collapsed geometry — the label moves next to Name instead
        // of into the routing-form slot, but the presence rule is identical.
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();

        let mut video = make_video_layer("Video");
        video.is_collapsed = true;
        let mut gen_layer = make_gen_layer("Gen");
        gen_layer.is_collapsed = true;

        let layers = vec![video, gen_layer];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        assert_eq!(panel.rows[0].id(LayerControl::GenType), None);
        assert!(panel.rows[1].id(LayerControl::GenType).is_some());
    }

    #[test]
    fn generator_label_collapsed_never_widens_row() {
        // Geometry-level check mirroring `collapsed_audio_content_fits_
        // collapsed_track_height`: the collapsed generator label is carved out
        // of Name's own width budget, so the row's total content width never
        // grows past what a non-generator collapsed row already uses.
        let row_gen =
            compute_layer_row(0.0, 140.0, 300.0, true, false, true, false, false, false, false, true);
        let row_video =
            compute_layer_row(0.0, 140.0, 300.0, true, false, false, false, false, false, false, false);

        let label = row_gen.rect(LayerControl::GenType);
        let name_gen = row_gen.rect(LayerControl::Name);
        let handle_gen = row_gen.rect(LayerControl::DragHandle);
        let handle_video = row_video.rect(LayerControl::DragHandle);

        // DragHandle's x (the row's right-hand boundary for this content) is
        // unchanged by the label's presence.
        assert_eq!(handle_gen.x, handle_video.x, "label must not push DragHandle right");
        // The label sits entirely between Name and DragHandle — it never
        // extends past the row's existing right edge.
        assert!(label.x + label.width <= handle_gen.x);
        // Name shrank to make room — it does not overlap the label.
        assert!(name_gen.x + name_gen.width <= label.x);
    }

    // ── P0.2 gate: D4 audio fit ──────────────────────────────────────
    //
    // `docs/TIMELINE_LAYOUT_P0_SPEC.md` D4: collapsed audio drops the
    // never-collapse exception and gets the same collapsed chrome as every
    // other layer type; expanded audio fits three rows inside
    // `TrackHeight::Normal` with no per-card height exception.

    #[test]
    fn collapsed_audio_has_no_expanded_controls() {
        // Mirrors `collapsed_layer_has_no_expanded_controls` for the audio
        // layer type: collapsed audio must not carry Gain/Send (the RC3
        // overflow this phase removes), only the same Mute/Solo/Analysis
        // button row every other collapsed layer keeps.
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();

        let mut layer = make_audio_layer("Collapsed Audio");
        layer.is_collapsed = true;

        let layers = vec![layer];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        assert_eq!(panel.rows[0].id(LayerControl::Gain), None);
        assert_eq!(panel.rows[0].id(LayerControl::Send), None);
        assert!(panel.rows[0].id(LayerControl::Mute).is_some());
        assert!(panel.rows[0].id(LayerControl::Solo).is_some());
        assert!(panel.rows[0].id(LayerControl::Analysis).is_some());
    }

    #[test]
    fn collapsed_audio_content_fits_collapsed_track_height() {
        // Geometry-level check on the same card the panel-level test above
        // exercises: every control the collapsed audio card keeps must sit
        // fully inside `TrackHeight::Collapsed`, mirroring
        // `expanded_audio_content_fits_normal_track_height` below.
        let height = color::COLLAPSED_TRACK_HEIGHT;
        let row = compute_layer_row(0.0, height, 300.0, true, false, false, true, false, false, false, false);
        assert!(!row.has(LayerControl::Gain));
        assert!(!row.has(LayerControl::Send));
        for c in [LayerControl::Mute, LayerControl::Solo, LayerControl::Analysis] {
            assert!(row.has(c), "collapsed audio row missing {c:?}");
            let r = row.rect(c);
            assert!(
                r.y + r.height <= height,
                "{c:?} bottom {} exceeds TrackHeight::Collapsed {height}",
                r.y + r.height
            );
        }
    }

    #[test]
    fn expanded_audio_content_fits_normal_track_height() {
        // The RC3/D4 overflow this phase fixes: Gain + Send used to spill
        // past whatever height the mapper assigned. Assert every control's
        // bottom edge sits inside `TrackHeight::Normal` — the state-only
        // height budget the card must fit without a per-type exception.
        let height = color::TRACK_HEIGHT;
        let row = compute_layer_row(0.0, height, 300.0, false, false, false, true, false, false, false, false);
        for c in [
            LayerControl::Mute,
            LayerControl::Solo,
            LayerControl::Analysis,
            LayerControl::Gain,
            LayerControl::Send,
        ] {
            assert!(row.has(c), "expanded audio row missing {c:?}");
            let r = row.rect(c);
            assert!(
                r.y + r.height <= height,
                "{c:?} bottom {} exceeds TrackHeight::Normal {height}",
                r.y + r.height
            );
        }
        // Gain no longer stacks below M|S|A as its own row — it shares the
        // button row's y with Mute/Solo/Analysis (the slot video cards give
        // Blend).
        let mute_y = row.rect(LayerControl::Mute).y;
        assert_eq!(row.rect(LayerControl::Gain).y, mute_y, "Gain joins the M|S|A row");
        // Gain and Analysis must not overlap horizontally.
        let analysis = row.rect(LayerControl::Analysis);
        let gain = row.rect(LayerControl::Gain);
        assert!(
            gain.x >= analysis.x + analysis.width,
            "Gain (x={}) overlaps Analysis (right edge={})",
            gain.x,
            analysis.x + analysis.width
        );
    }

    #[test]
    fn gain_db_mapping() {
        assert!((gain_db_to_norm(GAIN_DB_MIN) - 0.0).abs() < 1e-6);
        assert!((gain_db_to_norm(GAIN_DB_MAX) - 1.0).abs() < 1e-6);
        // 0 dB sits proportionally between the floor and ceiling.
        let expected = (0.0 - GAIN_DB_MIN) / (GAIN_DB_MAX - GAIN_DB_MIN);
        assert!((gain_db_to_norm(0.0) - expected).abs() < 1e-6);
        // Out-of-range clamps.
        assert_eq!(gain_db_to_norm(-200.0), 0.0);
        assert_eq!(gain_db_to_norm(200.0), 1.0);
        assert_eq!(gain_db_text(-60.0), "-inf");
        assert_eq!(gain_db_text(0.0), "+0.0 dB");
    }

    // ── P0.1 gate: header/lane Y agreement ───────────────────────────
    //
    // `docs/TIMELINE_LAYOUT_P0_SPEC.md` D1: the header panel must query the
    // same `CoordinateMapper` the viewport's lanes read
    // (`viewport/coordinate.rs` `track_y`) — never a copy. This asserts the
    // header's actual built row rects land at exactly the mapper's
    // `get_layer_y_offset`/`get_layer_height` for every layer, across the
    // three structural states the spec names: collapsed, hidden-child (of a
    // collapsed group), and group. Because both columns call the identical
    // mapper method, this is the strongest guard against RC2 (headers
    // drawing from a stale copy) regressing.
    #[test]
    fn header_rows_agree_with_mapper_y_collapsed_hidden_child_group() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();

        let mut collapsed = make_video_layer("Collapsed");
        collapsed.is_collapsed = true;

        let mut group = make_group_layer("Group");
        group.is_collapsed = true; // collapsed group hides its child below

        let mut hidden_child = make_video_layer("Hidden");
        hidden_child.parent_layer_id = Some("Group".into());

        let layers = vec![make_video_layer("Normal"), collapsed, group, hidden_child];
        let mapper = mapper_for(&layers);
        panel.set_layers(layers);
        panel.build(&mut tree, &layout, &mapper, 0.0);

        for i in 0..panel.layer_count() {
            let expected_height = mapper.get_layer_height(i);
            if expected_height <= 0.0 {
                // Hidden child of a collapsed group: build() skips the row
                // entirely, matching the zero-height lanes side sees too.
                assert!(
                    panel.rows[i].id(LayerControl::Background).is_none(),
                    "row {i} (hidden child) should have no built row"
                );
                continue;
            }
            let bg_id = panel.rows[i]
                .id(LayerControl::Background)
                .unwrap_or_else(|| panic!("row {i} should have a Background node"));
            let rect = tree.get_bounds(bg_id);
            // Strip panel_origin to recover the panel-local Y the mapper
            // assigned — the exact value the viewport's lanes read via the
            // same `mapper.get_layer_y_offset(i)` call.
            let local_y = rect.y - panel.panel_origin.y;
            let expected_y = mapper.get_layer_y_offset(i);
            assert!(
                (local_y - expected_y).abs() < 0.01,
                "row {i} y mismatch: header={local_y} mapper={expected_y}"
            );
            assert!(
                (rect.height - expected_height).abs() < 0.01,
                "row {i} height mismatch: header={} mapper={expected_height}",
                rect.height
            );
        }
    }
}
