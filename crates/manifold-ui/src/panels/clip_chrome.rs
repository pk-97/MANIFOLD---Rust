use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::PanelAction;

// ── Layout constants (from ClipChromeBitmapPanel.cs) ──────────────

const HEADER_ROW_H: f32 = 27.5;
const NAME_ROW_H: f32 = 20.0;
const SECTION_LABEL_H: f32 = 18.0;
const SMALL_ROW_H: f32 = 18.0;
const SLIDER_ROW_H: f32 = 22.5;
const BPM_ROW_H: f32 = 22.5;
const LOOP_BUTTON_H: f32 = 24.0;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = 2.0;
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const SOURCE_LABEL_W: f32 = 52.0;
const BPM_LABEL_W: f32 = 52.0;
const FONT_SIZE: u16 = 10;
const NAME_FONT_SIZE: u16 = 12;
const SMALL_FONT_SIZE: u16 = 11;

// ── Panel-specific colors ────────────────────────────────────────

const LOOP_ON_COLOR: Color32 = Color32::new(50, 100, 180, 255);
const LOOP_OFF_COLOR: Color32 = Color32::new(45, 45, 48, 255);
const BPM_BTN_COLOR: Color32 = Color32::new(40, 40, 42, 255);
const BPM_BTN_HOVER: Color32 = Color32::new(50, 50, 55, 255);
const GEN_TYPE_COLOR: Color32 = Color32::new(100, 199, 140, 255);

// ── ClipChromePanel ──────────────────────────────────────────────

pub struct ClipChromePanel {
    // Node IDs
    header_label_id: i32,
    chevron_btn_id: i32,
    name_label_id: i32,
    source_section_label_id: i32,
    source_name_label_id: i32,
    slip_slider: Option<SliderNodeIds>,
    bpm_label_id: i32,
    bpm_value_btn_id: i32,
    loop_toggle_btn_id: i32,
    loop_slider: Option<SliderNodeIds>,
    gen_type_label_id: i32,
    effects_label_id: i32,
    divider_ids: [i32; 3],

    // State
    is_collapsed: bool,
    has_clip: bool,
    mode_video: bool,
    mode_generator: bool,
    mode_looping: bool,
    dragging_slip: bool,
    dragging_loop: bool,

    // Cached values
    cached_name: String,
    cached_source_name: String,
    cached_bpm_text: String,
    cached_gen_type: String,
    cached_slip: f32,
    cached_loop_duration: f32,
    cached_loop_enabled: bool,
    max_slip: f32,
    max_loop_beats: f32,

    // Node range
    first_node: usize,
    node_count: usize,
}

impl ClipChromePanel {
    pub fn new() -> Self {
        Self {
            header_label_id: -1,
            chevron_btn_id: -1,
            name_label_id: -1,
            source_section_label_id: -1,
            source_name_label_id: -1,
            slip_slider: None,
            bpm_label_id: -1,
            bpm_value_btn_id: -1,
            loop_toggle_btn_id: -1,
            loop_slider: None,
            gen_type_label_id: -1,
            effects_label_id: -1,
            divider_ids: [-1; 3],
            is_collapsed: false,
            has_clip: false,
            mode_video: false,
            mode_generator: false,
            mode_looping: false,
            dragging_slip: false,
            dragging_loop: false,
            cached_name: String::new(),
            cached_source_name: String::new(),
            cached_bpm_text: "Auto".into(),
            cached_gen_type: String::new(),
            cached_slip: 0.0,
            cached_loop_duration: 0.0,
            cached_loop_enabled: false,
            max_slip: 1.0,
            max_loop_beats: 1.0,
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn compute_height(&self) -> f32 {
        if self.is_collapsed {
            return PAD_V + HEADER_ROW_H + PAD_V;
        }
        let mut h = PAD_V + HEADER_ROW_H + DIVIDER_H + NAME_ROW_H;
        if self.has_clip {
            h += DIVIDER_H;
            if self.mode_video {
                h += SECTION_LABEL_H + SMALL_ROW_H + SLIDER_ROW_H + BPM_ROW_H + LOOP_BUTTON_H;
                if self.mode_looping {
                    h += SLIDER_ROW_H;
                }
            } else if self.mode_generator {
                h += SMALL_ROW_H;
            }
            h += DIVIDER_H + SECTION_LABEL_H;
        }
        h += DIVIDER_H + PAD_V;
        h
    }

    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn is_dragging(&self) -> bool { self.dragging_slip || self.dragging_loop }

    /// Returns true if mode changed (caller should rebuild).
    pub fn set_mode(&mut self, has_clip: bool, is_video: bool, is_generator: bool, is_looping: bool) -> bool {
        if self.has_clip == has_clip && self.mode_video == is_video
            && self.mode_generator == is_generator && self.mode_looping == is_looping
        {
            return false;
        }
        self.has_clip = has_clip;
        self.mode_video = is_video;
        self.mode_generator = is_generator;
        self.mode_looping = is_looping;
        true
    }

    pub fn set_slip_range(&mut self, max: f32) { self.max_slip = max.max(0.001); }
    pub fn set_loop_range(&mut self, max_beats: f32) { self.max_loop_beats = max_beats.max(0.001); }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let mut cy = rect.y + PAD_V;

        let name = self.cached_name.clone();
        let source_name = self.cached_source_name.clone();
        let bpm_text = self.cached_bpm_text.clone();
        let gen_type = self.cached_gen_type.clone();
        let slip = self.cached_slip;
        let loop_dur = self.cached_loop_duration;

        // Header row
        let label_w = content_w - CHEVRON_W - GAP;
        self.header_label_id = tree.add_label(
            -1, cx, cy, label_w, HEADER_ROW_H,
            "Clip",
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: color::FONT_HEADING,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let chev_x = cx + content_w - CHEVRON_W;
        self.chevron_btn_id = tree.add_button(
            -1, chev_x, cy + (HEADER_ROW_H - 16.0) * 0.5, CHEVRON_W, 16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" },
        ) as i32;

        cy += HEADER_ROW_H;

        if self.is_collapsed {
            self.node_count = tree.count() - self.first_node;
            return;
        }

        let mut div_idx = 0;

        // Divider
        self.divider_ids[div_idx] = tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        ) as i32;
        div_idx += 1;
        cy += DIVIDER_H;

        // Name row
        self.name_label_id = tree.add_label(
            -1, cx, cy, content_w, NAME_ROW_H,
            &name,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: NAME_FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        cy += NAME_ROW_H;

        if self.has_clip {
            // Divider
            self.divider_ids[div_idx] = tree.add_panel(
                -1, cx, cy, content_w, DIVIDER_H,
                UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
            ) as i32;
            div_idx += 1;
            cy += DIVIDER_H;

            if self.mode_video {
                cy = self.build_video_section(tree, cx, cy, content_w, &source_name, &bpm_text, slip, loop_dur);
            } else if self.mode_generator {
                cy = self.build_gen_type_row(tree, cx, cy, content_w, &gen_type);
            }

            // Divider before effects label
            if div_idx < 3 {
                self.divider_ids[div_idx] = tree.add_panel(
                    -1, cx, cy, content_w, DIVIDER_H,
                    UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
                ) as i32;
            }
            cy += DIVIDER_H;

            // Effects section label
            self.effects_label_id = tree.add_label(
                -1, cx, cy, content_w, SECTION_LABEL_H,
                "Effects",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: SMALL_FONT_SIZE,
                    font_weight: FontWeight::Bold,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_video_section(
        &mut self, tree: &mut UITree,
        cx: f32, mut cy: f32, w: f32,
        source_name: &str, bpm_text: &str, slip: f32, loop_dur: f32,
    ) -> f32 {
        // "Source" section label
        self.source_section_label_id = tree.add_label(
            -1, cx, cy, w, SECTION_LABEL_H,
            "Source",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SECTION_LABEL_H;

        // Source name
        self.source_name_label_id = tree.add_label(
            -1, cx + SOURCE_LABEL_W + GAP, cy,
            (w - SOURCE_LABEL_W - GAP).max(10.0), SMALL_ROW_H,
            source_name,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SMALL_ROW_H;

        // Slip slider
        let slip_norm = if self.max_slip > 0.0 { (slip / self.max_slip).clamp(0.0, 1.0) } else { 0.0 };
        let slip_text = format!("{:.2}s", slip);
        let slip_rect = Rect::new(cx, cy, w, SLIDER_ROW_H);
        self.slip_slider = Some(BitmapSlider::build(
            tree, -1, slip_rect,
            Some("Slip"), slip_norm,
            &slip_text, &SliderColors::default_slider(),
            FONT_SIZE, SOURCE_LABEL_W,
        ));
        cy += SLIDER_ROW_H;

        // BPM row
        self.bpm_label_id = tree.add_label(
            -1, cx, cy, BPM_LABEL_W, BPM_ROW_H,
            "Src BPM",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let bpm_btn_w = (w - BPM_LABEL_W - GAP).max(20.0);
        self.bpm_value_btn_id = tree.add_button(
            -1, cx + BPM_LABEL_W + GAP, cy + (BPM_ROW_H - 18.0) * 0.5,
            bpm_btn_w, 18.0,
            UIStyle {
                bg_color: BPM_BTN_COLOR,
                hover_bg_color: BPM_BTN_HOVER,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FONT_SIZE,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            bpm_text,
        ) as i32;
        cy += BPM_ROW_H;

        // Loop toggle button
        let loop_base = if self.cached_loop_enabled { LOOP_ON_COLOR } else { LOOP_OFF_COLOR };
        self.loop_toggle_btn_id = tree.add_button(
            -1, cx, cy, w, LOOP_BUTTON_H,
            UIStyle {
                bg_color: loop_base,
                hover_bg_color: lighten(loop_base, 10),
                pressed_bg_color: darken(loop_base, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.cached_loop_enabled { "Loop ON" } else { "Loop OFF" },
        ) as i32;
        cy += LOOP_BUTTON_H;

        // Loop slider (only if looping)
        if self.mode_looping {
            let loop_norm = if self.max_loop_beats > 0.0 {
                (loop_dur / self.max_loop_beats).clamp(0.0, 1.0)
            } else { 0.0 };
            let loop_text = format_beat_value(loop_dur);
            let loop_rect = Rect::new(cx, cy, w, SLIDER_ROW_H);
            self.loop_slider = Some(BitmapSlider::build(
                tree, -1, loop_rect,
                Some("Loop"), loop_norm,
                &loop_text, &SliderColors::default_slider(),
                FONT_SIZE, SOURCE_LABEL_W,
            ));
            cy += SLIDER_ROW_H;
        } else {
            self.loop_slider = None;
        }

        cy
    }

    fn build_gen_type_row(
        &mut self, tree: &mut UITree,
        cx: f32, cy: f32, w: f32, gen_type: &str,
    ) -> f32 {
        let label = format!("Type: {}", gen_type);
        self.gen_type_label_id = tree.add_label(
            -1, cx, cy, w, SMALL_ROW_H,
            &label,
            UIStyle {
                text_color: GEN_TYPE_COLOR,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy + SMALL_ROW_H
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_name(&mut self, tree: &mut UITree, name: &str) {
        self.cached_name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    pub fn sync_collapsed(&mut self, tree: &mut UITree, collapsed: bool) {
        self.is_collapsed = collapsed;
        if self.chevron_btn_id >= 0 {
            tree.set_text(
                self.chevron_btn_id as u32,
                if collapsed { "\u{25B6}" } else { "\u{25BC}" },
            );
        }
    }

    pub fn sync_source_name(&mut self, tree: &mut UITree, name: &str) {
        self.cached_source_name = name.into();
        if self.source_name_label_id >= 0 {
            tree.set_text(self.source_name_label_id as u32, name);
        }
    }

    pub fn sync_slip(&mut self, tree: &mut UITree, value: f32) {
        self.cached_slip = value;
        if let Some(ref ids) = self.slip_slider {
            let norm = if self.max_slip > 0.0 { (value / self.max_slip).clamp(0.0, 1.0) } else { 0.0 };
            let text = format!("{:.2}s", value);
            BitmapSlider::update_value(tree, ids, norm, &text);
        }
    }

    pub fn sync_bpm(&mut self, tree: &mut UITree, text: &str) {
        self.cached_bpm_text = text.into();
        if self.bpm_value_btn_id >= 0 {
            tree.set_text(self.bpm_value_btn_id as u32, text);
        }
    }

    pub fn sync_gen_type(&mut self, tree: &mut UITree, gen_type: &str) {
        self.cached_gen_type = gen_type.into();
        if self.gen_type_label_id >= 0 {
            let label = format!("Type: {}", gen_type);
            tree.set_text(self.gen_type_label_id as u32, &label);
        }
    }

    pub fn sync_loop_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        self.cached_loop_enabled = enabled;
        if self.loop_toggle_btn_id >= 0 {
            tree.set_text(
                self.loop_toggle_btn_id as u32,
                if enabled { "Loop ON" } else { "Loop OFF" },
            );
            let base = if enabled { LOOP_ON_COLOR } else { LOOP_OFF_COLOR };
            tree.set_style(self.loop_toggle_btn_id as u32, UIStyle {
                bg_color: base,
                hover_bg_color: lighten(base, 10),
                pressed_bg_color: darken(base, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            });
        }
    }

    pub fn sync_loop_duration(&mut self, tree: &mut UITree, beats: f32) {
        self.cached_loop_duration = beats;
        if let Some(ref ids) = self.loop_slider {
            let norm = if self.max_loop_beats > 0.0 {
                (beats / self.max_loop_beats).clamp(0.0, 1.0)
            } else { 0.0 };
            let text = format_beat_value(beats);
            BitmapSlider::update_value(tree, ids, norm, &text);
        }
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.chevron_btn_id {
            return vec![PanelAction::ClipChromeCollapseToggle];
        }
        if id == self.bpm_value_btn_id && self.mode_video {
            return vec![PanelAction::ClipBpmClicked];
        }
        if id == self.loop_toggle_btn_id && self.mode_video {
            return vec![PanelAction::ClipLoopToggle];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        if self.mode_video {
            if let Some(ref ids) = self.slip_slider {
                if node_id == ids.track {
                    self.dragging_slip = true;
                    let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                    let val = norm * self.max_slip;
                    return vec![
                        PanelAction::ClipSlipSnapshot,
                        PanelAction::ClipSlipChanged(val),
                    ];
                }
            }
            if self.mode_looping {
                if let Some(ref ids) = self.loop_slider {
                    if node_id == ids.track {
                        self.dragging_loop = true;
                        let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                        let beats = snap_quarter_note(norm * self.max_loop_beats);
                        return vec![
                            PanelAction::ClipLoopSnapshot,
                            PanelAction::ClipLoopChanged(beats),
                        ];
                    }
                }
            }
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_slip {
            if let Some(ref ids) = self.slip_slider {
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let val = norm * self.max_slip;
                let text = format!("{:.2}s", val);
                BitmapSlider::update_value(tree, ids, norm, &text);
                return vec![PanelAction::ClipSlipChanged(val)];
            }
        }
        if self.dragging_loop {
            if let Some(ref ids) = self.loop_slider {
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let beats = snap_quarter_note(norm * self.max_loop_beats);
                let text = format_beat_value(beats);
                let snapped_norm = if self.max_loop_beats > 0.0 { beats / self.max_loop_beats } else { 0.0 };
                BitmapSlider::update_value(tree, ids, snapped_norm, &text);
                return vec![PanelAction::ClipLoopChanged(beats)];
            }
        }
        Vec::new()
    }

    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_slip {
            self.dragging_slip = false;
            if let Some(ref ids) = self.slip_slider {
                let norm = if self.max_slip > 0.0 { (self.cached_slip / self.max_slip).clamp(0.0, 1.0) } else { 0.0 };
                let text = format!("{:.2}s", self.cached_slip);
                BitmapSlider::update_value(tree, ids, norm, &text);
            }
            return vec![PanelAction::ClipSlipCommit];
        }
        if self.dragging_loop {
            self.dragging_loop = false;
            if let Some(ref ids) = self.loop_slider {
                let norm = if self.max_loop_beats > 0.0 {
                    (self.cached_loop_duration / self.max_loop_beats).clamp(0.0, 1.0)
                } else { 0.0 };
                let text = format_beat_value(self.cached_loop_duration);
                BitmapSlider::update_value(tree, ids, norm, &text);
            }
            return vec![PanelAction::ClipLoopCommit];
        }
        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        if let Some(ref ids) = self.slip_slider {
            if node_id == ids.track { return vec![PanelAction::ClipSlipSnapshot]; }
        }
        if let Some(ref ids) = self.loop_slider {
            if node_id == ids.track { return vec![PanelAction::ClipLoopSnapshot]; }
        }
        Vec::new()
    }

    pub fn bpm_button_rect(&self, tree: &UITree) -> Rect {
        if self.bpm_value_btn_id >= 0 {
            tree.get_bounds(self.bpm_value_btn_id as u32)
        } else {
            Rect::ZERO
        }
    }
}

impl Default for ClipChromePanel {
    fn default() -> Self { Self::new() }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Snap to nearest quarter-note subdivision.
fn snap_quarter_note(beats: f32) -> f32 {
    (beats * 4.0).round() / 4.0
}

/// Format beat duration as user-friendly string.
fn format_beat_value(beats: f32) -> String {
    if beats <= 0.0 { return "Full".into(); }
    let quarter_notes = (beats * 4.0).round() as i32;
    match quarter_notes {
        4 => "1b".into(),
        2 => "1/2b".into(),
        1 => "1/4b".into(),
        _ => format!("{:.2}", beats),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    #[test]
    fn build_clip_chrome_no_clip() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.header_label_id >= 0);
        assert!(panel.chevron_btn_id >= 0);
        assert!(panel.node_count > 0);
    }

    #[test]
    fn build_clip_chrome_video_mode() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.slip_slider.is_some());
        assert!(panel.bpm_value_btn_id >= 0);
        assert!(panel.loop_toggle_btn_id >= 0);
        assert!(panel.effects_label_id >= 0);
    }

    #[test]
    fn build_clip_chrome_gen_mode() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, true, false);
        panel.cached_gen_type = "Plasma".into();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.gen_type_label_id >= 0);
        assert!(panel.effects_label_id >= 0);
        assert!(panel.slip_slider.is_none());
    }

    #[test]
    fn set_mode_returns_changed() {
        let mut panel = ClipChromePanel::new();
        assert!(panel.set_mode(true, true, false, false));
        assert!(!panel.set_mode(true, true, false, false));
        assert!(panel.set_mode(true, true, false, true));
    }

    #[test]
    fn handle_click_bpm() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let actions = panel.handle_click(panel.bpm_value_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::ClipBpmClicked));
    }

    #[test]
    fn handle_click_loop_toggle() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let actions = panel.handle_click(panel.loop_toggle_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::ClipLoopToggle));
    }

    #[test]
    fn format_beat_value_common() {
        assert_eq!(format_beat_value(1.0), "1b");
        assert_eq!(format_beat_value(0.5), "1/2b");
        assert_eq!(format_beat_value(0.25), "1/4b");
        assert_eq!(format_beat_value(0.0), "Full");
    }

    #[test]
    fn snap_quarter_note_values() {
        assert!((snap_quarter_note(0.3) - 0.25).abs() < 0.01);
        assert!((snap_quarter_note(0.6) - 0.5).abs() < 0.01);
        assert!((snap_quarter_note(1.1) - 1.0).abs() < 0.01);
    }
}
