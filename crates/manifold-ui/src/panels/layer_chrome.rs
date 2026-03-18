use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::PanelAction;

// ── Layout constants (from LayerChromeBitmapPanel.cs) ─────────────

const HEADER_ROW_H: f32 = 27.5;
const NAME_ROW_H: f32 = 20.0;
const SLIDER_ROW_H: f32 = 22.5;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = 2.0;
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const OPACITY_LABEL_W: f32 = 50.0;
const FONT_SIZE: u16 = 10;
const NAME_FONT_SIZE: u16 = 12;

// ── LayerChromePanel ─────────────────────────────────────────────

pub struct LayerChromePanel {
    // Node IDs
    header_label_id: i32,
    chevron_btn_id: i32,
    name_label_id: i32,
    opacity_slider: Option<SliderNodeIds>,
    divider_ids: [i32; 3],

    // State
    is_collapsed: bool,
    dragging_opacity: bool,
    show_name: bool,
    show_opacity: bool,
    cached_header_text: String,
    cached_name: String,
    cached_opacity: f32,

    // Node range
    first_node: usize,
    node_count: usize,
}

impl LayerChromePanel {
    pub fn new() -> Self {
        Self {
            header_label_id: -1,
            chevron_btn_id: -1,
            name_label_id: -1,
            opacity_slider: None,
            divider_ids: [-1; 3],
            is_collapsed: false,
            dragging_opacity: false,
            show_name: true,
            show_opacity: true,
            cached_header_text: "Layer".into(),
            cached_name: String::new(),
            cached_opacity: 1.0,
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn compute_height(&self) -> f32 {
        if self.is_collapsed {
            return PAD_V + HEADER_ROW_H + PAD_V;
        }
        let mut h = PAD_V + HEADER_ROW_H;
        if self.show_name {
            h += DIVIDER_H + NAME_ROW_H;
        }
        if self.show_opacity {
            h += DIVIDER_H + SLIDER_ROW_H;
        }
        h += DIVIDER_H + PAD_V;
        h
    }

    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn is_dragging(&self) -> bool { self.dragging_opacity }
    pub fn is_collapsed(&self) -> bool { self.is_collapsed }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    /// Returns true if visibility changed (caller should rebuild).
    pub fn set_visibility(&mut self, show_name: bool, show_opacity: bool) -> bool {
        if self.show_name == show_name && self.show_opacity == show_opacity {
            return false;
        }
        self.show_name = show_name;
        self.show_opacity = show_opacity;
        true
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let mut cy = rect.y + PAD_V;

        let header_text = self.cached_header_text.clone();
        let name = self.cached_name.clone();
        let opacity = self.cached_opacity;

        // Header row
        let label_w = content_w - CHEVRON_W - GAP;
        self.header_label_id = tree.add_label(
            -1, cx, cy, label_w, HEADER_ROW_H,
            &header_text,
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

        // Name row (optional)
        if self.show_name {
            self.divider_ids[div_idx] = tree.add_panel(
                -1, cx, cy, content_w, DIVIDER_H,
                UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
            ) as i32;
            div_idx += 1;
            cy += DIVIDER_H;

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
        } else {
            self.name_label_id = -1;
        }

        // Opacity slider (optional)
        if self.show_opacity {
            self.divider_ids[div_idx] = tree.add_panel(
                -1, cx, cy, content_w, DIVIDER_H,
                UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
            ) as i32;
            div_idx += 1;
            cy += DIVIDER_H;

            let slider_rect = Rect::new(cx, cy, content_w, SLIDER_ROW_H);
            let val_text = format!("{:.2}", opacity);
            self.opacity_slider = Some(BitmapSlider::build(
                tree, -1, slider_rect,
                Some("Opacity"), opacity,
                &val_text, &SliderColors::default_slider(),
                FONT_SIZE, OPACITY_LABEL_W,
            ));
            cy += SLIDER_ROW_H;
        } else {
            self.opacity_slider = None;
        }

        // Final divider
        self.divider_ids[div_idx] = tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        ) as i32;

        self.node_count = tree.count() - self.first_node;
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_header_text(&mut self, tree: &mut UITree, text: &str) {
        self.cached_header_text = text.into();
        if self.header_label_id >= 0 {
            tree.set_text(self.header_label_id as u32, text);
        }
    }

    pub fn sync_name(&mut self, tree: &mut UITree, name: &str) {
        self.cached_name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    pub fn sync_opacity(&mut self, tree: &mut UITree, value: f32) {
        self.cached_opacity = value;
        if let Some(ref ids) = self.opacity_slider {
            let text = format!("{:.2}", value);
            BitmapSlider::update_value(tree, ids, value, &text);
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
            return vec![PanelAction::LayerChromeCollapseToggle];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        if let Some(ref ids) = self.opacity_slider {
            if node_id == ids.track {
                self.dragging_opacity = true;
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                return vec![
                    PanelAction::LayerOpacitySnapshot,
                    PanelAction::LayerOpacityChanged(norm),
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
                return vec![PanelAction::LayerOpacityChanged(norm)];
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
            return vec![PanelAction::LayerOpacityCommit];
        }
        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        if let Some(ref ids) = self.opacity_slider {
            if node_id == ids.track {
                return vec![PanelAction::LayerOpacityRightClick];
            }
        }
        Vec::new()
    }
}

impl Default for LayerChromePanel {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    #[test]
    fn build_layer_chrome() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.header_label_id >= 0);
        assert!(panel.chevron_btn_id >= 0);
        assert!(panel.name_label_id >= 0);
        assert!(panel.opacity_slider.is_some());
        assert!(panel.node_count > 0);
    }

    #[test]
    fn visibility_hides_rows() {
        let mut panel = LayerChromePanel::new();

        let full_h = panel.compute_height();
        panel.set_visibility(false, false);
        let minimal_h = panel.compute_height();

        assert!(minimal_h < full_h);
    }

    #[test]
    fn set_visibility_returns_changed() {
        let mut panel = LayerChromePanel::new();
        assert!(!panel.set_visibility(true, true)); // no change
        assert!(panel.set_visibility(false, true));  // changed
        assert!(!panel.set_visibility(false, true)); // no change
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::LayerChromeCollapseToggle));
    }

    #[test]
    fn sync_name_updates() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        tree.clear_dirty();
        panel.sync_name(&mut tree, "Drums Layer");
        assert!(tree.has_dirty());
        assert_eq!(
            tree.get_node(panel.name_label_id as u32).text.as_deref(),
            Some("Drums Layer"),
        );
    }
}
