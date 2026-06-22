use super::{Panel, PanelAction};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants (from TransportLayout.cs) ─────────────────────

const INSET: f32 = 8.0;
const GROUP_Y_PAD: f32 = 4.0;
const ITEM_SPACING: f32 = 5.0;
const SECTION_SPACER: f32 = 8.0;
const RIGHT_SPACING: f32 = 4.0;
const CENTER_SPACER: f32 = 12.0;

// Thin separators dropped into the existing section gaps so the bar reads as
// clustered groups (sources | transport | tempo | file | render) instead of one
// undifferentiated run of buttons.
const DIVIDER_W: f32 = 1.0;
const DIVIDER_V_INSET: f32 = 7.0;

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
const FRAME_BUTTON_W: f32 = 48.0;
const HDR_BUTTON_W: f32 = 35.0;
const PERC_BUTTON_W: f32 = 48.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_HOVER_C: Color32 = color::TRANSPORT_BUTTON_HOVER;
const SAVE_DIRTY_BG: Color32 = color::TRANSPORT_SAVE_DIRTY_BG;
const BPM_FIELD_HOVER: Color32 = color::TRANSPORT_BPM_FIELD_HOVER;

const BUTTON_FONT: u16 = color::FONT_SUBHEADING;
const STATUS_FONT: u16 = color::FONT_BODY;
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
    let hover = if is_active {
        lighten(bg, 30)
    } else {
        BUTTON_HOVER_C
    };
    let pressed = if is_active {
        darken(bg, 20)
    } else {
        color::BUTTON_PRESSED
    };
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
    clock_authority: Rect,
    link_button: Rect,
    link_dot: Rect,
    link_status: Rect,
    clk_button: Rect,
    clk_device: Rect,
    clk_dot: Rect,
    clk_status: Rect,
    sync_button: Rect,
    sync_dot: Rect,
    sync_status: Rect,
    play_button: Rect,
    stop_button: Rect,
    rec_button: Rect,
    bpm_label: Rect,
    bpm_field: Rect,
    bpm_reset: Rect,
    bpm_clear: Rect,
    new_button: Rect,
    open_button: Rect,
    open_recent: Rect,
    save_button: Rect,
    save_as: Rect,
    export_button: Rect,
    frame_button: Rect,
    hdr_button: Rect,
    perc_button: Rect,
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
        self.frame_button = Rect::new(x, ey, FRAME_BUTTON_W, eh);
        x += FRAME_BUTTON_W + RIGHT_SPACING;
        self.hdr_button = Rect::new(x, ey, HDR_BUTTON_W, eh);
        x += HDR_BUTTON_W + RIGHT_SPACING;
        self.perc_button = Rect::new(x, ey, PERC_BUTTON_W, eh);
    }

    fn left_width() -> f32 {
        CLOCK_AUTHORITY_W
            + ITEM_SPACING
            + SECTION_SPACER
            + ITEM_SPACING
            + LINK_BUTTON_W
            + ITEM_SPACING
            + STATUS_DOT_SIZE
            + ITEM_SPACING
            + STATUS_TEXT_W
            + ITEM_SPACING
            + SECTION_SPACER
            + ITEM_SPACING
            + CLK_BUTTON_W
            + ITEM_SPACING
            + CLK_DEVICE_W
            + ITEM_SPACING
            + STATUS_DOT_SIZE
            + ITEM_SPACING
            + STATUS_TEXT_W
            + ITEM_SPACING
            + SECTION_SPACER
            + ITEM_SPACING
            + SYNC_BUTTON_W
            + ITEM_SPACING
            + STATUS_DOT_SIZE
            + ITEM_SPACING
            + STATUS_TEXT_W
    }

    fn center_width() -> f32 {
        PLAY_BUTTON_W
            + ITEM_SPACING
            + STOP_BUTTON_W
            + ITEM_SPACING
            + REC_BUTTON_W
            + ITEM_SPACING
            + CENTER_SPACER
            + ITEM_SPACING
            + BPM_LABEL_W
            + ITEM_SPACING
            + BPM_FIELD_W
            + ITEM_SPACING
            + BPM_RESET_W
            + ITEM_SPACING
            + BPM_CLEAR_W
    }

    fn right_width() -> f32 {
        NEW_BUTTON_W
            + RIGHT_SPACING
            + OPEN_BUTTON_W
            + RIGHT_SPACING
            + OPEN_RECENT_W
            + RIGHT_SPACING
            + SAVE_BUTTON_W
            + RIGHT_SPACING
            + SAVE_AS_W
            + RIGHT_SPACING
            + SECTION_SPACER
            + EXPORT_BUTTON_W
            + RIGHT_SPACING
            + FRAME_BUTTON_W
            + RIGHT_SPACING
            + HDR_BUTTON_W
            + RIGHT_SPACING
            + PERC_BUTTON_W
    }
}

// ── TransportPanel ─────────────────────────────────────────────────

pub struct TransportPanel {
    layout: TransportLayout,

    // Node IDs (None = unset)
    clock_authority_id: Option<NodeId>,
    link_button_id: Option<NodeId>,
    link_dot_id: Option<NodeId>,
    link_status_id: Option<NodeId>,
    clk_button_id: Option<NodeId>,
    clk_device_id: Option<NodeId>,
    clk_dot_id: Option<NodeId>,
    clk_status_id: Option<NodeId>,
    sync_button_id: Option<NodeId>,
    sync_dot_id: Option<NodeId>,
    sync_status_id: Option<NodeId>,
    play_button_id: Option<NodeId>,
    stop_button_id: Option<NodeId>,
    rec_button_id: Option<NodeId>,
    bpm_label_id: Option<NodeId>,
    bpm_field_id: Option<NodeId>,
    bpm_reset_id: Option<NodeId>,
    bpm_clear_id: Option<NodeId>,
    new_button_id: Option<NodeId>,
    open_button_id: Option<NodeId>,
    open_recent_id: Option<NodeId>,
    save_button_id: Option<NodeId>,
    save_as_id: Option<NodeId>,
    export_button_id: Option<NodeId>,
    frame_button_id: Option<NodeId>,
    hdr_button_id: Option<NodeId>,
    perc_button_id: Option<NodeId>,

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
    perc_active: bool,

    // Cache tracking
    cache_first_node: usize,
    cache_node_count: usize,
}

impl TransportPanel {
    pub fn new() -> Self {
        Self {
            layout: TransportLayout::default(),
            clock_authority_id: None,
            link_button_id: None,
            link_dot_id: None,
            link_status_id: None,
            clk_button_id: None,
            clk_device_id: None,
            clk_dot_id: None,
            clk_status_id: None,
            sync_button_id: None,
            sync_dot_id: None,
            sync_status_id: None,
            play_button_id: None,
            stop_button_id: None,
            rec_button_id: None,
            bpm_label_id: None,
            bpm_field_id: None,
            bpm_reset_id: None,
            bpm_clear_id: None,
            new_button_id: None,
            open_button_id: None,
            open_recent_id: None,
            save_button_id: None,
            save_as_id: None,
            export_button_id: None,
            frame_button_id: None,
            hdr_button_id: None,
            perc_button_id: None,
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
            perc_active: false,
            cache_first_node: usize::MAX,
            cache_node_count: 0,
        }
    }

    // ── Public accessors ───────────────────────────────────────────

    pub fn bpm_field_id(&self) -> Option<NodeId> {
        self.bpm_field_id
    }
    pub fn clock_authority_node_id(&self) -> Option<NodeId> {
        self.clock_authority_id
    }
    pub fn clk_device_node_id(&self) -> Option<NodeId> {
        self.clk_device_id
    }

    /// Get bounds of any node owned by this panel.
    pub fn get_node_bounds(&self, tree: &UITree, node_id: Option<NodeId>) -> Rect {
        match node_id {
            Some(id) => tree.get_bounds(id),
            None => Rect::ZERO,
        }
    }

    // ── Push-based setters ─────────────────────────────────────────

    pub fn set_clock_authority(&mut self, tree: &mut UITree, text: &str, c: Color32) {
        self.clock_authority_text = text.into();
        self.clock_authority_color = c;
        if let Some(id) = self.clock_authority_id {
            tree.set_text(id, text);
            tree.set_style(id, button_style(c));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_link_state(
        &mut self,
        tree: &mut UITree,
        enabled: bool,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.link_enabled = enabled;
        self.link_dot_color = dot_color;
        self.link_status_text = status.into();
        self.link_status_color = status_color;
        if let Some(id) = self.link_button_id {
            let bg = if enabled {
                color::LINK_ORANGE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(bg));
        }
        if let Some(id) = self.link_dot_id {
            tree.set_style(id, dot_style(dot_color));
        }
        if let Some(id) = self.link_status_id {
            tree.set_text(id, status);
            tree.set_style(id, status_text_style(status_color));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_clk_state(
        &mut self,
        tree: &mut UITree,
        enabled: bool,
        device_text: &str,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.clk_enabled = enabled;
        self.clk_device_text = device_text.into();
        self.clk_dot_color = dot_color;
        self.clk_status_text = status.into();
        self.clk_status_color = status_color;
        if let Some(id) = self.clk_button_id {
            let bg = if enabled {
                color::MIDI_PURPLE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(bg));
        }
        if let Some(id) = self.clk_device_id {
            tree.set_text(id, device_text);
        }
        if let Some(id) = self.clk_dot_id {
            tree.set_style(id, dot_style(dot_color));
        }
        if let Some(id) = self.clk_status_id {
            tree.set_text(id, status);
            tree.set_style(id, status_text_style(status_color));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_sync_state(
        &mut self,
        tree: &mut UITree,
        enabled: bool,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.sync_enabled = enabled;
        self.sync_dot_color = dot_color;
        self.sync_status_text = status.into();
        self.sync_status_color = status_color;
        if let Some(id) = self.sync_button_id {
            let bg = if enabled {
                color::SYNC_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(bg));
        }
        if let Some(id) = self.sync_dot_id {
            tree.set_style(id, dot_style(dot_color));
        }
        if let Some(id) = self.sync_status_id {
            tree.set_text(id, status);
            tree.set_style(id, status_text_style(status_color));
        }
    }

    pub fn set_play_state(&mut self, tree: &mut UITree, text: &str, c: Color32) {
        self.play_text = text.into();
        self.play_color = c;
        if let Some(id) = self.play_button_id {
            tree.set_text(id, text);
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_record_state(&mut self, tree: &mut UITree, active: bool, enabled: bool) {
        self.rec_active = active;
        self.rec_enabled = enabled;
        if let Some(id) = self.rec_button_id {
            let c = if active {
                color::RECORD_ACTIVE
            } else {
                color::RECORD_RED
            };
            tree.set_style(id, button_style(c));
            if enabled {
                tree.clear_flag(id, UIFlags::DISABLED);
            } else {
                tree.set_flag(id, UIFlags::DISABLED);
            }
        }
    }

    pub fn set_bpm_text(&mut self, tree: &mut UITree, text: &str) {
        self.bpm_text = text.into();
        if let Some(id) = self.bpm_field_id {
            tree.set_text(id, text);
        }
    }

    pub fn set_bpm_reset_active(&mut self, tree: &mut UITree, active: bool) {
        self.bpm_reset_active = active;
        if let Some(id) = self.bpm_reset_id {
            let c = if active {
                color::BPM_RESET_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
            if active {
                tree.clear_flag(id, UIFlags::DISABLED);
            } else {
                tree.set_flag(id, UIFlags::DISABLED);
            }
        }
    }

    pub fn set_bpm_clear_active(&mut self, tree: &mut UITree, active: bool) {
        self.bpm_clear_active = active;
        if let Some(id) = self.bpm_clear_id {
            let c = if active {
                color::BPM_CLEAR_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
            if active {
                tree.clear_flag(id, UIFlags::DISABLED);
            } else {
                tree.set_flag(id, UIFlags::DISABLED);
            }
        }
    }

    pub fn set_save_text(&mut self, tree: &mut UITree, text: &str) {
        self.save_text = text.into();
        if let Some(id) = self.save_button_id {
            tree.set_text(id, text);
            let dirty = text.contains('*');
            let c = if dirty {
                SAVE_DIRTY_BG
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_export_label(&mut self, _tree: &mut UITree, _text: &str) {
        // Label display handled by viewport markers, not transport button text.
    }

    pub fn set_export_active(&mut self, tree: &mut UITree, active: bool) {
        self.export_active = active;
        if let Some(id) = self.export_button_id {
            let c = if active {
                color::SYNC_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_hdr_active(&mut self, tree: &mut UITree, active: bool) {
        self.hdr_active = active;
        if let Some(id) = self.hdr_button_id {
            let c = if active {
                color::SYNC_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
        }
    }

    pub fn set_perc_active(&mut self, tree: &mut UITree, active: bool) {
        self.perc_active = active;
        if let Some(id) = self.perc_button_id {
            let c = if active {
                color::SYNC_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            };
            tree.set_style(id, button_style(c));
        }
    }

    // ── Build helpers ──────────────────────────────────────────────

    fn build_left(&mut self, tree: &mut UITree, bg: NodeId) {
        // Clone strings to avoid borrow conflicts
        let clock_text = self.clock_authority_text.clone();
        let link_status = self.link_status_text.clone();
        let clk_device = self.clk_device_text.clone();
        let clk_status = self.clk_status_text.clone();
        let sync_status = self.sync_status_text.clone();

        self.clock_authority_id = Some(tree.add_button(
            Some(bg),
            self.layout.clock_authority.x,
            self.layout.clock_authority.y,
            self.layout.clock_authority.width,
            self.layout.clock_authority.height,
            button_style(self.clock_authority_color),
            &clock_text,
        ));

        let link_bg = if self.link_enabled {
            color::LINK_ORANGE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        self.link_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.link_button.x,
            self.layout.link_button.y,
            self.layout.link_button.width,
            self.layout.link_button.height,
            button_style(link_bg),
            "LINK",
        ));

        self.link_dot_id = Some(tree.add_panel(
            Some(bg),
            self.layout.link_dot.x,
            self.layout.link_dot.y,
            self.layout.link_dot.width,
            self.layout.link_dot.height,
            dot_style(self.link_dot_color),
        ));

        self.link_status_id = Some(tree.add_node(
            Some(bg),
            self.layout.link_status,
            UINodeType::Label,
            status_text_style(self.link_status_color),
            Some(&link_status),
            UIFlags::empty(),
        ));

        let clk_bg = if self.clk_enabled {
            color::MIDI_PURPLE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        self.clk_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.clk_button.x,
            self.layout.clk_button.y,
            self.layout.clk_button.width,
            self.layout.clk_button.height,
            button_style(clk_bg),
            "CLK",
        ));

        self.clk_device_id = Some(tree.add_button(
            Some(bg),
            self.layout.clk_device.x,
            self.layout.clk_device.y,
            self.layout.clk_device.width,
            self.layout.clk_device.height,
            button_style(color::BUTTON_INACTIVE_C32),
            &clk_device,
        ));

        self.clk_dot_id = Some(tree.add_panel(
            Some(bg),
            self.layout.clk_dot.x,
            self.layout.clk_dot.y,
            self.layout.clk_dot.width,
            self.layout.clk_dot.height,
            dot_style(self.clk_dot_color),
        ));

        self.clk_status_id = Some(tree.add_node(
            Some(bg),
            self.layout.clk_status,
            UINodeType::Label,
            status_text_style(self.clk_status_color),
            Some(&clk_status),
            UIFlags::empty(),
        ));

        let sync_bg = if self.sync_enabled {
            color::SYNC_ACTIVE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        self.sync_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.sync_button.x,
            self.layout.sync_button.y,
            self.layout.sync_button.width,
            self.layout.sync_button.height,
            button_style(sync_bg),
            "SYNC",
        ));

        self.sync_dot_id = Some(tree.add_panel(
            Some(bg),
            self.layout.sync_dot.x,
            self.layout.sync_dot.y,
            self.layout.sync_dot.width,
            self.layout.sync_dot.height,
            dot_style(self.sync_dot_color),
        ));

        self.sync_status_id = Some(tree.add_node(
            Some(bg),
            self.layout.sync_status,
            UINodeType::Label,
            status_text_style(self.sync_status_color),
            Some(&sync_status),
            UIFlags::empty(),
        ));
    }

    fn build_center(&mut self, tree: &mut UITree, bg: NodeId) {
        let play_text = self.play_text.clone();
        let bpm_text = self.bpm_text.clone();

        self.play_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.play_button.x,
            self.layout.play_button.y,
            self.layout.play_button.width,
            self.layout.play_button.height,
            button_style(self.play_color),
            &play_text,
        ));

        self.stop_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.stop_button.x,
            self.layout.stop_button.y,
            self.layout.stop_button.width,
            self.layout.stop_button.height,
            button_style(self.stop_color),
            "STOP",
        ));

        let rec_c = if self.rec_active {
            color::RECORD_ACTIVE
        } else {
            color::RECORD_RED
        };
        let rec_button_id = tree.add_button(
            Some(bg),
            self.layout.rec_button.x,
            self.layout.rec_button.y,
            self.layout.rec_button.width,
            self.layout.rec_button.height,
            button_style(rec_c),
            "REC",
        );
        self.rec_button_id = Some(rec_button_id);
        if !self.rec_enabled {
            tree.set_flag(rec_button_id, UIFlags::DISABLED);
        }

        self.bpm_label_id = Some(tree.add_node(
            Some(bg),
            self.layout.bpm_label,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: STATUS_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("BPM"),
            UIFlags::empty(),
        ));

        self.bpm_field_id = Some(tree.add_button(
            Some(bg),
            self.layout.bpm_field.x,
            self.layout.bpm_field.y,
            self.layout.bpm_field.width,
            self.layout.bpm_field.height,
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
        ));

        let reset_c = if self.bpm_reset_active {
            color::BPM_RESET_ACTIVE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        let bpm_reset_id = tree.add_button(
            Some(bg),
            self.layout.bpm_reset.x,
            self.layout.bpm_reset.y,
            self.layout.bpm_reset.width,
            self.layout.bpm_reset.height,
            button_style(reset_c),
            "R",
        );
        self.bpm_reset_id = Some(bpm_reset_id);
        if !self.bpm_reset_active {
            tree.set_flag(bpm_reset_id, UIFlags::DISABLED);
        }

        let clear_c = if self.bpm_clear_active {
            color::BPM_CLEAR_ACTIVE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        let bpm_clear_id = tree.add_button(
            Some(bg),
            self.layout.bpm_clear.x,
            self.layout.bpm_clear.y,
            self.layout.bpm_clear.width,
            self.layout.bpm_clear.height,
            button_style(clear_c),
            "CLR",
        );
        self.bpm_clear_id = Some(bpm_clear_id);
        if !self.bpm_clear_active {
            tree.set_flag(bpm_clear_id, UIFlags::DISABLED);
        }
    }

    fn build_right(&mut self, tree: &mut UITree, bg: NodeId) {
        let save_text = self.save_text.clone();

        self.new_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.new_button.x,
            self.layout.new_button.y,
            self.layout.new_button.width,
            self.layout.new_button.height,
            button_style(color::BUTTON_INACTIVE_C32),
            "NEW",
        ));

        self.open_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.open_button.x,
            self.layout.open_button.y,
            self.layout.open_button.width,
            self.layout.open_button.height,
            button_style(color::BUTTON_INACTIVE_C32),
            "OPEN",
        ));

        self.open_recent_id = Some(tree.add_button(
            Some(bg),
            self.layout.open_recent.x,
            self.layout.open_recent.y,
            self.layout.open_recent.width,
            self.layout.open_recent.height,
            UIStyle {
                bg_color: color::BUTTON_INACTIVE_C32,
                hover_bg_color: BUTTON_HOVER_C,
                pressed_bg_color: color::BUTTON_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_LABEL,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "OPEN RECENT",
        ));

        self.save_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.save_button.x,
            self.layout.save_button.y,
            self.layout.save_button.width,
            self.layout.save_button.height,
            button_style(color::BUTTON_INACTIVE_C32),
            &save_text,
        ));

        self.save_as_id = Some(tree.add_button(
            Some(bg),
            self.layout.save_as.x,
            self.layout.save_as.y,
            self.layout.save_as.width,
            self.layout.save_as.height,
            button_style(color::BUTTON_INACTIVE_C32),
            "SAVE AS",
        ));

        let export_bg = if self.export_active {
            color::SYNC_ACTIVE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        self.export_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.export_button.x,
            self.layout.export_button.y,
            self.layout.export_button.width,
            self.layout.export_button.height,
            button_style(export_bg),
            "EXPORT",
        ));

        self.frame_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.frame_button.x,
            self.layout.frame_button.y,
            self.layout.frame_button.width,
            self.layout.frame_button.height,
            button_style(color::BUTTON_INACTIVE_C32),
            "FRAME",
        ));

        let hdr_bg = if self.hdr_active {
            color::SYNC_ACTIVE
        } else {
            color::BUTTON_INACTIVE_C32
        };
        self.hdr_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.hdr_button.x,
            self.layout.hdr_button.y,
            self.layout.hdr_button.width,
            self.layout.hdr_button.height,
            button_style(hdr_bg),
            "HDR",
        ));

        self.perc_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.perc_button.x,
            self.layout.perc_button.y,
            self.layout.perc_button.width,
            self.layout.perc_button.height,
            button_style(color::BUTTON_INACTIVE_C32),
            "PERC",
        ));
    }

    /// Drop thin vertical dividers into the section gaps the layout already
    /// leaves, so the eye groups the bar into sources / transport / tempo /
    /// file / render. Non-interactive panels — they never intercept clicks.
    fn build_dividers(&mut self, tree: &mut UITree, bg: NodeId, bar: Rect) {
        let y = bar.y + DIVIDER_V_INSET;
        let h = (bar.height - DIVIDER_V_INSET * 2.0).max(1.0);
        let l = &self.layout;
        let mids = [
            (l.clock_authority.x_max() + l.link_button.x) * 0.5, // source | link
            (l.link_status.x_max() + l.clk_button.x) * 0.5,      // link | clk
            (l.clk_status.x_max() + l.sync_button.x) * 0.5,      // clk | sync
            (l.rec_button.x_max() + l.bpm_label.x) * 0.5,        // transport | tempo
            (l.save_as.x_max() + l.export_button.x) * 0.5,       // file | render
        ];
        for mx in mids {
            tree.add_panel(
                Some(bg),
                mx - DIVIDER_W * 0.5,
                y,
                DIVIDER_W,
                h,
                UIStyle {
                    bg_color: color::DIVIDER_COLOR,
                    ..UIStyle::default()
                },
            );
        }
    }

    fn handle_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        let id = Some(node_id);
        // clock_authority_id is read-only — authority is auto-determined from enabled sources
        if id == self.link_button_id {
            return vec![PanelAction::ToggleLink];
        }
        if id == self.clk_button_id {
            return vec![PanelAction::ToggleMidiClock];
        }
        if id == self.clk_device_id {
            return vec![PanelAction::SelectClkDevice];
        }
        if id == self.sync_button_id {
            return vec![PanelAction::ToggleSyncOutput];
        }
        if id == self.play_button_id {
            return vec![PanelAction::PlayPause];
        }
        if id == self.stop_button_id {
            return vec![PanelAction::Stop];
        }
        if id == self.rec_button_id {
            return vec![PanelAction::Record];
        }
        if id == self.bpm_field_id {
            return vec![PanelAction::BpmFieldClicked];
        }
        if id == self.bpm_reset_id {
            return vec![PanelAction::ResetBpm];
        }
        if id == self.bpm_clear_id {
            return vec![PanelAction::ClearBpm];
        }
        if id == self.new_button_id {
            return vec![PanelAction::NewProject];
        }
        if id == self.open_button_id {
            return vec![PanelAction::OpenProject];
        }
        if id == self.open_recent_id {
            return vec![PanelAction::OpenRecent];
        }
        if id == self.save_button_id {
            return vec![PanelAction::SaveProject];
        }
        if id == self.save_as_id {
            return vec![PanelAction::SaveProjectAs];
        }
        if id == self.export_button_id {
            return vec![PanelAction::ExportVideo];
        }
        if id == self.frame_button_id {
            return vec![PanelAction::ExportFrame];
        }
        if id == self.hdr_button_id {
            return vec![PanelAction::ToggleHdr];
        }
        if id == self.perc_button_id {
            return vec![PanelAction::TogglePercussion];
        }
        Vec::new()
    }

    /// Node-intent dispatch for the transport buttons' clicks. Mirrors
    /// `handle_click` — each button id maps to its action. See
    /// `docs/NODE_INTENT_DISPATCH.md`.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::intent::Gesture::Click;
        let mut on = |id: Option<NodeId>, a: PanelAction| {
            if let Some(id) = id {
                intents.on(id, Click, a);
            }
        };
        on(self.link_button_id, PanelAction::ToggleLink);
        on(self.clk_button_id, PanelAction::ToggleMidiClock);
        on(self.clk_device_id, PanelAction::SelectClkDevice);
        on(self.sync_button_id, PanelAction::ToggleSyncOutput);
        on(self.play_button_id, PanelAction::PlayPause);
        on(self.stop_button_id, PanelAction::Stop);
        on(self.rec_button_id, PanelAction::Record);
        on(self.bpm_field_id, PanelAction::BpmFieldClicked);
        on(self.bpm_reset_id, PanelAction::ResetBpm);
        on(self.bpm_clear_id, PanelAction::ClearBpm);
        on(self.new_button_id, PanelAction::NewProject);
        on(self.open_button_id, PanelAction::OpenProject);
        on(self.open_recent_id, PanelAction::OpenRecent);
        on(self.save_button_id, PanelAction::SaveProject);
        on(self.save_as_id, PanelAction::SaveProjectAs);
        on(self.export_button_id, PanelAction::ExportVideo);
        on(self.frame_button_id, PanelAction::ExportFrame);
        on(self.hdr_button_id, PanelAction::ToggleHdr);
        on(self.perc_button_id, PanelAction::TogglePercussion);
    }
}

impl Default for TransportPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for TransportPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.cache_first_node = tree.count();

        let bar = layout.transport_bar();
        self.layout.compute(bar);

        let bg = tree.add_panel(
            None,
            bar.x,
            bar.y,
            bar.width,
            bar.height,
            UIStyle {
                bg_color: color::PANEL_BG_DARK,
                ..UIStyle::default()
            },
        );

        self.build_left(tree, bg);
        self.build_center(tree, bg);
        self.build_right(tree, bg);
        self.build_dividers(tree, bg, bar);

        self.cache_node_count = tree.count() - self.cache_first_node;
    }

    fn update(&mut self, _tree: &mut UITree) {}

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click { node_id, .. } => self.handle_click(*node_id),
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

    #[test]
    fn build_transport() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();

        panel.build(&mut tree, &layout);

        assert!(panel.play_button_id.is_some());
        assert!(panel.stop_button_id.is_some());
        assert!(panel.rec_button_id.is_some());
        assert!(panel.bpm_field_id.is_some());
        assert!(panel.clock_authority_id.is_some());
        assert!(panel.link_button_id.is_some());
        assert!(panel.new_button_id.is_some());
        assert!(panel.save_button_id.is_some());
        assert!(panel.export_button_id.is_some());
        assert!(panel.frame_button_id.is_some());
        assert!(tree.count() >= 28); // bg + 11 left + 7 center + 9 right = 28
    }

    #[test]
    fn handle_click_play() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let actions = panel.handle_click(panel.play_button_id.unwrap());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::PlayPause));
    }

    #[test]
    fn handle_click_miss() {
        let panel = TransportPanel::new();
        let actions = panel.handle_click(NodeId(9999));
        assert!(actions.is_empty());
    }

    #[test]
    fn intent_resolves_play_button_click() {
        use crate::intent::{Gesture, IntentRegistry};
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        // The live path resolves the play button's click through the registry.
        let action = intents.resolve(&tree, panel.play_button_id, Gesture::Click);
        assert!(matches!(action, Some(PanelAction::PlayPause)));
        let stop = intents.resolve(&tree, panel.stop_button_id, Gesture::Click);
        assert!(matches!(stop, Some(PanelAction::Stop)));
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
            tree.get_node(panel.play_button_id.unwrap()).text.as_deref(),
            Some("PAUSE")
        );
    }
}
