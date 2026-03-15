use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants (from HeaderLayout.cs) ────────────────────────

const INSET: f32 = 8.0;
const GROUP_Y_PAD: f32 = 5.0;
const GROUP_SPACING: f32 = 5.0;

const PROJECT_NAME_W: f32 = 200.0;
const SPACER: f32 = 8.0;
const IMPORT_STATUS_W: f32 = 180.0;
const PROGRESS_BAR_W: f32 = 140.0;
const PROGRESS_BAR_H: f32 = 10.0;
const PROGRESS_BAR_INSET: f32 = 5.0;

const ZOOM_BUTTON_W: f32 = 28.0;
const ZOOM_LABEL_W: f32 = 70.0;
const MONITOR_BUTTON_W: f32 = 60.0;

const TIME_DISPLAY_W: f32 = 260.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_DIM: Color32 = Color32::new(71, 71, 74, 255);
const BUTTON_HOVER_H: Color32 = Color32::new(90, 90, 94, 255);
const BUTTON_PRESSED_H: Color32 = Color32::new(55, 55, 58, 255);
const BUTTON_ACTIVE: Color32 = Color32::new(89, 173, 232, 255);
const BUTTON_ACTIVE_HOVER: Color32 = Color32::new(110, 190, 240, 255);
const BUTTON_ACTIVE_PRESSED: Color32 = Color32::new(70, 150, 210, 255);
const PROGRESS_FILL: Color32 = Color32::new(89, 173, 232, 255);

const PROGRESS_RADIUS: f32 = 2.0;

// ── HeaderLayout ───────────────────────────────────────────────────

#[derive(Default)]
struct HeaderLayout {
    project_name: Rect,
    import_status: Rect,
    progress_bg: Rect,
    progress_fill: Rect,
    time_display: Rect,
    zoom_out: Rect,
    zoom_label: Rect,
    zoom_in: Rect,
    monitor_button: Rect,
}

impl HeaderLayout {
    fn compute(&mut self, bounds: Rect, import_progress: f32) {
        let elem_h = bounds.height - GROUP_Y_PAD * 2.0;
        let elem_y = bounds.y + GROUP_Y_PAD;

        // Left group
        let mut lx = bounds.x + INSET;

        self.project_name = Rect::new(lx, elem_y, PROJECT_NAME_W, elem_h);
        lx += PROJECT_NAME_W + SPACER;

        self.import_status = Rect::new(lx, elem_y, IMPORT_STATUS_W, elem_h);
        lx += IMPORT_STATUS_W;

        let prog_x = lx + PROGRESS_BAR_INSET;
        let prog_y = elem_y + (elem_h - PROGRESS_BAR_H) * 0.5;
        self.progress_bg = Rect::new(prog_x, prog_y, PROGRESS_BAR_W, PROGRESS_BAR_H);

        let fill_inset = 1.0;
        let max_fill_w = PROGRESS_BAR_W - fill_inset * 2.0;
        self.progress_fill = Rect::new(
            prog_x + fill_inset,
            prog_y + fill_inset,
            max_fill_w * import_progress.clamp(0.0, 1.0),
            PROGRESS_BAR_H - fill_inset * 2.0,
        );

        // Center group
        let cx = bounds.x + (bounds.width - TIME_DISPLAY_W) * 0.5;
        self.time_display = Rect::new(cx, elem_y, TIME_DISPLAY_W, elem_h);

        // Right group (right-to-left)
        let mut rx = bounds.x_max() - INSET;

        rx -= MONITOR_BUTTON_W;
        self.monitor_button = Rect::new(rx, elem_y, MONITOR_BUTTON_W, elem_h);
        rx -= GROUP_SPACING;

        rx -= ZOOM_BUTTON_W;
        self.zoom_in = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);

        rx -= ZOOM_LABEL_W;
        self.zoom_label = Rect::new(rx, elem_y, ZOOM_LABEL_W, elem_h);

        rx -= ZOOM_BUTTON_W;
        self.zoom_out = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);
    }
}

// ── HeaderPanel ────────────────────────────────────────────────────

pub struct HeaderPanel {
    layout: HeaderLayout,

    // Node IDs
    project_name_id: i32,
    import_status_id: i32,
    progress_bg_id: i32,
    progress_fill_id: i32,
    time_display_id: i32,
    zoom_label_id: i32,
    zoom_out_id: i32,
    zoom_in_id: i32,
    monitor_btn_id: i32,

    // State
    project_name: String,
    import_status: String,
    import_progress: f32,
    import_progress_visible: bool,
    time_display: String,
    zoom_label: String,
    monitor_active: bool,
}

impl HeaderPanel {
    pub fn new() -> Self {
        Self {
            layout: HeaderLayout::default(),
            project_name_id: -1,
            import_status_id: -1,
            progress_bg_id: -1,
            progress_fill_id: -1,
            time_display_id: -1,
            zoom_label_id: -1,
            zoom_out_id: -1,
            zoom_in_id: -1,
            monitor_btn_id: -1,
            project_name: "My Project".into(),
            import_status: String::new(),
            import_progress: 0.0,
            import_progress_visible: false,
            time_display: "00:00.00 / 00:00.00  |  1.1.1".into(),
            zoom_label: "120 px/beat".into(),
            monitor_active: false,
        }
    }

    // ── Push-based setters ─────────────────────────────────────────

    pub fn set_project_name(&mut self, tree: &mut UITree, name: &str) {
        self.project_name = name.into();
        if self.project_name_id >= 0 { tree.set_text(self.project_name_id as u32, name); }
    }

    pub fn set_import_status(&mut self, tree: &mut UITree, status: &str, progress: f32, show: bool) {
        self.import_status = status.into();
        self.import_progress = progress.clamp(0.0, 1.0);
        self.import_progress_visible = show;
        if self.import_status_id >= 0 { tree.set_text(self.import_status_id as u32, status); }
        if self.progress_bg_id >= 0 { tree.set_visible(self.progress_bg_id as u32, show); }
        if self.progress_fill_id >= 0 {
            tree.set_visible(self.progress_fill_id as u32, show);
            let bg = self.layout.progress_bg;
            let fill_inset = 1.0;
            let max_fill_w = bg.width - fill_inset * 2.0;
            tree.set_bounds(self.progress_fill_id as u32, Rect::new(
                bg.x + fill_inset, bg.y + fill_inset,
                max_fill_w * self.import_progress,
                bg.height - fill_inset * 2.0,
            ));
        }
    }

    pub fn set_time_display(&mut self, tree: &mut UITree, text: &str) {
        self.time_display = text.into();
        if self.time_display_id >= 0 { tree.set_text(self.time_display_id as u32, text); }
    }

    pub fn set_zoom_label(&mut self, tree: &mut UITree, text: &str) {
        self.zoom_label = text.into();
        if self.zoom_label_id >= 0 { tree.set_text(self.zoom_label_id as u32, text); }
    }

    pub fn set_monitor_active(&mut self, tree: &mut UITree, active: bool) {
        self.monitor_active = active;
        if self.monitor_btn_id >= 0 { tree.set_style(self.monitor_btn_id as u32, self.monitor_style()); }
    }

    fn monitor_style(&self) -> UIStyle {
        if self.monitor_active {
            UIStyle {
                bg_color: BUTTON_ACTIVE,
                hover_bg_color: BUTTON_ACTIVE_HOVER,
                pressed_bg_color: BUTTON_ACTIVE_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_HEADING,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        } else {
            UIStyle {
                bg_color: color::BUTTON_INACTIVE_C32,
                hover_bg_color: BUTTON_HOVER_H,
                pressed_bg_color: BUTTON_PRESSED_H,
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_HEADING,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        }
    }

    fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.zoom_out_id { return vec![PanelAction::ZoomOut]; }
        if id == self.zoom_in_id { return vec![PanelAction::ZoomIn]; }
        if id == self.monitor_btn_id { return vec![PanelAction::ToggleMonitor]; }
        Vec::new()
    }
}

impl Default for HeaderPanel {
    fn default() -> Self { Self::new() }
}

impl Panel for HeaderPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        let header = layout.header();
        self.layout.compute(header, self.import_progress);

        let project_name = self.project_name.clone();
        let import_status = self.import_status.clone();
        let time_display = self.time_display.clone();
        let zoom_label = self.zoom_label.clone();

        let bg = tree.add_panel(
            -1, header.x, header.y, header.width, header.height,
            UIStyle { bg_color: color::PANEL_BG_DARK, ..UIStyle::default() },
        ) as i32;

        // Left group
        self.project_name_id = tree.add_label(
            bg,
            self.layout.project_name.x, self.layout.project_name.y,
            self.layout.project_name.width, self.layout.project_name.height,
            &project_name,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_SUBHEADING,
                font_weight: FontWeight::Bold,
                ..UIStyle::default()
            },
        ) as i32;

        self.import_status_id = tree.add_label(
            bg,
            self.layout.import_status.x, self.layout.import_status.y,
            self.layout.import_status.width, self.layout.import_status.height,
            &import_status,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_LABEL,
                ..UIStyle::default()
            },
        ) as i32;

        self.progress_bg_id = tree.add_panel(
            bg,
            self.layout.progress_bg.x, self.layout.progress_bg.y,
            self.layout.progress_bg.width, self.layout.progress_bg.height,
            UIStyle {
                bg_color: color::SLIDER_TRACK_PRESSED_C32,
                corner_radius: PROGRESS_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.progress_bg_id as u32, self.import_progress_visible);

        self.progress_fill_id = tree.add_panel(
            bg,
            self.layout.progress_fill.x, self.layout.progress_fill.y,
            self.layout.progress_fill.width, self.layout.progress_fill.height,
            UIStyle {
                bg_color: PROGRESS_FILL,
                corner_radius: 1.0,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.progress_fill_id as u32, self.import_progress_visible);

        // Center group
        self.time_display_id = tree.add_node(
            bg, self.layout.time_display, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: color::FONT_HEADING,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            Some(&time_display), UIFlags::empty(),
        ) as i32;

        // Right group
        self.zoom_out_id = tree.add_button(
            bg,
            self.layout.zoom_out.x, self.layout.zoom_out.y,
            self.layout.zoom_out.width, self.layout.zoom_out.height,
            UIStyle {
                bg_color: BUTTON_DIM,
                hover_bg_color: BUTTON_HOVER_H,
                pressed_bg_color: BUTTON_PRESSED_H,
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_TITLE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "\u{2212}",
        ) as i32;

        self.zoom_label_id = tree.add_node(
            bg, self.layout.zoom_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: color::FONT_SUBHEADING,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            Some(&zoom_label), UIFlags::empty(),
        ) as i32;

        self.zoom_in_id = tree.add_button(
            bg,
            self.layout.zoom_in.x, self.layout.zoom_in.y,
            self.layout.zoom_in.width, self.layout.zoom_in.height,
            UIStyle {
                bg_color: BUTTON_DIM,
                hover_bg_color: BUTTON_HOVER_H,
                pressed_bg_color: BUTTON_PRESSED_H,
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_TITLE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "+",
        ) as i32;

        self.monitor_btn_id = tree.add_button(
            bg,
            self.layout.monitor_button.x, self.layout.monitor_button.y,
            self.layout.monitor_button.width, self.layout.monitor_button.height,
            self.monitor_style(),
            "OUT",
        ) as i32;
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
    fn build_header() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();

        panel.build(&mut tree, &layout);

        assert!(panel.project_name_id >= 0);
        assert!(panel.time_display_id >= 0);
        assert!(panel.zoom_out_id >= 0);
        assert!(panel.zoom_in_id >= 0);
        assert!(panel.monitor_btn_id >= 0);
        assert!(tree.count() >= 10); // bg + 9 elements
    }

    #[test]
    fn handle_click_zoom() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.zoom_in_id as u32);
        assert!(matches!(a[0], PanelAction::ZoomIn));

        let a = panel.handle_click(panel.zoom_out_id as u32);
        assert!(matches!(a[0], PanelAction::ZoomOut));
    }

    #[test]
    fn set_time_display_updates() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        tree.clear_dirty();
        panel.set_time_display(&mut tree, "01:30.50 | 4.2.3");
        assert!(tree.has_dirty());
        assert_eq!(
            tree.get_node(panel.time_display_id as u32).text.as_deref(),
            Some("01:30.50 | 4.2.3")
        );
    }
}
