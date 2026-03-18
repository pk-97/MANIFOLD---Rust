use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants (from TransportLayout.cs) ─────────────────────

const INSET: f32 = 8.0;
const GROUP_Y_PAD: f32 = 4.0;
const ITEM_SPACING: f32 = 5.0;
const SECTION_SPACER: f32 = 8.0;
const RIGHT_SPACING: f32 = 4.0;
const CENTER_SPACER: f32 = 12.0;

const STATUS_DOT_SIZE: f32 = 8.0;
const STATUS_TEXT_W: f32 = 55.0;

const CLOCK_AUTHORITY_W: f32 = 68.0;
const LINK_BUTTON_W: f32 = 45.0;
const CLK_BUTTON_W: f32 = 35.0;
const CLK_DEVICE_W: f32 = 100.0;
const SYNC_BUTTON_W: f32 = 45.0;

const PLAY_BUTTON_W: f32 = 50.0;
const STOP_BUTTON_W: f32 = 50.0;
const REC_BUTTON_W: f32 = 42.0;
const BPM_LABEL_W: f32 = 28.0;
const BPM_FIELD_W: f32 = 60.0;
const BPM_RESET_W: f32 = 24.0;
const BPM_CLEAR_W: f32 = 32.0;

const NEW_BUTTON_W: f32 = 40.0;
const OPEN_BUTTON_W: f32 = 45.0;
const OPEN_RECENT_W: f32 = 92.0;
const SAVE_BUTTON_W: f32 = 42.0;
const SAVE_AS_W: f32 = 55.0;
const EXPORT_BUTTON_W: f32 = 55.0;
const HDR_BUTTON_W: f32 = 35.0;
const PERC_BUTTON_W: f32 = 48.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_HOVER_C: Color32 = color::TRANSPORT_BUTTON_HOVER;
const SAVE_DIRTY_BG: Color32 = color::TRANSPORT_SAVE_DIRTY_BG;
const BPM_FIELD_HOVER: Color32 = color::TRANSPORT_BPM_FIELD_HOVER;

const BUTTON_FONT: u16 = 12;
const STATUS_FONT: u16 = 10;
// ── Style helpers ──────────────────────────────────────────────────

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

fn button_style(bg: Color32) -> UIStyle {
    let is_active = bg != color::BUTTON_INACTIVE_C32;
    let hover = if is_active { lighten(bg, 30) } else { BUTTON_HOVER_C };
    let pressed = if is_active { darken(bg, 20) } else { color::BUTTON_PRESSED };
    UIStyle {
        bg_color: bg,
        hover_bg_color: hover,
        pressed_bg_color: pressed,
        text_color: color::TEXT_WHITE_C32,
        font_size: BUTTON_FONT,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

fn dot_style(c: Color32) -> UIStyle {
    UIStyle {
        bg_color: c,
        corner_radius: 4.0,
        ..UIStyle::default()
    }
}

fn status_text_style(c: Color32) -> UIStyle {
    UIStyle {
        text_color: c,
        font_size: STATUS_FONT,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

// ── TransportLayout ────────────────────────────────────────────────

#[derive(Default)]
struct TransportLayout {
    clock_authority: Rect, link_button: Rect, link_dot: Rect, link_status: Rect,
    clk_button: Rect, clk_device: Rect, clk_dot: Rect, clk_status: Rect,
    sync_button: Rect, sync_dot: Rect, sync_status: Rect,
    play_button: Rect, stop_button: Rect, rec_button: Rect,
    bpm_label: Rect, bpm_field: Rect, bpm_reset: Rect, bpm_clear: Rect,
    new_button: Rect, open_button: Rect, open_recent: Rect,
    save_button: Rect, save_as: Rect, export_button: Rect,
    hdr_button: Rect, perc_button: Rect,
}

impl TransportLayout {
    fn compute(&mut self, bounds: Rect) {
        let elem_h = bounds.height - GROUP_Y_PAD * 2.0;
        let elem_y = bounds.y + GROUP_Y_PAD;
        self.compute_left(bounds, elem_y, elem_h);
        self.compute_center(bounds, elem_y, elem_h);
        self.compute_right(bounds, elem_y, elem_h);
    }

    fn compute_left(&mut self, bounds: Rect, ey: f32, eh: f32) {
        let mut x = bounds.x + INSET;
        let dot_y = ey + (eh - STATUS_DOT_SIZE) * 0.5;

        self.clock_authority = Rect::new(x, ey, CLOCK_AUTHORITY_W, eh);
        x += CLOCK_AUTHORITY_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;

        self.link_button = Rect::new(x, ey, LINK_BUTTON_W, eh);
        x += LINK_BUTTON_W + ITEM_SPACING;
        self.link_dot = Rect::new(x, dot_y, STATUS_DOT_SIZE, STATUS_DOT_SIZE);
        x += STATUS_DOT_SIZE + ITEM_SPACING;
        self.link_status = Rect::new(x, ey, STATUS_TEXT_W, eh);
        x += STATUS_TEXT_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;

        self.clk_button = Rect::new(x, ey, CLK_BUTTON_W, eh);
        x += CLK_BUTTON_W + ITEM_SPACING;
        self.clk_device = Rect::new(x, ey, CLK_DEVICE_W, eh);
        x += CLK_DEVICE_W + ITEM_SPACING;
        self.clk_dot = Rect::new(x, dot_y, STATUS_DOT_SIZE, STATUS_DOT_SIZE);
        x += STATUS_DOT_SIZE + ITEM_SPACING;
        self.clk_status = Rect::new(x, ey, STATUS_TEXT_W, eh);
        x += STATUS_TEXT_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;

        self.sync_button = Rect::new(x, ey, SYNC_BUTTON_W, eh);
        x += SYNC_BUTTON_W + ITEM_SPACING;
        self.sync_dot = Rect::new(x, dot_y, STATUS_DOT_SIZE, STATUS_DOT_SIZE);
        x += STATUS_DOT_SIZE + ITEM_SPACING;
        self.sync_status = Rect::new(x, ey, STATUS_TEXT_W, eh);
    }

    fn compute_center(&mut self, bounds: Rect, ey: f32, eh: f32) {
        let total_w = Self::center_width();
        let centered_x = bounds.x + (bounds.width - total_w) * 0.5;
        // Clamp so center group doesn't overlap left or right groups
        let left_end = bounds.x + INSET + Self::left_width() + SECTION_SPACER;
        let right_start = bounds.x_max() - INSET - Self::right_width() - SECTION_SPACER;
        let max_center_x = (right_start - total_w).max(left_end);
        let mut x = centered_x.max(left_end).min(max_center_x);

        self.play_button = Rect::new(x, ey, PLAY_BUTTON_W, eh);
        x += PLAY_BUTTON_W + ITEM_SPACING;
        self.stop_button = Rect::new(x, ey, STOP_BUTTON_W, eh);
        x += STOP_BUTTON_W + ITEM_SPACING;
        self.rec_button = Rect::new(x, ey, REC_BUTTON_W, eh);
        x += REC_BUTTON_W + ITEM_SPACING + CENTER_SPACER + ITEM_SPACING;

        self.bpm_label = Rect::new(x, ey, BPM_LABEL_W, eh);
        x += BPM_LABEL_W + ITEM_SPACING;
        self.bpm_field = Rect::new(x, ey, BPM_FIELD_W, eh);
        x += BPM_FIELD_W + ITEM_SPACING;
        self.bpm_reset = Rect::new(x, ey, BPM_RESET_W, eh);
        x += BPM_RESET_W + ITEM_SPACING;
        self.bpm_clear = Rect::new(x, ey, BPM_CLEAR_W, eh);
    }

    fn compute_right(&mut self, bounds: Rect, ey: f32, eh: f32) {
        let mut x = bounds.x_max() - INSET - Self::right_width();

        self.new_button = Rect::new(x, ey, NEW_BUTTON_W, eh);
        x += NEW_BUTTON_W + RIGHT_SPACING;
        self.open_button = Rect::new(x, ey, OPEN_BUTTON_W, eh);
        x += OPEN_BUTTON_W + RIGHT_SPACING;
        self.open_recent = Rect::new(x, ey, OPEN_RECENT_W, eh);
        x += OPEN_RECENT_W + RIGHT_SPACING;
        self.save_button = Rect::new(x, ey, SAVE_BUTTON_W, eh);
        x += SAVE_BUTTON_W + RIGHT_SPACING;
        self.save_as = Rect::new(x, ey, SAVE_AS_W, eh);
        x += SAVE_AS_W + RIGHT_SPACING + SECTION_SPACER;
        self.export_button = Rect::new(x, ey, EXPORT_BUTTON_W, eh);
        x += EXPORT_BUTTON_W + RIGHT_SPACING;
        self.hdr_button = Rect::new(x, ey, HDR_BUTTON_W, eh);
        x += HDR_BUTTON_W + RIGHT_SPACING;
        self.perc_button = Rect::new(x, ey, PERC_BUTTON_W, eh);
    }

    fn left_width() -> f32 {
        CLOCK_AUTHORITY_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING
            + LINK_BUTTON_W + ITEM_SPACING + STATUS_DOT_SIZE + ITEM_SPACING + STATUS_TEXT_W
            + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING
            + CLK_BUTTON_W + ITEM_SPACING + CLK_DEVICE_W + ITEM_SPACING + STATUS_DOT_SIZE
            + ITEM_SPACING + STATUS_TEXT_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING
            + SYNC_BUTTON_W + ITEM_SPACING + STATUS_DOT_SIZE + ITEM_SPACING + STATUS_TEXT_W
    }

    fn center_width() -> f32 {
        PLAY_BUTTON_W + ITEM_SPACING
            + STOP_BUTTON_W + ITEM_SPACING
            + REC_BUTTON_W + ITEM_SPACING + CENTER_SPACER + ITEM_SPACING
            + BPM_LABEL_W + ITEM_SPACING
            + BPM_FIELD_W + ITEM_SPACING
            + BPM_RESET_W + ITEM_SPACING
            + BPM_CLEAR_W
    }

    fn right_width() -> f32 {
        NEW_BUTTON_W + RIGHT_SPACING
            + OPEN_BUTTON_W + RIGHT_SPACING
            + OPEN_RECENT_W + RIGHT_SPACING
            + SAVE_BUTTON_W + RIGHT_SPACING
            + SAVE_AS_W + RIGHT_SPACING + SECTION_SPACER
            + EXPORT_BUTTON_W + RIGHT_SPACING
            + HDR_BUTTON_W + RIGHT_SPACING
            + PERC_BUTTON_W
    }
}

// ── TransportPanel ─────────────────────────────────────────────────

pub struct TransportPanel {
    layout: TransportLayout,

    // Node IDs (-1 = unset)
    clock_authority_id: i32, link_button_id: i32, link_dot_id: i32, link_status_id: i32,
    clk_button_id: i32, clk_device_id: i32, clk_dot_id: i32, clk_status_id: i32,
    sync_button_id: i32, sync_dot_id: i32, sync_status_id: i32,
    play_button_id: i32, stop_button_id: i32, rec_button_id: i32,
    bpm_label_id: i32, bpm_field_id: i32, bpm_reset_id: i32, bpm_clear_id: i32,
    new_button_id: i32, open_button_id: i32, open_recent_id: i32,
    save_button_id: i32, save_as_id: i32, export_button_id: i32,
    hdr_button_id: i32, perc_button_id: i32,

    // Dynamic state
    clock_authority_text: String,
    clock_authority_color: Color32,
    link_enabled: bool,
    link_dot_color: Color32,
    link_status_text: String,
    link_status_color: Color32,
    clk_enabled: bool,
    clk_device_text: String,
    clk_dot_color: Color32,
    clk_status_text: String,
    clk_status_color: Color32,
    sync_enabled: bool,
    sync_dot_color: Color32,
    sync_status_text: String,
    sync_status_color: Color32,
    play_text: String,
    play_color: Color32,
    stop_color: Color32,
    rec_active: bool,
    rec_enabled: bool,
    bpm_text: String,
    bpm_reset_active: bool,
    bpm_clear_active: bool,
    save_text: String,
    export_active: bool,
    hdr_active: bool,
}

impl TransportPanel {
    pub fn new() -> Self {
        Self {
            layout: TransportLayout::default(),
            clock_authority_id: -1, link_button_id: -1, link_dot_id: -1, link_status_id: -1,
            clk_button_id: -1, clk_device_id: -1, clk_dot_id: -1, clk_status_id: -1,
            sync_button_id: -1, sync_dot_id: -1, sync_status_id: -1,
            play_button_id: -1, stop_button_id: -1, rec_button_id: -1,
            bpm_label_id: -1, bpm_field_id: -1, bpm_reset_id: -1, bpm_clear_id: -1,
            new_button_id: -1, open_button_id: -1, open_recent_id: -1,
            save_button_id: -1, save_as_id: -1, export_button_id: -1,
            hdr_button_id: -1, perc_button_id: -1,
            clock_authority_text: "SRC:INT".into(),
            clock_authority_color: color::BUTTON_INACTIVE_C32,
            link_enabled: false,
            link_dot_color: color::DRIVER_INACTIVE_C32,
            link_status_text: "Off".into(),
            link_status_color: color::TEXT_DIMMED_C32,
            clk_enabled: false,
            clk_device_text: "Select...".into(),
            clk_dot_color: color::DRIVER_INACTIVE_C32,
            clk_status_text: "Off".into(),
            clk_status_color: color::TEXT_DIMMED_C32,
            sync_enabled: false,
            sync_dot_color: color::DRIVER_INACTIVE_C32,
            sync_status_text: "Off".into(),
            sync_status_color: color::TEXT_DIMMED_C32,
            play_text: "PLAY".into(),
            play_color: color::PLAY_GREEN,
            stop_color: color::STOP_RED,
            rec_active: false,
            rec_enabled: true,
            bpm_text: "120.0".into(),
            bpm_reset_active: false,
            bpm_clear_active: false,
            save_text: "SAVE".into(),
            export_active: false,
            hdr_active: false,
        }
    }

    // ── Public accessors ───────────────────────────────────────────

    pub fn bpm_field_id(&self) -> i32 { self.bpm_field_id }
    pub fn clock_authority_node_id(&self) -> i32 { self.clock_authority_id }
    pub fn clk_device_node_id(&self) -> i32 { self.clk_device_id }

    /// Get bounds of any node owned by this panel.
    pub fn get_node_bounds(&self, tree: &UITree, node_id: i32) -> Rect {
        if node_id < 0 { return Rect::ZERO; }
        tree.get_bounds(node_id as u32)
    }

    // ── Push-based setters ─────────────────────────────────────────

    pub fn set_clock_authority(&mut self, tree: &mut UITree, text: &str, c: Color32) {
        self.clock_authority_text = text.into();
        self.clock_authority_color = c;
        if self.clock_authority_id >= 0 {
            let id = self.clock_authority_id as u32;
            tree.set_text(id, text);
            tree.set_style(id, button_style(c));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_link_state(
        &mut self, tree: &mut UITree,
        enabled: bool, dot_color: Color32, status: &str, status_color: Color32,
    ) {
        self.link_enabled = enabled;
        self.link_dot_color = dot_color;
        self.link_status_text = status.into();
        self.link_status_color = status_color;
        if self.link_button_id >= 0 {
            let bg = if enabled { color::LINK_ORANGE } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(self.link_button_id as u32, button_style(bg));
        }
        if self.link_dot_id >= 0 {
            tree.set_style(self.link_dot_id as u32, dot_style(dot_color));
        }
        if self.link_status_id >= 0 {
            tree.set_text(self.link_status_id as u32, status);
            tree.set_style(self.link_status_id as u32, status_text_style(status_color));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_clk_state(
        &mut self, tree: &mut UITree,
        enabled: bool, device_text: &str, dot_color: Color32,
        status: &str, status_color: Color32,
    ) {
        self.clk_enabled = enabled;
        self.clk_device_text = device_text.into();
        self.clk_dot_color = dot_color;
        self.clk_status_text = status.into();
        self.clk_status_color = status_color;
        if self.clk_button_id >= 0 {
            let bg = if enabled { color::MIDI_PURPLE } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(self.clk_button_id as u32, button_style(bg));
        }
        if self.clk_device_id >= 0 { tree.set_text(self.clk_device_id as u32, device_text); }
        if self.clk_dot_id >= 0 { tree.set_style(self.clk_dot_id as u32, dot_style(dot_color)); }
        if self.clk_status_id >= 0 {
            tree.set_text(self.clk_status_id as u32, status);
            tree.set_style(self.clk_status_id as u32, status_text_style(status_color));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_sync_state(
        &mut self, tree: &mut UITree,
        enabled: bool, dot_color: Color32, status: &str, status_color: Color32,
    ) {
        self.sync_enabled = enabled;
        self.sync_dot_color = dot_color;
        self.sync_status_text = status.into();
        self.sync_status_color = status_color;
        if self.sync_button_id >= 0 {
            let bg = if enabled { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(self.sync_button_id as u32, button_style(bg));
        }
        if self.sync_dot_id >= 0 { tree.set_style(self.sync_dot_id as u32, dot_style(dot_color)); }
        if self.sync_status_id >= 0 {
            tree.set_text(self.sync_status_id as u32, status);
            tree.set_style(self.sync_status_id as u32, status_text_style(status_color));
        }
    }

    pub fn set_play_state(&mut self, tree: &mut UITree, text: &str, c: Color32) {
        self.play_text = text.into();
        self.play_color = c;
        if self.play_button_id >= 0 {
            let id = self.play_button_id as u32;
            tree.set_text(id, text);
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_record_state(&mut self, tree: &mut UITree, active: bool, enabled: bool) {
        self.rec_active = active;
        self.rec_enabled = enabled;
        if self.rec_button_id >= 0 {
            let id = self.rec_button_id as u32;
            let c = if active { color::RECORD_ACTIVE } else { color::RECORD_RED };
            tree.set_style(id, button_style(c));
            if enabled { tree.clear_flag(id, UIFlags::DISABLED); }
            else { tree.set_flag(id, UIFlags::DISABLED); }
        }
    }

    pub fn set_bpm_text(&mut self, tree: &mut UITree, text: &str) {
        self.bpm_text = text.into();
        if self.bpm_field_id >= 0 { tree.set_text(self.bpm_field_id as u32, text); }
    }

    pub fn set_bpm_reset_active(&mut self, tree: &mut UITree, active: bool) {
        self.bpm_reset_active = active;
        if self.bpm_reset_id >= 0 {
            let id = self.bpm_reset_id as u32;
            let c = if active { color::BPM_RESET_ACTIVE } else { color::BUTTON_PRESSED };
            tree.set_style(id, button_style(c));
            if active { tree.clear_flag(id, UIFlags::DISABLED); }
            else { tree.set_flag(id, UIFlags::DISABLED); }
        }
    }

    pub fn set_bpm_clear_active(&mut self, tree: &mut UITree, active: bool) {
        self.bpm_clear_active = active;
        if self.bpm_clear_id >= 0 {
            let id = self.bpm_clear_id as u32;
            let c = if active { color::BPM_CLEAR_ACTIVE } else { color::BUTTON_PRESSED };
            tree.set_style(id, button_style(c));
            if active { tree.clear_flag(id, UIFlags::DISABLED); }
            else { tree.set_flag(id, UIFlags::DISABLED); }
        }
    }

    pub fn set_save_text(&mut self, tree: &mut UITree, text: &str) {
        self.save_text = text.into();
        if self.save_button_id >= 0 {
            let id = self.save_button_id as u32;
            tree.set_text(id, text);
            let dirty = text.contains('*');
            let c = if dirty { SAVE_DIRTY_BG } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_export_label(&mut self, tree: &mut UITree, text: &str) {
        let _ = (tree, text);
    }

    pub fn set_export_active(&mut self, tree: &mut UITree, active: bool) {
        self.export_active = active;
        if self.export_button_id >= 0 {
            let c = if active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(self.export_button_id as u32, button_style(c));
        }
    }

    pub fn set_hdr_active(&mut self, tree: &mut UITree, active: bool) {
        self.hdr_active = active;
        if self.hdr_button_id >= 0 {
            let c = if active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
            tree.set_style(self.hdr_button_id as u32, button_style(c));
        }
    }

    // ── Build helpers ──────────────────────────────────────────────

    fn build_left(&mut self, tree: &mut UITree, bg: i32) {
        // Clone strings to avoid borrow conflicts
        let clock_text = self.clock_authority_text.clone();
        let link_status = self.link_status_text.clone();
        let clk_device = self.clk_device_text.clone();
        let clk_status = self.clk_status_text.clone();
        let sync_status = self.sync_status_text.clone();

        self.clock_authority_id = tree.add_button(
            bg, self.layout.clock_authority.x, self.layout.clock_authority.y,
            self.layout.clock_authority.width, self.layout.clock_authority.height,
            button_style(self.clock_authority_color), &clock_text,
        ) as i32;

        let link_bg = if self.link_enabled { color::LINK_ORANGE } else { color::BUTTON_INACTIVE_C32 };
        self.link_button_id = tree.add_button(
            bg, self.layout.link_button.x, self.layout.link_button.y,
            self.layout.link_button.width, self.layout.link_button.height,
            button_style(link_bg), "LINK",
        ) as i32;

        self.link_dot_id = tree.add_panel(
            bg, self.layout.link_dot.x, self.layout.link_dot.y,
            self.layout.link_dot.width, self.layout.link_dot.height,
            dot_style(self.link_dot_color),
        ) as i32;

        self.link_status_id = tree.add_node(
            bg, self.layout.link_status, UINodeType::Label,
            status_text_style(self.link_status_color),
            Some(&link_status), UIFlags::empty(),
        ) as i32;

        let clk_bg = if self.clk_enabled { color::MIDI_PURPLE } else { color::BUTTON_INACTIVE_C32 };
        self.clk_button_id = tree.add_button(
            bg, self.layout.clk_button.x, self.layout.clk_button.y,
            self.layout.clk_button.width, self.layout.clk_button.height,
            button_style(clk_bg), "CLK",
        ) as i32;

        self.clk_device_id = tree.add_button(
            bg, self.layout.clk_device.x, self.layout.clk_device.y,
            self.layout.clk_device.width, self.layout.clk_device.height,
            button_style(color::BUTTON_INACTIVE_C32), &clk_device,
        ) as i32;

        self.clk_dot_id = tree.add_panel(
            bg, self.layout.clk_dot.x, self.layout.clk_dot.y,
            self.layout.clk_dot.width, self.layout.clk_dot.height,
            dot_style(self.clk_dot_color),
        ) as i32;

        self.clk_status_id = tree.add_node(
            bg, self.layout.clk_status, UINodeType::Label,
            status_text_style(self.clk_status_color),
            Some(&clk_status), UIFlags::empty(),
        ) as i32;

        let sync_bg = if self.sync_enabled { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        self.sync_button_id = tree.add_button(
            bg, self.layout.sync_button.x, self.layout.sync_button.y,
            self.layout.sync_button.width, self.layout.sync_button.height,
            button_style(sync_bg), "SYNC",
        ) as i32;

        self.sync_dot_id = tree.add_panel(
            bg, self.layout.sync_dot.x, self.layout.sync_dot.y,
            self.layout.sync_dot.width, self.layout.sync_dot.height,
            dot_style(self.sync_dot_color),
        ) as i32;

        self.sync_status_id = tree.add_node(
            bg, self.layout.sync_status, UINodeType::Label,
            status_text_style(self.sync_status_color),
            Some(&sync_status), UIFlags::empty(),
        ) as i32;
    }

    fn build_center(&mut self, tree: &mut UITree, bg: i32) {
        let play_text = self.play_text.clone();
        let bpm_text = self.bpm_text.clone();

        self.play_button_id = tree.add_button(
            bg, self.layout.play_button.x, self.layout.play_button.y,
            self.layout.play_button.width, self.layout.play_button.height,
            button_style(self.play_color), &play_text,
        ) as i32;

        self.stop_button_id = tree.add_button(
            bg, self.layout.stop_button.x, self.layout.stop_button.y,
            self.layout.stop_button.width, self.layout.stop_button.height,
            button_style(self.stop_color), "STOP",
        ) as i32;

        let rec_c = if self.rec_active { color::RECORD_ACTIVE } else { color::RECORD_RED };
        self.rec_button_id = tree.add_button(
            bg, self.layout.rec_button.x, self.layout.rec_button.y,
            self.layout.rec_button.width, self.layout.rec_button.height,
            button_style(rec_c), "REC",
        ) as i32;
        if !self.rec_enabled { tree.set_flag(self.rec_button_id as u32, UIFlags::DISABLED); }

        self.bpm_label_id = tree.add_node(
            bg, self.layout.bpm_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: STATUS_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("BPM"), UIFlags::empty(),
        ) as i32;

        self.bpm_field_id = tree.add_button(
            bg, self.layout.bpm_field.x, self.layout.bpm_field.y,
            self.layout.bpm_field.width, self.layout.bpm_field.height,
            UIStyle {
                bg_color: color::SLIDER_TRACK_C32,
                hover_bg_color: BPM_FIELD_HOVER,
                pressed_bg_color: color::BUTTON_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: BUTTON_FONT,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            &bpm_text,
        ) as i32;

        let reset_c = if self.bpm_reset_active { color::BPM_RESET_ACTIVE } else { color::BUTTON_PRESSED };
        self.bpm_reset_id = tree.add_button(
            bg, self.layout.bpm_reset.x, self.layout.bpm_reset.y,
            self.layout.bpm_reset.width, self.layout.bpm_reset.height,
            button_style(reset_c), "R",
        ) as i32;
        if !self.bpm_reset_active { tree.set_flag(self.bpm_reset_id as u32, UIFlags::DISABLED); }

        let clear_c = if self.bpm_clear_active { color::BPM_CLEAR_ACTIVE } else { color::BUTTON_PRESSED };
        self.bpm_clear_id = tree.add_button(
            bg, self.layout.bpm_clear.x, self.layout.bpm_clear.y,
            self.layout.bpm_clear.width, self.layout.bpm_clear.height,
            button_style(clear_c), "CLR",
        ) as i32;
        if !self.bpm_clear_active { tree.set_flag(self.bpm_clear_id as u32, UIFlags::DISABLED); }
    }

    fn build_right(&mut self, tree: &mut UITree, bg: i32) {
        let save_text = self.save_text.clone();

        self.new_button_id = tree.add_button(
            bg, self.layout.new_button.x, self.layout.new_button.y,
            self.layout.new_button.width, self.layout.new_button.height,
            button_style(color::BUTTON_INACTIVE_C32), "NEW",
        ) as i32;

        self.open_button_id = tree.add_button(
            bg, self.layout.open_button.x, self.layout.open_button.y,
            self.layout.open_button.width, self.layout.open_button.height,
            button_style(color::BUTTON_INACTIVE_C32), "OPEN",
        ) as i32;

        self.open_recent_id = tree.add_button(
            bg, self.layout.open_recent.x, self.layout.open_recent.y,
            self.layout.open_recent.width, self.layout.open_recent.height,
            UIStyle {
                bg_color: color::BUTTON_INACTIVE_C32,
                hover_bg_color: BUTTON_HOVER_C,
                pressed_bg_color: color::BUTTON_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: 11,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "OPEN RECENT",
        ) as i32;

        self.save_button_id = tree.add_button(
            bg, self.layout.save_button.x, self.layout.save_button.y,
            self.layout.save_button.width, self.layout.save_button.height,
            button_style(color::BUTTON_INACTIVE_C32), &save_text,
        ) as i32;

        self.save_as_id = tree.add_button(
            bg, self.layout.save_as.x, self.layout.save_as.y,
            self.layout.save_as.width, self.layout.save_as.height,
            button_style(color::BUTTON_INACTIVE_C32), "SAVE AS",
        ) as i32;

        let export_bg = if self.export_active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        self.export_button_id = tree.add_button(
            bg, self.layout.export_button.x, self.layout.export_button.y,
            self.layout.export_button.width, self.layout.export_button.height,
            button_style(export_bg), "EXPORT",
        ) as i32;

        let hdr_bg = if self.hdr_active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        self.hdr_button_id = tree.add_button(
            bg, self.layout.hdr_button.x, self.layout.hdr_button.y,
            self.layout.hdr_button.width, self.layout.hdr_button.height,
            button_style(hdr_bg), "HDR",
        ) as i32;

        self.perc_button_id = tree.add_button(
            bg, self.layout.perc_button.x, self.layout.perc_button.y,
            self.layout.perc_button.width, self.layout.perc_button.height,
            button_style(color::BUTTON_INACTIVE_C32), "PERC",
        ) as i32;
    }

    fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.clock_authority_id { return vec![PanelAction::CycleClockAuthority]; }
        if id == self.link_button_id { return vec![PanelAction::ToggleLink]; }
        if id == self.clk_button_id { return vec![PanelAction::ToggleMidiClock]; }
        if id == self.clk_device_id { return vec![PanelAction::SelectClkDevice]; }
        if id == self.sync_button_id { return vec![PanelAction::ToggleSyncOutput]; }
        if id == self.play_button_id { return vec![PanelAction::PlayPause]; }
        if id == self.stop_button_id { return vec![PanelAction::Stop]; }
        if id == self.rec_button_id { return vec![PanelAction::Record]; }
        if id == self.bpm_field_id { return vec![PanelAction::BpmFieldClicked]; }
        if id == self.bpm_reset_id { return vec![PanelAction::ResetBpm]; }
        if id == self.bpm_clear_id { return vec![PanelAction::ClearBpm]; }
        if id == self.new_button_id { return vec![PanelAction::NewProject]; }
        if id == self.open_button_id { return vec![PanelAction::OpenProject]; }
        if id == self.open_recent_id { return vec![PanelAction::OpenRecent]; }
        if id == self.save_button_id { return vec![PanelAction::SaveProject]; }
        if id == self.save_as_id { return vec![PanelAction::SaveProjectAs]; }
        if id == self.export_button_id { return vec![PanelAction::ExportVideo]; }
        if id == self.hdr_button_id { return vec![PanelAction::ToggleHdr]; }
        if id == self.perc_button_id { return vec![PanelAction::TogglePercussion]; }
        Vec::new()
    }
}

impl Default for TransportPanel {
    fn default() -> Self { Self::new() }
}

impl Panel for TransportPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        let bar = layout.transport_bar();
        self.layout.compute(bar);

        let bg = tree.add_panel(
            -1, bar.x, bar.y, bar.width, bar.height,
            UIStyle { bg_color: color::PANEL_BG_DARK, ..UIStyle::default() },
        ) as i32;

        self.build_left(tree, bg);
        self.build_center(tree, bg);
        self.build_right(tree, bg);
    }

    fn update(&mut self, _tree: &mut UITree) {}

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click { node_id, .. } => self.handle_click(*node_id),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_transport() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();

        panel.build(&mut tree, &layout);

        assert!(panel.play_button_id >= 0);
        assert!(panel.stop_button_id >= 0);
        assert!(panel.rec_button_id >= 0);
        assert!(panel.bpm_field_id >= 0);
        assert!(panel.clock_authority_id >= 0);
        assert!(panel.link_button_id >= 0);
        assert!(panel.new_button_id >= 0);
        assert!(panel.save_button_id >= 0);
        assert!(panel.export_button_id >= 0);
        assert!(tree.count() >= 27); // bg + 11 left + 7 center + 8 right = 27
    }

    #[test]
    fn handle_click_play() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let actions = panel.handle_click(panel.play_button_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::PlayPause));
    }

    #[test]
    fn handle_click_miss() {
        let panel = TransportPanel::new();
        let actions = panel.handle_click(9999);
        assert!(actions.is_empty());
    }

    #[test]
    fn set_play_state_updates_tree() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        tree.clear_dirty();
        panel.set_play_state(&mut tree, "PAUSE", color::PAUSED_YELLOW);

        assert!(tree.has_dirty());
        assert_eq!(panel.play_text, "PAUSE");
        assert_eq!(
            tree.get_node(panel.play_button_id as u32).text.as_deref(),
            Some("PAUSE")
        );
    }
}
