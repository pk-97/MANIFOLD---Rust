use super::{Panel, PanelAction};
use crate::chrome::{ChromeHost, Pad, Sizing, View, components};
use crate::color::{self, darken, lighten};
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::scroll_container::ScrollContainer;
use crate::tree::UITree;
use manifold_foundation::LayerId;
use crate::types::note_number_to_name;

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
const INFO_H: f32 = color::LAYER_CTRL_INFO_ROW_HEIGHT;
const SEP_H: f32 = color::LAYER_CTRL_SEPARATOR_HEIGHT;
const RIGHT_GUTTER: f32 = color::LAYER_CTRL_RIGHT_GUTTER;
const TOP_GAP: f32 = color::LAYER_CTRL_TOP_ROW_GAP;
const FOLDER_W: f32 = color::LAYER_CTRL_FOLDER_BTN_WIDTH;
const NEW_CLIP_W: f32 = color::LAYER_CTRL_NEW_CLIP_BTN_WIDTH;
const ADD_GEN_W: f32 = color::LAYER_CTRL_ADD_GEN_CLIP_BTN_WIDTH;
const GEN_TYPE_H: f32 = color::LAYER_CTRL_GEN_TYPE_ROW_HEIGHT;
const BADGE_SIZE: f32 = color::LAYER_CTRL_TYPE_BADGE_SIZE;
// Widths for the MIDI trigger-mode toggle and per-layer device dropdown
// packed into the existing MIDI / CH rows (no new row, preserves TRACK_HEIGHT).
const MODE_TOGGLE_W: f32 = 32.0;
const DEV_LBL_W: f32 = 24.0;
const MIDI_LBL_W: f32 = color::LAYER_CTRL_MIDI_LABEL_WIDTH;
const CH_LBL_W: f32 = color::LAYER_CTRL_CHANNEL_LABEL_WIDTH;
const ACCENT_W: f32 = color::GROUP_ACCENT_BAR_WIDTH;
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

const NAME_FONT: u16 = color::FONT_LABEL;
const SMALL_FONT: u16 = color::FONT_SMALL;
const BTN_FONT: u16 = color::FONT_BODY;
const LH_BTN_RADIUS: f32 = color::SMALL_RADIUS; // §14.4: local copy → token alias

// ── Style helpers ───────────────────────────────────────────────────

// Mute / Solo / LED / Analysis are all the same state-button mechanic — filled
// with their identity colour when on, a neutral chip when off — so they delegate
// to `components::state_button_style` and differ only in the carve-out hue (plus
// this card's smaller font + tighter radius). One mechanic, four hues.

fn mute_style(muted: bool) -> UIStyle {
    state_btn(color::MUTED_COLOR, muted)
}

fn analysis_style(analysis: bool) -> UIStyle {
    state_btn(color::ANALYSIS_COLOR, analysis)
}

fn solo_style(solo: bool) -> UIStyle {
    state_btn(color::SOLO_COLOR, solo)
}

fn led_style(led: bool) -> UIStyle {
    state_btn(color::LED_COLOR, led)
}

/// The layer-card flavour of [`components::state_button_style`]: the shared
/// on/off mechanic with this panel's smaller font + tighter radius.
fn state_btn(active_color: Color32, active: bool) -> UIStyle {
    UIStyle {
        font_size: BTN_FONT,
        corner_radius: LH_BTN_RADIUS,
        ..components::state_button_style(active_color, active)
    }
}

fn small_button_style() -> UIStyle {
    UIStyle {
        bg_color: color::BUTTON_DIM,
        hover_bg_color: color::BUTTON_HIGHLIGHTED,
        pressed_bg_color: color::BUTTON_PRESSED,
        text_color: color::TEXT_WHITE_C32,
        font_size: SMALL_FONT,
        corner_radius: LH_BTN_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

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

fn bg_style(selected: bool, layer_color: Color32) -> UIStyle {
    let bg = if selected {
        lighten(layer_color, 30)
    } else {
        layer_color
    };
    let hover = lighten(bg, 15);
    let pressed = darken(bg, 10);
    UIStyle {
        bg_color: bg,
        hover_bg_color: hover,
        pressed_bg_color: pressed,
        corner_radius: color::BUTTON_RADIUS,
        ..UIStyle::default()
    }
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
    /// "No send".
    pub audio_send_name: Option<String>,
    /// Y offset within the layer controls panel (panel-local).
    pub y_offset: f32,
    /// Height of this layer row.
    pub height: f32,
    pub is_selected: bool,
    /// Layer color (auto-assigned or user-set).
    pub color: Color32,
}

impl LayerInfo {
    /// The type-badge icon for this layer (§24 5d). Group / audio / generator are
    /// flagged explicitly; everything else is a video layer.
    fn badge_icon(&self) -> crate::icons::Icon {
        use crate::icons::Icon;
        if self.is_group {
            Icon::LayerGroup
        } else if self.is_audio {
            Icon::LayerAudio
        } else if self.is_generator {
            Icon::LayerGenerator
        } else {
            Icon::LayerVideo
        }
    }
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
    Chevron,
    Name,
    DragHandle,
    GenType,
    Mute,
    Solo,
    Led,
    Blend,
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
    /// Layer-type badge (§24 5d): a type glyph in the name row so type is read
    /// from an icon, not from the header restructuring by type. Decoration —
    /// non-interactive, drawn last (on top of the background).
    TypeBadge,
}

const N_CONTROLS: usize = 29;

impl LayerControl {
    /// All controls in declaration (build / z) order.
    const ALL: [LayerControl; N_CONTROLS] = [
        LayerControl::Background,
        LayerControl::AccentBar,
        LayerControl::Connector,
        LayerControl::BottomBorder,
        LayerControl::Chevron,
        LayerControl::Name,
        LayerControl::DragHandle,
        LayerControl::GenType,
        LayerControl::Mute,
        LayerControl::Solo,
        LayerControl::Led,
        LayerControl::Blend,
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
        LayerControl::TypeBadge,
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
) -> LayerRowData {
    use LayerControl as C;
    let mut d = LayerRowData::default();
    let w = if panel_width > 0.0 {
        panel_width
    } else {
        color::LAYER_CONTROLS_WIDTH
    };

    d.set(C::Background, Rect::new(0.0, y_offset, w, height));

    let left_indent = if is_child { CHILD_INDENT } else { 0.0 };
    let pad = PAD + left_indent;
    let mut y = y_offset + PAD;

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
            Rect::new(0.0, y_offset + height - BORDER_H, w, BORDER_H),
        );
    }

    // ── Top row: Chevron | TypeBadge | Name | DragHandle ──
    let chevron_w = CHEVRON_W;
    d.set(C::Chevron, Rect::new(pad, y, CHEVRON_W, BTN_H));

    // Type badge (§24 5d): a square glyph between the chevron and the name,
    // vertically centred in the name-row height. Same slot for every type — the
    // glyph differs, the layout doesn't.
    let badge_x = pad + chevron_w + if chevron_w > 0.0 { TOP_GAP } else { 0.0 };
    let badge_y = y + (BTN_H - BADGE_SIZE) * 0.5;
    d.set(C::TypeBadge, Rect::new(badge_x, badge_y, BADGE_SIZE, BADGE_SIZE));

    let name_left = badge_x + BADGE_SIZE + TOP_GAP;
    let handle_x = w - pad - HANDLE_W - 8.0;
    let name_w = (handle_x - name_left - TOP_GAP).max(20.0);
    d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
    d.set(C::DragHandle, Rect::new(handle_x, y, HANDLE_W, BTN_H));

    y += ROW_STEP;

    // ── Generator type row (expanded only) ──
    // §24 5d: a collapsed generator is sized like every other collapsed track
    // (the Collapsed preset, 48px) and shows its type via the name-row badge, not
    // a dedicated row — so the subtype name only appears when there's room to
    // expand. Adding it while collapsed is exactly what forced the old taller-by-
    // type collapsed-generator height.
    if is_generator && !is_collapsed {
        let gen_w = w - name_left - pad;
        d.set(C::GenType, Rect::new(name_left, y, gen_w, GEN_TYPE_H));
        y += GEN_TYPE_H;
    }

    // ── Button row: M | S | [L | BlendMode] ──
    // Audio layers carry only Mute / Solo here, then a Gain row and a Send row;
    // they have no LED output, blend mode, folder, clip, or MIDI controls.
    let mut btn_x = pad;
    d.set(C::Mute, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + 2.0;
    d.set(C::Solo, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + 2.0;

    if is_audio {
        return compute_audio_row(d, y_offset, height, w, pad, btn_x, y);
    }

    d.set(C::Led, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
    btn_x += MS_BTN_W + 4.0;

    let dd_w = (w - btn_x - pad - RIGHT_GUTTER).max(20.0);
    d.set(C::Blend, Rect::new(btn_x, y, dd_w, BTN_H));

    y += BTN_H + 2.0;

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
            Rect::new(0.0, y_offset + height - sep_h, w, sep_h),
        );
        return d;
    }

    // ── Info label ──
    d.set(C::Info, Rect::new(pad, y, w - pad * 2.0, INFO_H));
    y += 16.0;

    if is_group {
        y += 2.0;
    } else if is_generator {
        d.set(C::AddGenClip, Rect::new(pad, y, ADD_GEN_W, BTN_H));
        y += BTN_H + 2.0;

        // MIDI note + trigger-mode toggle (share one row)
        d.set(C::MidiLabel, Rect::new(pad, y, MIDI_LBL_W, BTN_H));
        let gen_midi_x = pad + MIDI_LBL_W + 2.0;
        let right_edge = w - pad - RIGHT_GUTTER;
        let mode_x = right_edge - MODE_TOGGLE_W;
        d.set(
            C::MidiInput,
            Rect::new(gen_midi_x, y, (mode_x - 4.0 - gen_midi_x).max(10.0), BTN_H),
        );
        d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
        y += ROW_STEP;

        // MIDI channel + device dropdown (share one row)
        d.set(C::ChLabel, Rect::new(pad, y, CH_LBL_W, BTN_H));
        let gen_ch_x = pad + CH_LBL_W + 2.0;
        let ch_w = 44.0;
        d.set(C::ChDropdown, Rect::new(gen_ch_x, y, ch_w, BTN_H));
        let dev_lbl_x = gen_ch_x + ch_w + 6.0;
        d.set(C::DevLabel, Rect::new(dev_lbl_x, y, DEV_LBL_W, BTN_H));
        let dev_x = dev_lbl_x + DEV_LBL_W + 2.0;
        d.set(
            C::DevDropdown,
            Rect::new(dev_x, y, (right_edge - dev_x).max(10.0), BTN_H),
        );
    } else {
        // Folder | PathLabel | +new clip
        d.set(C::Folder, Rect::new(pad, y, FOLDER_W, BTN_H));
        let path_left = pad + FOLDER_W + 4.0;
        let new_clip_x = w - pad - NEW_CLIP_W;
        let path_w = (new_clip_x - path_left - 4.0).max(10.0);
        d.set(C::PathLabel, Rect::new(path_left, y, path_w, BTN_H));
        d.set(C::NewClip, Rect::new(new_clip_x, y, NEW_CLIP_W, BTN_H));
        y += ROW_STEP;

        // MIDI note + trigger-mode toggle (share one row)
        d.set(C::MidiLabel, Rect::new(pad, y, MIDI_LBL_W, BTN_H));
        let midi_x = pad + MIDI_LBL_W + 2.0;
        let right_edge = w - pad - RIGHT_GUTTER;
        let mode_x = right_edge - MODE_TOGGLE_W;
        d.set(
            C::MidiInput,
            Rect::new(midi_x, y, (mode_x - 4.0 - midi_x).max(10.0), BTN_H),
        );
        d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
        y += ROW_STEP;

        // MIDI channel + device dropdown (share one row)
        d.set(C::ChLabel, Rect::new(pad, y, CH_LBL_W, BTN_H));
        let ch_x = pad + CH_LBL_W + 2.0;
        let ch_w = 44.0;
        d.set(C::ChDropdown, Rect::new(ch_x, y, ch_w, BTN_H));
        let dev_lbl_x = ch_x + ch_w + 6.0;
        d.set(C::DevLabel, Rect::new(dev_lbl_x, y, DEV_LBL_W, BTN_H));
        let dev_x = dev_lbl_x + DEV_LBL_W + 2.0;
        d.set(
            C::DevDropdown,
            Rect::new(dev_x, y, (right_edge - dev_x).max(10.0), BTN_H),
        );
    }

    let _ = y; // suppress unused
    d.set(
        C::Separator,
        Rect::new(0.0, y_offset + height - sep_h, w, sep_h),
    );
    d
}

/// Audio-layer controls: Mute / Solo are already placed by `compute_layer_row`;
/// this lays out the Gain row and the Send row beneath them, then the
/// separator. Audio cards never collapse their detail controls.
fn compute_audio_row(
    mut d: LayerRowData,
    y_offset: f32,
    height: f32,
    w: f32,
    pad: f32,
    btn_x: f32,
    y_buttons: f32,
) -> LayerRowData {
    use LayerControl as C;
    // Analysis-only toggle on the button row after Mute/Solo (M | S | A): silent to
    // master, still feeding the send. See `docs/LAYER_CONTROLS_DESIGN.md` §5.3.
    d.set(C::Analysis, Rect::new(btn_x, y_buttons, MS_BTN_W, BTN_H));
    let mut y = y_buttons + BTN_H + 2.0;
    let right_edge = w - pad - RIGHT_GUTTER;

    // Gain: dB slider spanning the row (label is drawn inside the slider widget).
    d.set(C::Gain, Rect::new(pad, y, (right_edge - pad).max(20.0), BTN_H));
    y += ROW_STEP;

    // Send: modulation-send dropdown spanning the row.
    d.set(C::Send, Rect::new(pad, y, (right_edge - pad).max(20.0), BTN_H));
    y += BTN_H + 2.0;

    let _ = y;
    d.set(
        C::Separator,
        Rect::new(0.0, y_offset + height - SEP_H, w, SEP_H),
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
}

impl Default for LayerRowIds {
    fn default() -> Self {
        Self {
            id: [None; N_CONTROLS],
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

    // Active layer (pushed from app layer each frame)
    active_layer: Option<LayerId>,
    cached_active_layer: Option<LayerId>,
    // Pending multi-select active flags (applied in update())
    pending_active_layers: Option<Vec<bool>>,

    // ── Live recording controls (in spacer area above layers) ──
    record_btn_id: Option<NodeId>,
    audio_device_label_id: Option<NodeId>,
    recording_active: bool,
    audio_device_name: String,

    /// Host for the declarative top chrome (background + recording controls).
    /// The per-layer scroll rows are still built imperatively below it.
    host: ChromeHost,

    // Screen-space origin of the layer controls panel
    panel_origin: Vec2,
    panel_width: f32,

    // Scroll container for clipping layer rows to the visible area.
    // Scroll offset is set externally via set_scroll_y() (synced with viewport).
    scroll: ScrollContainer,

    // Per-row gain slider drag state (audio layers). Reuses the shared
    // SliderDragState machine — same as the inspector/master sliders.
    gain_sliders: Vec<crate::slider::SliderDragState>,
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
            active_layer: None,
            cached_active_layer: None,
            pending_active_layers: None,
            record_btn_id: None,
            audio_device_label_id: None,
            recording_active: false,
            audio_device_name: "No audio input".into(),
            host: ChromeHost::new(),
            panel_origin: Vec2::ZERO,
            panel_width: 0.0,
            scroll: ScrollContainer::new(),
            gain_sliders: Vec::new(),
            active_gain_drag: -1,
            cache_first_node: usize::MAX,
            cache_node_count: 0,
        }
    }

    /// Set vertical scroll offset (synchronized with viewport).
    pub fn set_scroll_y(&mut self, y: f32) {
        self.scroll.set_scroll_offset(y);
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
        if let Some(id) = self.record_btn_id {
            tree.set_style(id, self.record_btn_style());
            let label = if active {
                "Stop Recording"
            } else {
                "Record Live"
            };
            tree.set_text(id, label);
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
                        View::button(if self.recording_active {
                            "Stop Recording"
                        } else {
                            "Record Live"
                        })
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
                tree.set_style(mute_id, mute_style(muted));
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
                tree.set_style(solo_id, solo_style(solo));
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
                tree.set_style(led_id, led_style(led));
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
        }
    }

    pub fn set_layer_name(&mut self, tree: &mut UITree, index: usize, name: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::Name) {
                tree.set_text(id, name);
            }
    }

    pub fn set_blend_mode_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && let Some(id) = row.id(LayerControl::Blend) {
                tree.set_text(id, text);
            }
    }

    pub fn set_midi_note_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
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
                return vec![
                    PanelAction::AudioGainSnapshot(i),
                    PanelAction::AudioGainChanged(i, val),
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
        if let Some(val) = self.gain_sliders[i].apply_drag(pos_x, tree, &gain_db_text) {
            return vec![PanelAction::AudioGainChanged(i, val)];
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
        if self.gain_sliders[i].end_drag() {
            return vec![PanelAction::AudioGainCommit(i)];
        }
        Vec::new()
    }

    /// Call when a drag begins on a layer header node.
    /// Returns PanelAction if the drag starts on a drag handle.
    pub fn handle_drag_begin(&mut self, tree: &mut UITree, node_id: NodeId) -> Vec<PanelAction> {
        // Try exact node_id match first (works when no rebuild happened since PointerDown).
        let mut matched_index: Option<usize> = None;
        for (i, row) in self.rows.iter().enumerate() {
            if row.id(LayerControl::DragHandle) == Some(node_id) {
                matched_index = Some(i);
                break;
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
            return vec![PanelAction::LayerDragStarted(i)];
        }
        self.drag_source = -1;
        Vec::new()
    }

    /// Call during an active drag with the current pointer position (screen space).
    pub fn handle_drag(&mut self, tree: &mut UITree, screen_pos: Vec2) -> Vec<PanelAction> {
        if self.drag_source < 0 {
            return Vec::new();
        }

        // Convert screen pos to panel-local Y
        let local_y = screen_pos.y - self.panel_origin.y;

        // Find target layer based on Y position
        let mut target = -1i32;
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.height <= 0.0 {
                continue;
            }
            if local_y >= layer.y_offset && local_y < layer.y_offset + layer.height {
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
            self.update_insert_indicator(tree);
            return vec![PanelAction::LayerDragMoved(
                self.drag_source as usize,
                target as usize,
            )];
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
            vec![PanelAction::LayerDragEnded(source, target)]
        } else {
            Vec::new()
        }
    }

    // ── Drag visual helpers ─────────────────────────────────────────

    fn update_insert_indicator(&self, tree: &mut UITree) {
        let Some(indicator_id) = self.insert_indicator_id else {
            return;
        };

        let y = if self.drag_target <= self.drag_source {
            self.layers
                .get(self.drag_target as usize)
                .map_or(0.0, |l| l.y_offset)
        } else {
            self.layers
                .get(self.drag_target as usize)
                .map_or(0.0, |l| l.y_offset + l.height)
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
                C::TypeBadge => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    // The icon char routes the renderer to the badge glyph; the
                    // node bounds size it (square, centred). Contrast colour so it
                    // reads on the layer-coloured background.
                    &layer.badge_icon().text(),
                    UIStyle {
                        text_color: text_clr,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    },
                ),
                C::Name => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    UIStyle {
                        bg_color: Color32::TRANSPARENT,
                        hover_bg_color: color::LAYER_CHEVRON_HOVER,
                        pressed_bg_color: color::LAYER_CHEVRON_PRESSED,
                        text_color: text_clr,
                        font_size: NAME_FONT,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                    &layer.name,
                ),
                C::DragHandle => {
                    // Hamburger icon drawn as 3 horizontal bars.
                    let handle = tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        UIStyle {
                            bg_color: color::HANDLE_BG,
                            hover_bg_color: color::BUTTON_HIGHLIGHTED,
                            pressed_bg_color: color::BUTTON_PRESSED,
                            corner_radius: LH_BTN_RADIUS,
                            ..UIStyle::default()
                        },
                        "",
                    );
                    let bar_w: f32 = 10.0;
                    let bar_h: f32 = 1.5;
                    let bar_x = r.x + (r.width - bar_w) * 0.5;
                    let bar_style = UIStyle {
                        bg_color: color::TEXT_ON_DARK,
                        ..UIStyle::default()
                    };
                    for i in 0..3 {
                        let bar_y = r.y + 4.5 + i as f32 * 4.0;
                        tree.add_panel(Some(handle), bar_x, bar_y, bar_w, bar_h, bar_style);
                    }
                    handle
                }
                C::GenType => {
                    let gen_text = layer.generator_type.as_deref().unwrap_or("Unknown");
                    tree.add_label(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        gen_text,
                        UIStyle {
                            text_color: text_clr,
                            font_size: SMALL_FONT,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    )
                }
                C::Mute => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    mute_style(layer.is_muted),
                    "M",
                ),
                C::Solo => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    solo_style(layer.is_solo),
                    "S",
                ),
                C::Analysis => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    analysis_style(layer.analysis_only),
                    "A",
                ),
                C::Led => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    led_style(layer.is_led),
                    "L",
                ),
                C::Blend => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    small_button_style(),
                    &layer.blend_mode,
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
                C::Folder => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    small_button_style(),
                    "Folder",
                ),
                C::PathLabel => {
                    let path_text =
                        folder_path_text(&layer.video_folder_path, layer.source_clip_count);
                    tree.add_label(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        &path_text,
                        UIStyle {
                            text_color: text_clr,
                            font_size: SMALL_FONT,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    )
                }
                C::NewClip => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    small_button_style(),
                    "+ new clip",
                ),
                C::MidiLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "MIDI",
                    UIStyle {
                        text_color: text_clr,
                        font_size: SMALL_FONT,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
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
                        field_style(),
                        &midi_text,
                    )
                }
                C::MidiMode => {
                    let mode_text = if layer.midi_all_notes { "All" } else { "Note" };
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        small_button_style(),
                        mode_text,
                    )
                }
                C::ChLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "CH",
                    UIStyle {
                        text_color: text_clr,
                        font_size: SMALL_FONT,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
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
                        small_button_style(),
                        &ch_text,
                    )
                }
                C::DevLabel => tree.add_label(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    "DEV",
                    UIStyle {
                        text_color: text_clr,
                        font_size: SMALL_FONT,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
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
                        small_button_style(),
                        dev_text,
                    )
                }
                C::AddGenClip => tree.add_button(
                    clip_parent,
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    small_button_style(),
                    "+ Clip",
                ),
                C::Gain => {
                    // Reusable dB slider; its `track` node is the drag target we
                    // address as the Gain control. The full node set is handed to
                    // a SliderDragState so drag updates reuse the shared machine.
                    let norm = gain_db_to_norm(layer.audio_gain_db);
                    let value_text = gain_db_text(layer.audio_gain_db);
                    let slider = crate::slider::BitmapSlider::build(
                        tree,
                        clip_parent,
                        r,
                        None,
                        norm,
                        &value_text,
                        &crate::slider::SliderColors::default_slider(),
                        SMALL_FONT,
                        0.0,
                    );
                    let track = slider.track;
                    let mut gs = crate::slider::SliderDragState::with_range(
                        GAIN_DB_MIN,
                        GAIN_DB_MAX,
                        false,
                    );
                    gs.set_ids(slider);
                    self.gain_sliders[index] = gs;
                    track
                }
                C::Send => {
                    let send_text = layer.audio_send_name.as_deref().unwrap_or("No send");
                    tree.add_button(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        small_button_style(),
                        send_text,
                    )
                }
            };
            ids.set(c, node);
        }

        self.rows[index] = ids;
    }

    fn handle_click(&self, node_id: NodeId, modifiers: crate::input::Modifiers) -> Vec<PanelAction> {
        // Record Live button
        if self.record_btn_id == Some(node_id) {
            return vec![PanelAction::ToggleLiveRecording];
        }
        // Audio input device dropdown
        if self.audio_device_label_id == Some(node_id) {
            return vec![PanelAction::SelectAudioInputDevice];
        }
        // Add Layer button
        if self.add_layer_btn == Some(node_id) {
            return vec![PanelAction::AddLayerClicked];
        }
        use LayerControl as C;
        for (i, row) in self.rows.iter().enumerate() {
            for &c in &C::ALL {
                if row.id(c) != Some(node_id) {
                    continue;
                }
                return match c {
                    C::Mute => vec![PanelAction::ToggleMute(i)],
                    C::Solo => vec![PanelAction::ToggleSolo(i)],
                    C::Analysis => vec![PanelAction::ToggleAnalysisOnly(i)],
                    C::Led => vec![PanelAction::ToggleLed(i)],
                    C::Chevron => vec![PanelAction::ChevronClicked(i)],
                    C::Blend => vec![PanelAction::BlendModeClicked(i)],
                    C::Folder => vec![PanelAction::FolderClicked(i)],
                    C::NewClip => vec![PanelAction::NewClipClicked(i)],
                    C::AddGenClip => vec![PanelAction::AddGenClipClicked(i)],
                    C::MidiInput => vec![PanelAction::MidiInputClicked(i)],
                    C::MidiMode => vec![PanelAction::MidiTriggerModeClicked(i)],
                    C::ChDropdown => vec![PanelAction::MidiChannelClicked(i)],
                    C::DevDropdown => vec![PanelAction::MidiDeviceClicked(i)],
                    C::Send => vec![PanelAction::AudioSendClicked(i)],
                    C::Name | C::Background | C::DragHandle => {
                        vec![PanelAction::LayerClicked(i, modifiers)]
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
            if row.id(LayerControl::Name) == Some(node_id) {
                return vec![PanelAction::LayerDoubleClicked(i)];
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
            row.for_each_id(|id| {
                intents.on(
                    id,
                    crate::intent::Gesture::RightClick,
                    PanelAction::LayerHeaderRightClicked(i),
                );
            });
        }
    }

    // ── Update-in-place (vertical scroll) ─────────────────────

    /// Try to update layer row Y positions in-place for vertical scroll.
    /// Computes the Y delta from the new scroll offset and shifts all node
    /// positions. Returns `true` if successful, `false` if full rebuild needed.
    pub fn try_update_vertical_scroll(&mut self, tree: &mut UITree, layout: &ScreenLayout) -> bool {
        // Guard: must have been built
        if self.rows.is_empty() || self.rows.len() != self.layers.len() {
            return false;
        }

        let lc = layout.layer_controls();
        let header_spacer = layout.track_header_height();
        let new_origin_y = lc.y + header_spacer - self.scroll.scroll_offset();
        let delta_y = new_origin_y - self.panel_origin.y;

        // Nothing changed
        if delta_y.abs() < 0.001 {
            return true;
        }

        // Update all node positions (and their children) by delta_y
        for row in &self.rows {
            row.for_each_id(|id| {
                tree.offset_node_and_children(id, delta_y);
            });
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

impl Panel for LayerHeaderPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.cache_first_node = tree.count();

        let lc = layout.layer_controls();
        // Offset layer rows down by the header stack (overview strip + ruler + waveform lanes)
        // so they align vertically with the track content area. `track_header_height()`
        // is the single source for this offset — the viewport reads the same value, so
        // the two cannot diverge.
        let header_spacer = layout.track_header_height();
        self.panel_origin = Vec2::new(lc.x, lc.y + header_spacer - self.scroll.scroll_offset());
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
        // Only resize cached state vectors if layer count changed —
        // preserve existing values to keep dirty-check logic correct.
        self.cached_mute.resize(layer_count, false);
        self.cached_solo.resize(layer_count, false);
        self.cached_led.resize(layer_count, false);
        self.cached_selected.resize(layer_count, false);
        self.cached_colors.resize(layer_count, Color32::TRANSPARENT);

        // Swap layers out to avoid borrow conflict in build_layer_row
        // (takes O(1), avoids cloning the entire Vec)
        let layers_snapshot = std::mem::take(&mut self.layers);

        for i in 0..layer_count {
            let layer = &layers_snapshot[i];
            if layer.height <= 0.0 {
                continue;
            }

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
                layer.y_offset,
                layer.height,
                lc.width,
                layer.is_collapsed,
                layer.is_group,
                layer.is_generator,
                layer.is_audio,
                is_child,
                is_last_child,
                layer.is_group && !layer.is_collapsed,
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

        // Tell ScrollContainer the total content height so set_scroll_offset
        // clamps correctly. The viewport drives the offset, but ScrollContainer
        // acts as a safety net — clamping to known content bounds.
        let content_h = self
            .layers
            .last()
            .map(|l| l.y_offset + l.height)
            .unwrap_or(0.0);
        self.scroll.set_content_height(content_h);

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

    fn update(&mut self, tree: &mut UITree) {
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

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
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

    fn first_node(&self) -> usize {
        self.cache_first_node
    }
    fn node_count(&self) -> usize {
        self.cache_node_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_video_layer(name: &str, y_offset: f32, height: f32) -> LayerInfo {
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
            y_offset,
            height,
            is_selected: false,
            color: Color32::new(100, 148, 210, 220),
        }
    }

    fn make_audio_layer(name: &str, y_offset: f32, height: f32) -> LayerInfo {
        LayerInfo {
            is_audio: true,
            audio_send_name: Some("Drums".into()),
            ..make_video_layer(name, y_offset, height)
        }
    }

    fn make_gen_layer(name: &str, y_offset: f32, height: f32) -> LayerInfo {
        LayerInfo {
            is_generator: true,
            generator_type: Some("Plasma".into()),
            ..make_video_layer(name, y_offset, height)
        }
    }

    fn make_group_layer(name: &str, y_offset: f32, height: f32) -> LayerInfo {
        LayerInfo {
            is_group: true,
            ..make_video_layer(name, y_offset, height)
        }
    }

    #[test]
    fn build_layer_header() {
        let mut tree = UITree::new();
        // Use tall screen so all 3 layers (y=0..420) fit in timeline body.
        let layout = ScreenLayout::new(1920.0, 2160.0);
        let mut panel = LayerHeaderPanel::new();

        panel.set_layers(vec![
            make_video_layer("Layer 1", 0.0, 140.0),
            make_video_layer("Layer 2", 140.0, 140.0),
            make_gen_layer("Gen Layer", 280.0, 140.0),
        ]);

        panel.build(&mut tree, &layout);

        assert_eq!(panel.layer_count(), 3);
        // All layers should have bg, name, mute, solo, blend_mode
        for i in 0..3 {
            assert!(panel.rows[i].id(LayerControl::Background).is_some(), "layer {} bg", i);
            assert!(panel.rows[i].id(LayerControl::Name).is_some(), "layer {} name", i);
            assert!(panel.rows[i].id(LayerControl::Mute).is_some(), "layer {} mute", i);
            assert!(panel.rows[i].id(LayerControl::Solo).is_some(), "layer {} solo", i);
            assert!(panel.rows[i].id(LayerControl::Blend).is_some(), "layer {} blend", i);
        }
        // Generator layer should have gen_type and add_gen_clip
        assert!(panel.rows[2].id(LayerControl::GenType).is_some());
        assert!(panel.rows[2].id(LayerControl::AddGenClip).is_some());
        // Video layers should have folder and new_clip
        assert!(panel.rows[0].id(LayerControl::Folder).is_some());
        assert!(panel.rows[0].id(LayerControl::NewClip).is_some());
        // Insert indicator
        assert!(panel.insert_indicator_id.is_some());
    }

    #[test]
    fn handle_click_mute_solo() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Mute).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::ToggleMute(0)));

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Solo).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::ToggleSolo(0)));
    }

    #[test]
    fn intent_resolves_right_click_anywhere_in_row_to_layer_menu() {
        use crate::intent::{Gesture, IntentRegistry};
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![
            make_video_layer("L0", 0.0, 140.0),
            make_video_layer("L1", 140.0, 140.0),
        ]);
        panel.build(&mut tree, &layout);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        // A right-click on any control in a row resolves to that layer's menu —
        // the name, mute, and solo controls all map to the same layer index.
        for ctrl in [LayerControl::Name, LayerControl::Mute, LayerControl::Solo] {
            let node = panel.rows[1].id(ctrl);
            let action = intents.resolve(&tree, node, Gesture::RightClick);
            assert!(
                matches!(action, Some(PanelAction::LayerHeaderRightClicked(1))),
                "node {node:?} ({ctrl:?}) should resolve to layer 1's menu, got {action:?}"
            );
        }
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(
            panel.rows[0].id(LayerControl::Chevron).unwrap(),
            crate::input::Modifiers::NONE,
        );
        assert!(matches!(a[0], PanelAction::ChevronClicked(0)));
    }

    #[test]
    fn set_mute_state_updates() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        tree.clear_dirty();
        panel.set_mute_state(&mut tree, 0, true);
        assert!(tree.has_dirty());

        // Calling again with same state should not dirty
        tree.clear_dirty();
        panel.set_mute_state(&mut tree, 0, true);
        assert!(!tree.has_dirty());
    }

    #[test]
    fn build_with_group() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();

        let mut child = make_video_layer("Child", 70.0, 140.0);
        child.parent_layer_id = Some("Group".into());

        panel.set_layers(vec![make_group_layer("Group", 0.0, 70.0), child]);

        panel.build(&mut tree, &layout);

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

        let mut layer = make_video_layer("Collapsed", 0.0, 48.0);
        layer.is_collapsed = true;

        panel.set_layers(vec![layer]);
        panel.build(&mut tree, &layout);

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
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let event = UIEvent::DoubleClick {
            node_id: panel.rows[0].id(LayerControl::Name).unwrap(),
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        };
        let a = panel.handle_event(&event, &tree);
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::LayerDoubleClicked(0)));
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
    ) -> LayerRowData {
        use LayerControl as C;
        let mut d = LayerRowData::default();
        let w = if panel_width > 0.0 {
            panel_width
        } else {
            color::LAYER_CONTROLS_WIDTH
        };
        d.set(C::Background, Rect::new(0.0, y_offset, w, height));
        let left_indent = if is_child { CHILD_INDENT } else { 0.0 };
        let pad = PAD + left_indent;
        let mut y = y_offset + PAD;
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
                Rect::new(0.0, y_offset + height - BORDER_H, w, BORDER_H),
            );
        }
        let chevron_w = CHEVRON_W;
        d.set(C::Chevron, Rect::new(pad, y, CHEVRON_W, BTN_H));
        let badge_x = pad + chevron_w + if chevron_w > 0.0 { TOP_GAP } else { 0.0 };
        let badge_y = y + (BTN_H - BADGE_SIZE) * 0.5;
        d.set(C::TypeBadge, Rect::new(badge_x, badge_y, BADGE_SIZE, BADGE_SIZE));
        let name_left = badge_x + BADGE_SIZE + TOP_GAP;
        let handle_x = w - pad - HANDLE_W - 8.0;
        let name_w = (handle_x - name_left - TOP_GAP).max(20.0);
        d.set(C::Name, Rect::new(name_left, y, name_w, NAME_H));
        d.set(C::DragHandle, Rect::new(handle_x, y, HANDLE_W, BTN_H));
        y += ROW_STEP;
        if is_generator && !is_collapsed {
            let gen_w = w - name_left - pad;
            d.set(C::GenType, Rect::new(name_left, y, gen_w, GEN_TYPE_H));
            y += GEN_TYPE_H;
        }
        let mut btn_x = pad;
        d.set(C::Mute, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 2.0;
        d.set(C::Solo, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 2.0;
        if is_audio {
            d.set(C::Analysis, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
            let mut ay = y + BTN_H + 2.0;
            let right_edge = w - pad - RIGHT_GUTTER;
            d.set(C::Gain, Rect::new(pad, ay, (right_edge - pad).max(20.0), BTN_H));
            ay += ROW_STEP;
            d.set(C::Send, Rect::new(pad, ay, (right_edge - pad).max(20.0), BTN_H));
            d.set(
                C::Separator,
                Rect::new(0.0, y_offset + height - SEP_H, w, SEP_H),
            );
            return d;
        }
        d.set(C::Led, Rect::new(btn_x, y, MS_BTN_W, BTN_H));
        btn_x += MS_BTN_W + 4.0;
        let dd_w = (w - btn_x - pad - RIGHT_GUTTER).max(20.0);
        d.set(C::Blend, Rect::new(btn_x, y, dd_w, BTN_H));
        y += BTN_H + 2.0;
        let sep_h = if is_group {
            color::GROUP_SEPARATOR_HEIGHT
        } else {
            SEP_H
        };
        if is_collapsed && !is_group {
            d.set(
                C::Separator,
                Rect::new(0.0, y_offset + height - sep_h, w, sep_h),
            );
            return d;
        }
        d.set(C::Info, Rect::new(pad, y, w - pad * 2.0, INFO_H));
        y += 16.0;
        if is_group {
            y += 2.0;
        } else if is_generator {
            d.set(C::AddGenClip, Rect::new(pad, y, ADD_GEN_W, BTN_H));
            y += BTN_H + 2.0;
            d.set(C::MidiLabel, Rect::new(pad, y, MIDI_LBL_W, BTN_H));
            let gen_midi_x = pad + MIDI_LBL_W + 2.0;
            let right_edge = w - pad - RIGHT_GUTTER;
            let mode_x = right_edge - MODE_TOGGLE_W;
            d.set(
                C::MidiInput,
                Rect::new(gen_midi_x, y, (mode_x - 4.0 - gen_midi_x).max(10.0), BTN_H),
            );
            d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
            y += ROW_STEP;
            d.set(C::ChLabel, Rect::new(pad, y, CH_LBL_W, BTN_H));
            let gen_ch_x = pad + CH_LBL_W + 2.0;
            let ch_w = 44.0;
            d.set(C::ChDropdown, Rect::new(gen_ch_x, y, ch_w, BTN_H));
            let dev_lbl_x = gen_ch_x + ch_w + 6.0;
            d.set(C::DevLabel, Rect::new(dev_lbl_x, y, DEV_LBL_W, BTN_H));
            let dev_x = dev_lbl_x + DEV_LBL_W + 2.0;
            d.set(
                C::DevDropdown,
                Rect::new(dev_x, y, (right_edge - dev_x).max(10.0), BTN_H),
            );
        } else {
            d.set(C::Folder, Rect::new(pad, y, FOLDER_W, BTN_H));
            let path_left = pad + FOLDER_W + 4.0;
            let new_clip_x = w - pad - NEW_CLIP_W;
            let path_w = (new_clip_x - path_left - 4.0).max(10.0);
            d.set(C::PathLabel, Rect::new(path_left, y, path_w, BTN_H));
            d.set(C::NewClip, Rect::new(new_clip_x, y, NEW_CLIP_W, BTN_H));
            y += ROW_STEP;
            d.set(C::MidiLabel, Rect::new(pad, y, MIDI_LBL_W, BTN_H));
            let midi_x = pad + MIDI_LBL_W + 2.0;
            let right_edge = w - pad - RIGHT_GUTTER;
            let mode_x = right_edge - MODE_TOGGLE_W;
            d.set(
                C::MidiInput,
                Rect::new(midi_x, y, (mode_x - 4.0 - midi_x).max(10.0), BTN_H),
            );
            d.set(C::MidiMode, Rect::new(mode_x, y, MODE_TOGGLE_W, BTN_H));
            y += ROW_STEP;
            d.set(C::ChLabel, Rect::new(pad, y, CH_LBL_W, BTN_H));
            let ch_x = pad + CH_LBL_W + 2.0;
            let ch_w = 44.0;
            d.set(C::ChDropdown, Rect::new(ch_x, y, ch_w, BTN_H));
            let dev_lbl_x = ch_x + ch_w + 6.0;
            d.set(C::DevLabel, Rect::new(dev_lbl_x, y, DEV_LBL_W, BTN_H));
            let dev_x = dev_lbl_x + DEV_LBL_W + 2.0;
            d.set(
                C::DevDropdown,
                Rect::new(dev_x, y, (right_edge - dev_x).max(10.0), BTN_H),
            );
        }
        let _ = y;
        d.set(
            C::Separator,
            Rect::new(0.0, y_offset + height - sep_h, w, sep_h),
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
        ];
        for (coll, grp, genr, aud, child, last, gexp, label) in cases {
            let live = compute_layer_row(0.0, 140.0, 300.0, coll, grp, genr, aud, child, last, gexp);
            let oracle = oracle_row(0.0, 140.0, 300.0, coll, grp, genr, aud, child, last, gexp);
            assert_row_eq(&live, &oracle, label);
        }
    }

    #[test]
    fn audio_card_controls() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_audio_layer("Drums In", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

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
        assert!(matches!(a.as_slice(), [PanelAction::AudioSendClicked(0)]));
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
}
