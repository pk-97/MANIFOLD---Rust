use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::PanelAction;

// ── Layout constants (from MasterChromeBitmapPanel.cs) ───────────

const HEADER_ROW_H: f32 = 27.5;
const EXIT_PATH_ROW_H: f32 = 27.5;
const SLIDER_ROW_H: f32 = 22.5;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = 2.0;
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const EXIT_LABEL_W: f32 = 60.0;
const OPACITY_LABEL_W: f32 = 50.0;
const FONT_SIZE: u16 = 10;

// ── Panel-specific colors ────────────────────────────────────────

const EXIT_PATH_BG: Color32 = Color32::new(48, 48, 51, 255);
const EXIT_PATH_HOVER: Color32 = Color32::new(58, 58, 63, 255);
const EXIT_PATH_PRESSED: Color32 = Color32::new(40, 40, 43, 255);

// ── MasterChromePanel ────────────────────────────────────────────

pub struct MasterChromePanel {
    // Node IDs
    header_label_id: i32,
    chevron_btn_id: i32,
    exit_path_label_id: i32,
    exit_path_btn_id: i32,
    opacity_slider: Option<SliderNodeIds>,
    divider_ids: [i32; 3],

    // State
    is_collapsed: bool,
    dragging_opacity: bool,
    cached_opacity: f32,
    cached_exit_path: String,

    // Node range for ownership checking
    first_node: usize,
    node_count: usize,
}

impl MasterChromePanel {
    pub fn new() -> Self {
        Self {
            header_label_id: -1,
            chevron_btn_id: -1,
            exit_path_label_id: -1,
            exit_path_btn_id: -1,
            opacity_slider: None,
            divider_ids: [-1; 3],
            is_collapsed: false,
            dragging_opacity: false,
            cached_opacity: 1.0,
            cached_exit_path: "Default".into(),
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn compute_height(&self) -> f32 {
        if self.is_collapsed {
            PAD_V + HEADER_ROW_H + PAD_V
        } else {
            PAD_V + HEADER_ROW_H + DIVIDER_H
                + EXIT_PATH_ROW_H + DIVIDER_H
                + SLIDER_ROW_H + DIVIDER_H + PAD_V
        }
    }

    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn is_dragging(&self) -> bool { self.dragging_opacity }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let mut cy = rect.y + PAD_V;

        let opacity = self.cached_opacity;
        let exit_path = self.cached_exit_path.clone();

        // Header row
        let label_w = content_w - CHEVRON_W - GAP;
        self.header_label_id = tree.add_label(
            -1, cx, cy, label_w, HEADER_ROW_H,
            "Master FX",
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

        // Divider 0
        self.divider_ids[0] = tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        ) as i32;
        cy += DIVIDER_H;

        // Exit path row
        self.exit_path_label_id = tree.add_label(
            -1, cx, cy, EXIT_LABEL_W, EXIT_PATH_ROW_H,
            "Exit Path",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let btn_x = cx + EXIT_LABEL_W + GAP;
        let btn_w = (content_w - EXIT_LABEL_W - GAP).max(20.0);
        self.exit_path_btn_id = tree.add_button(
            -1, btn_x, cy + (EXIT_PATH_ROW_H - 18.0) * 0.5, btn_w, 18.0,
            UIStyle {
                bg_color: EXIT_PATH_BG,
                hover_bg_color: EXIT_PATH_HOVER,
                pressed_bg_color: EXIT_PATH_PRESSED,
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FONT_SIZE,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            &exit_path,
        ) as i32;
        cy += EXIT_PATH_ROW_H;

        // Divider 1
        self.divider_ids[1] = tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        ) as i32;
        cy += DIVIDER_H;

        // Opacity slider
        let slider_rect = Rect::new(cx, cy, content_w, SLIDER_ROW_H);
        let val_text = format!("{:.2}", opacity);
        self.opacity_slider = Some(BitmapSlider::build(
            tree, -1, slider_rect,
            Some("Opacity"), opacity,
            &val_text, &SliderColors::default_slider(),
            FONT_SIZE, OPACITY_LABEL_W,
        ));
        cy += SLIDER_ROW_H;

        // Divider 2
        self.divider_ids[2] = tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        ) as i32;

        self.node_count = tree.count() - self.first_node;
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_opacity(&mut self, tree: &mut UITree, value: f32) {
        self.cached_opacity = value;
        if let Some(ref ids) = self.opacity_slider {
            let text = format!("{:.2}", value);
            BitmapSlider::update_value(tree, ids, value, &text);
        }
    }

    pub fn sync_exit_path(&mut self, tree: &mut UITree, path: &str) {
        self.cached_exit_path = path.into();
        if self.exit_path_btn_id >= 0 {
            tree.set_text(self.exit_path_btn_id as u32, path);
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

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.chevron_btn_id {
            return vec![PanelAction::MasterCollapseToggle];
        }
        if id == self.exit_path_btn_id {
            return vec![PanelAction::MasterExitPathClicked];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        if let Some(ref ids) = self.opacity_slider {
            if node_id == ids.track {
                self.dragging_opacity = true;
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                return vec![
                    PanelAction::MasterOpacitySnapshot,
                    PanelAction::MasterOpacityChanged(norm),
                ];
            }
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_opacity {
            if let Some(ref ids) = self.opacity_slider {
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let text = format!("{:.2}", norm);
                BitmapSlider::update_value(tree, ids, norm, &text);
                return vec![PanelAction::MasterOpacityChanged(norm)];
            }
        }
        Vec::new()
    }

    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_opacity {
            self.dragging_opacity = false;
            if let Some(ref ids) = self.opacity_slider {
                let text = format!("{:.2}", self.cached_opacity);
                BitmapSlider::update_value(tree, ids, self.cached_opacity, &text);
            }
            return vec![PanelAction::MasterOpacityCommit];
        }
        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        if let Some(ref ids) = self.opacity_slider {
            if node_id == ids.track {
                return vec![PanelAction::MasterOpacityRightClick];
            }
        }
        Vec::new()
    }

    pub fn exit_path_button_rect(&self, tree: &UITree) -> Rect {
        if self.exit_path_btn_id >= 0 {
            tree.get_bounds(self.exit_path_btn_id as u32)
        } else {
            Rect::ZERO
        }
    }
}

impl Default for MasterChromePanel {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    #[test]
    fn build_master_chrome() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.header_label_id >= 0);
        assert!(panel.chevron_btn_id >= 0);
        assert!(panel.exit_path_btn_id >= 0);
        assert!(panel.opacity_slider.is_some());
        assert!(panel.node_count > 0);
    }

    #[test]
    fn collapsed_height_smaller() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let expanded_h = panel.compute_height();
        panel.sync_collapsed(&mut tree, true);
        let collapsed_h = panel.compute_height();

        assert!(collapsed_h < expanded_h);
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::MasterCollapseToggle));
    }

    #[test]
    fn handle_click_exit_path() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.exit_path_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::MasterExitPathClicked));
    }

    #[test]
    fn sync_opacity_updates() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        tree.clear_dirty();
        panel.sync_opacity(&mut tree, 0.5);
        assert!(tree.has_dirty());
    }

    #[test]
    fn sync_exit_path_updates() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        tree.clear_dirty();
        panel.sync_exit_path(&mut tree, "Additive");
        assert!(tree.has_dirty());
        assert_eq!(
            tree.get_node(panel.exit_path_btn_id as u32).text.as_deref(),
            Some("Additive"),
        );
    }
}
