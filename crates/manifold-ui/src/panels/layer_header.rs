use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use manifold_core::LayerId;
use super::{Panel, PanelAction};

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
const MIDI_LBL_W: f32 = color::LAYER_CTRL_MIDI_LABEL_WIDTH;
const CH_LBL_W: f32 = color::LAYER_CTRL_CHANNEL_LABEL_WIDTH;
const ACCENT_W: f32 = color::GROUP_ACCENT_BAR_WIDTH;
const CHILD_INDENT: f32 = color::GROUP_CHILD_INDENT_PX;
const BORDER_H: f32 = color::GROUP_BOTTOM_BORDER_HEIGHT;

// ── Panel-specific colors ───────────────────────────────────────────

const BG_COLOR: Color32 = color::CONTROL_BG;
const BG_SELECTED: Color32 = color::SELECTED_LAYER_CONTROL;
const SEP_COLOR: Color32 = color::SEPARATOR_COLOR;
const ACCENT_COLOR: Color32 = color::DEFAULT_GROUP_ACCENT;
const BORDER_CLR: Color32 = color::GROUP_BOTTOM_BORDER;
const GEN_TYPE_CLR: Color32 = color::CLIP_GEN_NORMAL;

const DRAG_SOURCE_DIM: Color32 = color::LAYER_DRAG_SOURCE_DIM;
const INSERT_LINE_CLR: Color32 = color::LAYER_INSERT_LINE;
const INSERT_LINE_H: f32 = 2.0;

const NAME_FONT: u16 = color::LAYER_CTRL_NAME_FONT_SIZE;
const SMALL_FONT: u16 = color::LAYER_CTRL_SMALL_FONT_SIZE;
const HANDLE_FONT: u16 = color::LAYER_CTRL_HANDLE_FONT_SIZE;
const BTN_FONT: u16 = 10;
const LH_BTN_RADIUS: f32 = 2.0;

// ── Style helpers ───────────────────────────────────────────────────

fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_add(amount),
        c.g.saturating_add(amount),
        c.b.saturating_add(amount),
        c.a,
    )
}

fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_sub(amount),
        c.g.saturating_sub(amount),
        c.b.saturating_sub(amount),
        c.a,
    )
}

fn mute_style(muted: bool) -> UIStyle {
    let bg = if muted { color::MUTED_COLOR } else { color::BUTTON_DIM };
    UIStyle {
        bg_color: bg,
        hover_bg_color: if muted { lighten(color::MUTED_COLOR, 30) } else { color::BUTTON_HIGHLIGHTED },
        pressed_bg_color: if muted { darken(color::MUTED_COLOR, 20) } else { color::BUTTON_PRESSED },
        text_color: color::TEXT_WHITE_C32,
        font_size: BTN_FONT,
        corner_radius: LH_BTN_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

fn solo_style(solo: bool) -> UIStyle {
    let bg = if solo { color::SOLO_COLOR } else { color::BUTTON_DIM };
    UIStyle {
        bg_color: bg,
        hover_bg_color: if solo { lighten(color::SOLO_COLOR, 30) } else { color::BUTTON_HIGHLIGHTED },
        pressed_bg_color: if solo { darken(color::SOLO_COLOR, 20) } else { color::BUTTON_PRESSED },
        text_color: color::TEXT_WHITE_C32,
        font_size: BTN_FONT,
        corner_radius: LH_BTN_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
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

/// Blend `tint` into `base` at the given fraction (0.0 = all base, 1.0 = all tint).
fn tint_bg(base: Color32, tint: Color32, amount: f32) -> Color32 {
    let blend = |b: u8, t: u8| -> u8 {
        (b as f32 * (1.0 - amount) + t as f32 * amount) as u8
    };
    Color32::new(blend(base.r, tint.r), blend(base.g, tint.g), blend(base.b, tint.b), base.a)
}

fn bg_style(selected: bool, layer_color: Color32) -> UIStyle {
    let base = if selected { BG_SELECTED } else { BG_COLOR };
    let amount = if selected { 0.35 } else { 0.20 };
    let bg = tint_bg(base, layer_color, amount);
    let hover = lighten(bg, if selected { 10 } else { 12 });
    let pressed = darken(bg, 8);
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
    pub is_muted: bool,
    pub is_solo: bool,
    pub parent_layer_id: Option<String>,
    pub blend_mode: String,
    pub generator_type: Option<String>,
    pub clip_count: usize,
    pub video_folder_path: Option<String>,
    pub source_clip_count: usize,
    pub midi_note: i32,
    pub midi_channel: i32,
    /// Y offset within the layer controls panel (panel-local).
    pub y_offset: f32,
    /// Height of this layer row.
    pub height: f32,
    pub is_selected: bool,
    /// Layer color (auto-assigned or user-set).
    pub color: Color32,
}

// ── LayerRowData ────────────────────────────────────────────────────

#[derive(Default)]
struct LayerRowData {
    background: Rect,
    chevron: Rect,
    name: Rect,
    drag_handle: Rect,
    mute: Rect,
    solo: Rect,
    blend_mode: Rect,
    separator: Rect,
    accent_bar: Rect,
    connector: Rect,
    bottom_border: Rect,
    info: Rect,
    folder: Rect,
    path_label: Rect,
    new_clip: Rect,
    gen_type: Rect,
    add_gen_clip: Rect,
    midi_label: Rect,
    midi_input: Rect,
    ch_label: Rect,
    ch_dropdown: Rect,
    has_chevron: bool,
    has_accent_bar: bool,
    has_connector: bool,
    has_bottom_border: bool,
    has_expanded_controls: bool,
    has_gen_type: bool,
    has_video_controls: bool,
    has_generator_controls: bool,
    #[allow(dead_code)]
    has_group_info: bool,
}

/// Compute element rects for one layer row in panel-local coordinates.
#[allow(clippy::too_many_arguments)]
fn compute_layer_row(
    y_offset: f32, height: f32, panel_width: f32,
    is_collapsed: bool, is_group: bool, is_generator: bool,
    is_child: bool, is_last_child: bool, is_group_expanded: bool,
) -> LayerRowData {
    let mut d = LayerRowData::default();
    let w = if panel_width > 0.0 { panel_width } else { color::LAYER_CONTROLS_WIDTH };

    d.background = Rect::new(0.0, y_offset, w, height);

    let left_indent = if is_child { CHILD_INDENT } else { 0.0 };
    let pad = PAD + left_indent;
    let mut y = y_offset + PAD;

    // ── Group visuals ──
    d.has_accent_bar = is_child;
    if is_child {
        d.accent_bar = Rect::new(0.0, y_offset, ACCENT_W, height);
    }

    d.has_connector = is_group && is_group_expanded;
    if d.has_connector {
        d.connector = Rect::new(0.0, y_offset + height * 0.5, ACCENT_W, height * 0.5);
    }

    d.has_bottom_border = is_child && is_last_child;
    if d.has_bottom_border {
        d.bottom_border = Rect::new(0.0, y_offset + height - BORDER_H, w, BORDER_H);
    }

    // ── Top row: Chevron | Name | DragHandle ──
    d.has_chevron = is_group || !is_child;
    let chevron_w = if d.has_chevron { CHEVRON_W } else { 0.0 };
    if d.has_chevron {
        d.chevron = Rect::new(pad, y, CHEVRON_W, BTN_H);
    }

    let name_left = pad + chevron_w + if chevron_w > 0.0 { TOP_GAP } else { 0.0 };
    let handle_x = w - pad - HANDLE_W - 8.0;
    let name_w = (handle_x - name_left - TOP_GAP).max(20.0);
    d.name = Rect::new(name_left, y, name_w, NAME_H);
    d.drag_handle = Rect::new(handle_x, y, HANDLE_W, BTN_H);

    y += ROW_STEP;

    // ── Generator type row ──
    d.has_gen_type = is_generator;
    if is_generator {
        let gen_w = w - name_left - pad;
        d.gen_type = Rect::new(name_left, y, gen_w, GEN_TYPE_H);
        y += GEN_TYPE_H;
    }

    // ── Button row: M | S | BlendMode ──
    let mut btn_x = pad;
    d.mute = Rect::new(btn_x, y, MS_BTN_W, BTN_H);
    btn_x += MS_BTN_W + 2.0;
    d.solo = Rect::new(btn_x, y, MS_BTN_W, BTN_H);
    btn_x += MS_BTN_W + 4.0;

    let dd_w = (w - btn_x - pad - RIGHT_GUTTER).max(20.0);
    d.blend_mode = Rect::new(btn_x, y, dd_w, BTN_H);

    y += BTN_H + 2.0;

    // ── Collapsed non-group: skip detail controls ──
    d.has_expanded_controls = !is_collapsed || is_group;
    if !d.has_expanded_controls {
        d.separator = Rect::new(0.0, y_offset + height - SEP_H, w, SEP_H);
        return d;
    }

    // ── Info label ──
    d.info = Rect::new(pad, y, w - pad * 2.0, INFO_H);
    y += 16.0;

    if is_group {
        d.has_group_info = true;
        y += 2.0;
    } else if is_generator {
        d.has_generator_controls = true;
        d.add_gen_clip = Rect::new(pad, y, ADD_GEN_W, BTN_H);
        y += BTN_H + 2.0;

        // MIDI note
        d.midi_label = Rect::new(pad, y, MIDI_LBL_W, BTN_H);
        let gen_midi_x = pad + MIDI_LBL_W + 2.0;
        d.midi_input = Rect::new(gen_midi_x, y, w - gen_midi_x - pad - RIGHT_GUTTER, BTN_H);
        y += ROW_STEP;

        // MIDI channel
        d.ch_label = Rect::new(pad, y, CH_LBL_W, BTN_H);
        let gen_ch_x = pad + CH_LBL_W + 2.0;
        d.ch_dropdown = Rect::new(gen_ch_x, y, w - gen_ch_x - pad - RIGHT_GUTTER, BTN_H);
    } else {
        d.has_video_controls = true;

        // Folder | PathLabel | +new clip
        d.folder = Rect::new(pad, y, FOLDER_W, BTN_H);
        let path_left = pad + FOLDER_W + 4.0;
        let new_clip_x = w - pad - NEW_CLIP_W;
        let path_w = (new_clip_x - path_left - 4.0).max(10.0);
        d.path_label = Rect::new(path_left, y, path_w, BTN_H);
        d.new_clip = Rect::new(new_clip_x, y, NEW_CLIP_W, BTN_H);
        y += ROW_STEP;

        // MIDI note
        d.midi_label = Rect::new(pad, y, MIDI_LBL_W, BTN_H);
        let midi_x = pad + MIDI_LBL_W + 2.0;
        d.midi_input = Rect::new(midi_x, y, w - midi_x - pad - RIGHT_GUTTER, BTN_H);
        y += ROW_STEP;

        // MIDI channel
        d.ch_label = Rect::new(pad, y, CH_LBL_W, BTN_H);
        let ch_x = pad + CH_LBL_W + 2.0;
        d.ch_dropdown = Rect::new(ch_x, y, w - ch_x - pad - RIGHT_GUTTER, BTN_H);
    }

    let _ = y; // suppress unused
    d.separator = Rect::new(0.0, y_offset + height - SEP_H, w, SEP_H);
    d
}

// ── LayerRowIds ─────────────────────────────────────────────────────

#[derive(Clone)]
struct LayerRowIds {
    bg: i32,
    chevron: i32,
    name: i32,
    drag_handle: i32,
    mute: i32,
    solo: i32,
    blend_mode: i32,
    separator: i32,
    info: i32,
    accent_bar: i32,
    connector: i32,
    bottom_border: i32,
    folder: i32,
    path_label: i32,
    new_clip: i32,
    gen_type: i32,
    add_gen_clip: i32,
    midi_label: i32,
    midi_input: i32,
    ch_label: i32,
    ch_dropdown: i32,
}

impl Default for LayerRowIds {
    fn default() -> Self {
        Self {
            bg: -1, chevron: -1, name: -1, drag_handle: -1,
            mute: -1, solo: -1, blend_mode: -1, separator: -1,
            info: -1, accent_bar: -1, connector: -1, bottom_border: -1,
            folder: -1, path_label: -1, new_clip: -1, gen_type: -1,
            add_gen_clip: -1, midi_label: -1, midi_input: -1,
            ch_label: -1, ch_dropdown: -1,
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn midi_note_to_name(note: i32) -> String {
    if note < 0 { return "None".into(); }
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = (note / 12) - 1;
    let name = NAMES[(note % 12) as usize];
    format!("{}{}", name, octave)
}

fn folder_path_text(path: &Option<String>, source_count: usize) -> String {
    match path {
        Some(p) if !p.is_empty() => {
            let folder = p.trim_end_matches(['/', '\\'])
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
        let child_count = all_layers.iter()
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

// ── LayerHeaderPanel ────────────────────────────────────────────────

pub struct LayerHeaderPanel {
    layers: Vec<LayerInfo>,
    rows: Vec<LayerRowIds>,

    // Drag-reorder state
    drag_source: i32,
    drag_target: i32,
    insert_indicator_id: i32,
    add_layer_btn: i32,
    // Saved during PointerDown on drag handle so DragBegin can find the
    // correct layer even after a tree rebuild has invalidated node IDs.
    pending_drag_layer: i32,

    // Cached state for dirty-checking
    cached_mute: Vec<bool>,
    cached_solo: Vec<bool>,
    cached_selected: Vec<bool>,
    cached_colors: Vec<Color32>,

    // Active layer (pushed from app layer each frame)
    active_layer: Option<LayerId>,
    cached_active_layer: Option<LayerId>,
    // Pending multi-select active flags (applied in update())
    pending_active_layers: Option<Vec<bool>>,

    // Screen-space origin of the layer controls panel
    panel_origin: Vec2,
    panel_width: f32,

    // Vertical scroll offset — synchronized with viewport scroll_y_px
    scroll_y_px: f32,

    // Scroll container for clipping layer rows to the visible area
    scroll_clip_id: i32,
}

impl LayerHeaderPanel {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            rows: Vec::new(),
            drag_source: -1,
            drag_target: -1,
            insert_indicator_id: -1,
            add_layer_btn: -1,
            pending_drag_layer: -1,
            cached_mute: Vec::new(),
            cached_solo: Vec::new(),
            cached_selected: Vec::new(),
            cached_colors: Vec::new(),
            active_layer: None,
            cached_active_layer: None,
            pending_active_layers: None,
            panel_origin: Vec2::ZERO,
            panel_width: 0.0,
            scroll_y_px: 0.0,
            scroll_clip_id: -1,
        }
    }

    /// Set vertical scroll offset (synchronized with viewport).
    pub fn set_scroll_y(&mut self, y: f32) {
        self.scroll_y_px = y;
    }

    /// Set the layer data snapshot. Must be called before build().
    pub fn set_layers(&mut self, layers: Vec<LayerInfo>) {
        self.layers = layers;
    }

    /// Number of layers in the current build.
    pub fn layer_count(&self) -> usize {
        self.rows.len()
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

    // ── Accessors ───────────────────────────────────────────────────

    pub fn blend_mode_node_id(&self, index: usize) -> i32 {
        self.rows.get(index).map_or(-1, |r| r.blend_mode)
    }

    pub fn midi_channel_node_id(&self, index: usize) -> i32 {
        self.rows.get(index).map_or(-1, |r| r.ch_dropdown)
    }

    pub fn midi_input_node_id(&self, index: usize) -> i32 {
        self.rows.get(index).map_or(-1, |r| r.midi_input)
    }

    pub fn name_node_id(&self, index: usize) -> i32 {
        self.rows.get(index).map_or(-1, |r| r.name)
    }

    pub fn get_node_bounds(&self, tree: &UITree, node_id: i32) -> Rect {
        if node_id < 0 { return Rect::ZERO; }
        tree.get_bounds(node_id as u32)
    }

    // ── Push-based setters ──────────────────────────────────────────

    pub fn set_mute_state(&mut self, tree: &mut UITree, index: usize, muted: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_mute.get_mut(index) {
                if *cached == muted { return; }
                *cached = muted;
            }
            if row.mute >= 0 {
                tree.set_style(row.mute as u32, mute_style(muted));
            }
        }
    }

    pub fn set_solo_state(&mut self, tree: &mut UITree, index: usize, solo: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_solo.get_mut(index) {
                if *cached == solo { return; }
                *cached = solo;
            }
            if row.solo >= 0 {
                tree.set_style(row.solo as u32, solo_style(solo));
            }
        }
    }

    pub fn set_selection(&mut self, tree: &mut UITree, index: usize, selected: bool) {
        if let Some(row) = self.rows.get(index) {
            if let Some(cached) = self.cached_selected.get_mut(index) {
                if *cached == selected { return; }
                *cached = selected;
            }
            if row.bg >= 0 {
                let layer_color = self.cached_colors.get(index)
                    .copied().unwrap_or(Color32::TRANSPARENT);
                tree.set_style(row.bg as u32, bg_style(selected, layer_color));
            }
        }
    }

    pub fn set_layer_name(&mut self, tree: &mut UITree, index: usize, name: &str) {
        if let Some(row) = self.rows.get(index)
            && row.name >= 0 { tree.set_text(row.name as u32, name); }
    }

    pub fn set_blend_mode_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && row.blend_mode >= 0 { tree.set_text(row.blend_mode as u32, text); }
    }

    pub fn set_midi_note_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && row.midi_input >= 0 { tree.set_text(row.midi_input as u32, text); }
    }

    pub fn set_midi_channel_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && row.ch_dropdown >= 0 { tree.set_text(row.ch_dropdown as u32, text); }
    }

    pub fn set_info_text(&mut self, tree: &mut UITree, index: usize, text: &str) {
        if let Some(row) = self.rows.get(index)
            && row.info >= 0 { tree.set_text(row.info as u32, text); }
    }

    // ── Drag-reorder (separate from Panel trait — needs &mut UITree) ──

    /// Returns true if a layer drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.drag_source >= 0
    }

    /// Call when a drag begins on a layer header node.
    /// Returns PanelAction if the drag starts on a drag handle.
    pub fn handle_drag_begin(&mut self, tree: &mut UITree, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        // Try exact node_id match first (works when no rebuild happened since PointerDown).
        let mut matched_index: Option<usize> = None;
        for (i, row) in self.rows.iter().enumerate() {
            if id == row.drag_handle {
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
                && row.bg >= 0
            {
                tree.set_style(row.bg as u32, UIStyle {
                    bg_color: DRAG_SOURCE_DIM,
                    ..UIStyle::default()
                });
            }
            return vec![PanelAction::LayerDragStarted(i)];
        }
        self.drag_source = -1;
        Vec::new()
    }

    /// Call during an active drag with the current pointer position (screen space).
    pub fn handle_drag(&mut self, tree: &mut UITree, screen_pos: Vec2) -> Vec<PanelAction> {
        if self.drag_source < 0 { return Vec::new(); }

        // Convert screen pos to panel-local Y
        let local_y = screen_pos.y - self.panel_origin.y;

        // Find target layer based on Y position
        let mut target = -1i32;
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.height <= 0.0 { continue; }
            if local_y >= layer.y_offset && local_y < layer.y_offset + layer.height {
                target = i as i32;
                break;
            }
        }

        if target < 0 {
            target = if local_y < 0.0 { 0 } else { (self.layers.len() as i32 - 1).max(0) };
        }

        if target != self.drag_target {
            self.drag_target = target;
            self.update_insert_indicator(tree);
            return vec![PanelAction::LayerDragMoved(self.drag_source as usize, target as usize)];
        }
        Vec::new()
    }

    /// Call when a drag ends.
    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.drag_source < 0 { return Vec::new(); }

        let source = self.drag_source as usize;
        let target = self.drag_target as usize;
        self.drag_source = -1;
        self.drag_target = -1;

        self.hide_insert_indicator(tree);

        // Restore source layer appearance
        if let Some(row) = self.rows.get(source)
            && row.bg >= 0 {
                let selected = self.cached_selected.get(source).copied().unwrap_or(false);
                let layer_color = self.cached_colors.get(source)
                    .copied().unwrap_or(Color32::TRANSPARENT);
                tree.set_style(row.bg as u32, bg_style(selected, layer_color));
            }

        if source != target {
            vec![PanelAction::LayerDragEnded(source, target)]
        } else {
            Vec::new()
        }
    }

    // ── Drag visual helpers ─────────────────────────────────────────

    fn update_insert_indicator(&self, tree: &mut UITree) {
        if self.insert_indicator_id < 0 { return; }

        let y = if self.drag_target <= self.drag_source {
            self.layers.get(self.drag_target as usize)
                .map_or(0.0, |l| l.y_offset)
        } else {
            self.layers.get(self.drag_target as usize)
                .map_or(0.0, |l| l.y_offset + l.height)
        };

        let screen_y = self.panel_origin.y + y - INSERT_LINE_H * 0.5;
        tree.set_bounds(self.insert_indicator_id as u32,
            Rect::new(self.panel_origin.x, screen_y, self.panel_width, INSERT_LINE_H));
        tree.set_style(self.insert_indicator_id as u32,
            UIStyle { bg_color: INSERT_LINE_CLR, ..UIStyle::default() });
    }

    fn hide_insert_indicator(&self, tree: &mut UITree) {
        if self.insert_indicator_id < 0 { return; }
        tree.set_bounds(self.insert_indicator_id as u32,
            Rect::new(0.0, -10.0, 0.0, 0.0));
        tree.set_style(self.insert_indicator_id as u32,
            UIStyle { bg_color: Color32::TRANSPARENT, ..UIStyle::default() });
    }

    // ── Build helpers ───────────────────────────────────────────────

    fn build_layer_row(
        &mut self,
        tree: &mut UITree,
        index: usize,
        layer: &LayerInfo,
        row: LayerRowData,
        origin: Vec2,
        clip_parent: i32,
    ) {
        let ids = &mut self.rows[index];
        let s = |r: Rect| screen(r, origin);

        // Background (full row interactive area, tinted with layer color)
        let bg_r = s(row.background);
        ids.bg = tree.add_button(
            clip_parent, bg_r.x, bg_r.y, bg_r.width, bg_r.height,
            bg_style(layer.is_selected, layer.color), "",
        ) as i32;

        // Group accent bar
        if row.has_accent_bar {
            let r = s(row.accent_bar);
            ids.accent_bar = tree.add_panel(
                clip_parent, r.x, r.y, r.width, r.height,
                UIStyle { bg_color: ACCENT_COLOR, ..UIStyle::default() },
            ) as i32;
        }

        // Group connector
        if row.has_connector {
            let r = s(row.connector);
            ids.connector = tree.add_panel(
                clip_parent, r.x, r.y, r.width, r.height,
                UIStyle { bg_color: ACCENT_COLOR, ..UIStyle::default() },
            ) as i32;
        }

        // Group bottom border
        if row.has_bottom_border {
            let r = s(row.bottom_border);
            ids.bottom_border = tree.add_panel(
                clip_parent, r.x, r.y, r.width, r.height,
                UIStyle { bg_color: BORDER_CLR, ..UIStyle::default() },
            ) as i32;
        }

        // Chevron
        if row.has_chevron {
            let chev = if layer.is_collapsed { "\u{25B6}" } else { "\u{25BC}" };
            let r = s(row.chevron);
            ids.chevron = tree.add_button(
                clip_parent, r.x, r.y, r.width, r.height,
                UIStyle {
                    bg_color: Color32::TRANSPARENT,
                    hover_bg_color: color::BUTTON_HIGHLIGHTED,
                    pressed_bg_color: color::BUTTON_PRESSED,
                    text_color: color::CHEVRON_COLOR,
                    font_size: SMALL_FONT,
                    corner_radius: color::SMALL_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                chev,
            ) as i32;
        }

        // Layer name
        let nr = s(row.name);
        ids.name = tree.add_button(
            clip_parent, nr.x, nr.y, nr.width, nr.height,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::LAYER_CHEVRON_HOVER,
                pressed_bg_color: color::LAYER_CHEVRON_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: NAME_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
            &layer.name,
        ) as i32;

        // Drag handle
        let dr = s(row.drag_handle);
        ids.drag_handle = tree.add_button(
            clip_parent, dr.x, dr.y, dr.width, dr.height,
            UIStyle {
                bg_color: color::HANDLE_BG,
                hover_bg_color: color::BUTTON_HIGHLIGHTED,
                pressed_bg_color: color::BUTTON_PRESSED,
                text_color: color::TEXT_DIMMED_C32,
                font_size: HANDLE_FONT,
                corner_radius: LH_BTN_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "\u{2261}",
        ) as i32;

        // Generator type label
        if row.has_gen_type {
            let gen_text = layer.generator_type.as_deref().unwrap_or("Unknown");
            let r = s(row.gen_type);
            ids.gen_type = tree.add_label(
                clip_parent, r.x, r.y, r.width, r.height,
                gen_text,
                UIStyle {
                    text_color: GEN_TYPE_CLR,
                    font_size: SMALL_FONT,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;
        }

        // Mute button
        let mr = s(row.mute);
        ids.mute = tree.add_button(
            clip_parent, mr.x, mr.y, mr.width, mr.height,
            mute_style(layer.is_muted), "M",
        ) as i32;

        // Solo button
        let sr = s(row.solo);
        ids.solo = tree.add_button(
            clip_parent, sr.x, sr.y, sr.width, sr.height,
            solo_style(layer.is_solo), "S",
        ) as i32;

        // Blend mode
        let br = s(row.blend_mode);
        ids.blend_mode = tree.add_button(
            clip_parent, br.x, br.y, br.width, br.height,
            small_button_style(), &layer.blend_mode,
        ) as i32;

        // Separator
        let sepr = s(row.separator);
        ids.separator = tree.add_panel(
            clip_parent, sepr.x, sepr.y, sepr.width, sepr.height,
            UIStyle { bg_color: SEP_COLOR, ..UIStyle::default() },
        ) as i32;

        // ── Expanded controls ──
        if !row.has_expanded_controls {
            return;
        }

        // Info label
        let info = info_text(layer, &self.layers);
        let ir = s(row.info);
        ids.info = tree.add_label(
            clip_parent, ir.x, ir.y, ir.width, ir.height,
            &info,
            UIStyle {
                text_color: color::TEXT_SUBTLE,
                font_size: SMALL_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        if row.has_video_controls {
            // Folder button
            let fr = s(row.folder);
            ids.folder = tree.add_button(
                clip_parent, fr.x, fr.y, fr.width, fr.height,
                small_button_style(), "Folder",
            ) as i32;

            // Path label
            let path_text = folder_path_text(&layer.video_folder_path, layer.source_clip_count);
            let pr = s(row.path_label);
            ids.path_label = tree.add_label(
                clip_parent, pr.x, pr.y, pr.width, pr.height,
                &path_text,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: SMALL_FONT,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;

            // +new clip button
            let ncr = s(row.new_clip);
            ids.new_clip = tree.add_button(
                clip_parent, ncr.x, ncr.y, ncr.width, ncr.height,
                small_button_style(), "+ new clip",
            ) as i32;
        }

        // MIDI controls — shared by video and generator
        if row.has_video_controls || row.has_generator_controls {
            // MIDI label + input
            let mlr = s(row.midi_label);
            ids.midi_label = tree.add_label(
                clip_parent, mlr.x, mlr.y, mlr.width, mlr.height,
                "MIDI",
                UIStyle {
                    text_color: color::TEXT_SUBTLE,
                    font_size: SMALL_FONT,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;

            let midi_text = midi_note_to_name(layer.midi_note);
            let mir = s(row.midi_input);
            ids.midi_input = tree.add_button(
                clip_parent, mir.x, mir.y, mir.width, mir.height,
                field_style(), &midi_text,
            ) as i32;

            // Channel label + dropdown
            let clr = s(row.ch_label);
            ids.ch_label = tree.add_label(
                clip_parent, clr.x, clr.y, clr.width, clr.height,
                "CH",
                UIStyle {
                    text_color: color::TEXT_SUBTLE,
                    font_size: SMALL_FONT,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;

            let ch_text = if layer.midi_channel < 0 {
                "All".to_string()
            } else {
                format!("Ch {}", layer.midi_channel + 1)
            };
            let cdr = s(row.ch_dropdown);
            ids.ch_dropdown = tree.add_button(
                clip_parent, cdr.x, cdr.y, cdr.width, cdr.height,
                small_button_style(), &ch_text,
            ) as i32;
        }

        if row.has_generator_controls {
            let agr = s(row.add_gen_clip);
            ids.add_gen_clip = tree.add_button(
                clip_parent, agr.x, agr.y, agr.width, agr.height,
                small_button_style(), "+ Clip",
            ) as i32;
        }
    }

    fn handle_click(&self, node_id: u32, modifiers: crate::input::Modifiers) -> Vec<PanelAction> {
        let id = node_id as i32;
        // Add Layer button
        if id == self.add_layer_btn && id >= 0 {
            return vec![PanelAction::AddLayerClicked];
        }
        for (i, row) in self.rows.iter().enumerate() {
            if id == row.mute { return vec![PanelAction::ToggleMute(i)]; }
            if id == row.solo { return vec![PanelAction::ToggleSolo(i)]; }
            if id == row.chevron { return vec![PanelAction::ChevronClicked(i)]; }
            if id == row.blend_mode { return vec![PanelAction::BlendModeClicked(i)]; }
            if id == row.folder { return vec![PanelAction::FolderClicked(i)]; }
            if id == row.new_clip { return vec![PanelAction::NewClipClicked(i)]; }
            if id == row.add_gen_clip { return vec![PanelAction::AddGenClipClicked(i)]; }
            if id == row.midi_input { return vec![PanelAction::MidiInputClicked(i)]; }
            if id == row.ch_dropdown { return vec![PanelAction::MidiChannelClicked(i)]; }
            if id == row.name || id == row.bg || id == row.drag_handle {
                return vec![PanelAction::LayerClicked(i, modifiers)];
            }
        }
        Vec::new()
    }

    fn handle_double_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        for (i, row) in self.rows.iter().enumerate() {
            if id == row.name {
                return vec![PanelAction::LayerDoubleClicked(i)];
            }
        }
        Vec::new()
    }

    fn handle_right_click(&self, pos: Vec2) -> Vec<PanelAction> {
        // Reject right-clicks outside the layer controls X bounds.
        // All panels receive all events; without this check, a right-click
        // in the inspector (same Y as a layer) would open the layer context menu.
        if self.panel_width > 0.0 {
            let local_x = pos.x - self.panel_origin.x;
            if local_x < 0.0 || local_x > self.panel_width {
                return Vec::new();
            }
        }
        // Position-based lookup: find which layer the Y coordinate falls in
        let local_y = pos.y - self.panel_origin.y;
        for (i, layer) in self.layers.iter().enumerate() {
            if local_y >= layer.y_offset && local_y < layer.y_offset + layer.height {
                return vec![PanelAction::LayerHeaderRightClicked(i)];
            }
        }
        Vec::new()
    }
}

impl Default for LayerHeaderPanel {
    fn default() -> Self { Self::new() }
}

impl Panel for LayerHeaderPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        let lc = layout.layer_controls();
        // Offset layer rows down by the header stack (overview strip + ruler + waveform lanes)
        // so they align vertically with the track content area.
        // INVARIANT: this MUST match viewport.rs header_h computation exactly.
        let header_spacer = layout.track_header_height();
        self.panel_origin = Vec2::new(lc.x, lc.y + header_spacer - self.scroll_y_px);
        self.panel_width = lc.width;

        // Full-area background (prevents compositor blit bleed-through)
        tree.add_panel(
            -1, lc.x, lc.y, lc.width, lc.height,
            UIStyle { bg_color: color::CONTROL_BG, ..UIStyle::default() },
        );

        // Create a clip region for the scrollable layer rows area.
        // This prevents layer content from overflowing into the header or footer.
        // The clip rect covers from below the header spacer to the bottom of the body.
        let clip_top = lc.y + header_spacer;
        let clip_height = (lc.height - header_spacer).max(0.0);
        let clip_rect = Rect::new(lc.x, clip_top, lc.width, clip_height);
        self.scroll_clip_id = tree.add_node(
            -1,
            clip_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        ) as i32;

        let layer_count = self.layers.len();
        self.rows = vec![LayerRowIds::default(); layer_count];
        // Only resize cached state vectors if layer count changed —
        // preserve existing values to keep dirty-check logic correct.
        self.cached_mute.resize(layer_count, false);
        self.cached_solo.resize(layer_count, false);
        self.cached_selected.resize(layer_count, false);
        self.cached_colors.resize(layer_count, Color32::TRANSPARENT);

        // Clone layers to avoid borrow conflict in build_layer_row
        let layers_snapshot = self.layers.clone();

        // Clip bounds: layer rows are only visible within the scrollable area
        // (below the header spacer, above the footer). Rows outside are skipped.
        let clip_top = lc.y + header_spacer;
        let clip_bottom = lc.y + lc.height;

        for i in 0..layer_count {
            let layer = &layers_snapshot[i];
            if layer.height <= 0.0 { continue; }

            // Check if this layer row is within the visible clip bounds
            let row_screen_y = self.panel_origin.y + layer.y_offset;
            let row_screen_bottom = row_screen_y + layer.height;
            if row_screen_bottom < clip_top || row_screen_y > clip_bottom {
                // Entirely outside visible area — skip
                continue;
            }

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
                layer.y_offset, layer.height, lc.width,
                layer.is_collapsed, layer.is_group, layer.is_generator,
                is_child, is_last_child,
                layer.is_group && !layer.is_collapsed,
            );

            self.build_layer_row(tree, i, layer, row, self.panel_origin, self.scroll_clip_id);
            self.cached_mute[i] = layer.is_muted;
            self.cached_solo[i] = layer.is_solo;
            self.cached_selected[i] = layer.is_selected;
            self.cached_colors[i] = layer.color;
        }

        // Insert indicator (hidden off-screen)
        self.insert_indicator_id = tree.add_panel(
            self.scroll_clip_id, lc.x, lc.y - 10.0, lc.width, INSERT_LINE_H,
            UIStyle { bg_color: Color32::TRANSPARENT, ..UIStyle::default() },
        ) as i32;

        // No "+ Add Layer" button — layers are added via right-click context menu
        self.add_layer_btn = -1;
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
            let old_idx = old.and_then(|id|
                self.layers.iter().position(|l| l.layer_id == *id));
            let new_idx = new.and_then(|id|
                self.layers.iter().position(|l| l.layer_id == *id));

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
            UIEvent::Click { node_id, modifiers, .. } => {
                self.pending_drag_layer = -1;
                self.handle_click(*node_id, *modifiers)
            }
            UIEvent::DoubleClick { node_id, .. } => self.handle_double_click(*node_id),
            UIEvent::RightClick { pos, .. } => self.handle_right_click(*pos),
            // PointerDown on drag handle → save index for DragBegin fallback.
            // Do NOT return LayerClicked here: that triggers a structural rebuild
            // which invalidates node IDs before DragBegin fires, breaking drag.
            // Selection happens on Click (release) instead — acceptable for drag handles.
            UIEvent::PointerDown { node_id, .. } => {
                let id = *node_id as i32;
                for (i, row) in self.rows.iter().enumerate() {
                    if id == row.drag_handle {
                        self.pending_drag_layer = i as i32;
                        return Vec::new();
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
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
            is_muted: false,
            is_solo: false,
            parent_layer_id: None,
            blend_mode: "Normal".into(),
            generator_type: None,
            clip_count: 5,
            video_folder_path: None,
            source_clip_count: 0,
            midi_note: -1,
            midi_channel: -1,
            y_offset,
            height,
            is_selected: false,
            color: Color32::new(100, 148, 210, 220),
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
            assert!(panel.rows[i].bg >= 0, "layer {} bg", i);
            assert!(panel.rows[i].name >= 0, "layer {} name", i);
            assert!(panel.rows[i].mute >= 0, "layer {} mute", i);
            assert!(panel.rows[i].solo >= 0, "layer {} solo", i);
            assert!(panel.rows[i].blend_mode >= 0, "layer {} blend", i);
        }
        // Generator layer should have gen_type and add_gen_clip
        assert!(panel.rows[2].gen_type >= 0);
        assert!(panel.rows[2].add_gen_clip >= 0);
        // Video layers should have folder and new_clip
        assert!(panel.rows[0].folder >= 0);
        assert!(panel.rows[0].new_clip >= 0);
        // Insert indicator
        assert!(panel.insert_indicator_id >= 0);
    }

    #[test]
    fn handle_click_mute_solo() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.rows[0].mute as u32, crate::input::Modifiers::NONE);
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::ToggleMute(0)));

        let a = panel.handle_click(panel.rows[0].solo as u32, crate::input::Modifiers::NONE);
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::ToggleSolo(0)));
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.rows[0].chevron as u32, crate::input::Modifiers::NONE);
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

        panel.set_layers(vec![
            make_group_layer("Group", 0.0, 70.0),
            child,
        ]);

        panel.build(&mut tree, &layout);

        // Group has connector
        assert!(panel.rows[0].connector >= 0);
        // Child has accent bar and bottom border (last child)
        assert!(panel.rows[1].accent_bar >= 0);
        assert!(panel.rows[1].bottom_border >= 0);
    }

    #[test]
    fn midi_note_name_conversion() {
        assert_eq!(midi_note_to_name(-1), "None");
        assert_eq!(midi_note_to_name(60), "C4");
        assert_eq!(midi_note_to_name(69), "A4");
        assert_eq!(midi_note_to_name(36), "C2");
        assert_eq!(midi_note_to_name(127), "G9");
    }

    #[test]
    fn folder_path_extraction() {
        assert_eq!(folder_path_text(&None, 0), "None");
        assert_eq!(folder_path_text(&Some(String::new()), 0), "None");
        assert_eq!(folder_path_text(&Some("/Users/test/Videos/Drums/".into()), 12), "Drums/ (12)");
        assert_eq!(folder_path_text(&Some("C:\\Videos\\Synth".into()), 5), "Synth/ (5)");
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
        assert_eq!(panel.rows[0].folder, -1);
        assert_eq!(panel.rows[0].new_clip, -1);
        assert_eq!(panel.rows[0].midi_input, -1);
        assert_eq!(panel.rows[0].ch_dropdown, -1);
        // But should still have mute/solo/blend
        assert!(panel.rows[0].mute >= 0);
        assert!(panel.rows[0].solo >= 0);
        assert!(panel.rows[0].blend_mode >= 0);
    }

    #[test]
    fn handle_double_click_name() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = LayerHeaderPanel::new();
        panel.set_layers(vec![make_video_layer("L1", 0.0, 140.0)]);
        panel.build(&mut tree, &layout);

        let event = UIEvent::DoubleClick {
            node_id: panel.rows[0].name as u32,
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
        assert_eq!(panel.blend_mode_node_id(0), -1);
        assert_eq!(panel.midi_channel_node_id(99), -1);
        assert_eq!(panel.name_node_id(0), -1);
    }
}
