use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants (from FooterLayout.cs) ────────────────────────

const PAD: f32 = 8.0;
const ELEM_Y_PAD: f32 = 3.0;
const LABEL_GAP: f32 = 4.0;
const SECTION_SPACER: f32 = 18.0;

const QUANTIZE_LABEL_W: f32 = 20.0;
const QUANTIZE_BUTTON_W: f32 = 44.0;
const RESOLUTION_LABEL_W: f32 = 32.0;
const RESOLUTION_BUTTON_W: f32 = 120.0;
const FPS_LABEL_W: f32 = 32.0;
const FPS_FIELD_W: f32 = 46.0;
const RIGHT_GUTTER: f32 = 10.0;

// ── Panel-specific colors ──────────────────────────────────────────

const FOOTER_BTN_HOVER: Color32 = color::FOOTER_BTN_HOVER;
const FOOTER_BTN_PRESSED: Color32 = color::FOOTER_BTN_PRESSED;

const FOOTER_FONT: u16 = 11;

// ── FooterLayout ───────────────────────────────────────────────────

#[derive(Default)]
struct FooterLayout {
    selection_info: Rect,
    quantize_label: Rect,
    quantize_button: Rect,
    resolution_label: Rect,
    resolution_button: Rect,
    fps_label: Rect,
    fps_field: Rect,
}

impl FooterLayout {
    fn compute(&mut self, bounds: Rect) {
        let elem_h = bounds.height - ELEM_Y_PAD * 2.0;
        let y = bounds.y + ELEM_Y_PAD;

        // Right-to-left
        let mut rx = bounds.x_max() - RIGHT_GUTTER;

        rx -= FPS_FIELD_W;
        self.fps_field = Rect::new(rx, y, FPS_FIELD_W, elem_h);
        rx -= LABEL_GAP;
        rx -= FPS_LABEL_W;
        self.fps_label = Rect::new(rx, y, FPS_LABEL_W, elem_h);
        rx -= SECTION_SPACER;

        rx -= RESOLUTION_BUTTON_W;
        self.resolution_button = Rect::new(rx, y, RESOLUTION_BUTTON_W, elem_h);
        rx -= LABEL_GAP;
        rx -= RESOLUTION_LABEL_W;
        self.resolution_label = Rect::new(rx, y, RESOLUTION_LABEL_W, elem_h);
        rx -= SECTION_SPACER;

        rx -= QUANTIZE_BUTTON_W;
        self.quantize_button = Rect::new(rx, y, QUANTIZE_BUTTON_W, elem_h);
        rx -= LABEL_GAP;
        rx -= QUANTIZE_LABEL_W;
        self.quantize_label = Rect::new(rx, y, QUANTIZE_LABEL_W, elem_h);
        rx -= SECTION_SPACER;

        let lx = bounds.x + PAD;
        let info_w = (rx - lx).max(0.0);
        self.selection_info = Rect::new(lx, y, info_w, elem_h);
    }
}

// ── FooterPanel ────────────────────────────────────────────────────

pub struct FooterPanel {
    layout: FooterLayout,

    // Node IDs
    selection_info_id: i32,
    quantize_label_id: i32,
    quantize_button_id: i32,
    resolution_label_id: i32,
    resolution_button_id: i32,
    fps_label_id: i32,
    fps_field_id: i32,

    // State
    selection_info: String,
    quantize_text: String,
    resolution_text: String,
    fps_text: String,
}

impl FooterPanel {
    pub fn new() -> Self {
        Self {
            layout: FooterLayout::default(),
            selection_info_id: -1,
            quantize_label_id: -1,
            quantize_button_id: -1,
            resolution_label_id: -1,
            resolution_button_id: -1,
            fps_label_id: -1,
            fps_field_id: -1,
            selection_info: String::new(),
            quantize_text: "Off".into(),
            resolution_text: "1080p".into(),
            fps_text: "60".into(),
        }
    }

    // ── Public accessors ───────────────────────────────────────────

    pub fn fps_field_id(&self) -> i32 { self.fps_field_id }
    pub fn resolution_button_id(&self) -> i32 { self.resolution_button_id }

    // ── Push-based setters ─────────────────────────────────────────

    pub fn set_selection_info(&mut self, tree: &mut UITree, text: &str) {
        self.selection_info = text.into();
        if self.selection_info_id >= 0 { tree.set_text(self.selection_info_id as u32, text); }
    }

    pub fn set_quantize_text(&mut self, tree: &mut UITree, text: &str) {
        self.quantize_text = text.into();
        if self.quantize_button_id >= 0 { tree.set_text(self.quantize_button_id as u32, text); }
    }

    pub fn set_resolution_text(&mut self, tree: &mut UITree, text: &str) {
        self.resolution_text = text.into();
        if self.resolution_button_id >= 0 { tree.set_text(self.resolution_button_id as u32, text); }
    }

    pub fn set_fps_text(&mut self, tree: &mut UITree, text: &str) {
        self.fps_text = text.into();
        if self.fps_field_id >= 0 { tree.set_text(self.fps_field_id as u32, text); }
    }

    fn footer_button_style() -> UIStyle {
        UIStyle {
            bg_color: color::BUTTON_INACTIVE_C32,
            hover_bg_color: FOOTER_BTN_HOVER,
            pressed_bg_color: FOOTER_BTN_PRESSED,
            text_color: color::TEXT_PRIMARY_C32,
            font_size: FOOTER_FONT,
            corner_radius: color::SMALL_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }

    fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.quantize_button_id { return vec![PanelAction::CycleQuantize]; }
        if id == self.resolution_button_id { return vec![PanelAction::ResolutionClicked]; }
        if id == self.fps_field_id { return vec![PanelAction::FpsFieldClicked]; }
        Vec::new()
    }
}

impl Default for FooterPanel {
    fn default() -> Self { Self::new() }
}

impl Panel for FooterPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        let footer = layout.footer();
        self.layout.compute(footer);

        let selection_info = self.selection_info.clone();
        let quantize_text = self.quantize_text.clone();
        let resolution_text = self.resolution_text.clone();
        let fps_text = self.fps_text.clone();

        let bg = tree.add_panel(
            -1, footer.x, footer.y, footer.width, footer.height,
            UIStyle { bg_color: color::PANEL_BG_DARK, ..UIStyle::default() },
        ) as i32;

        // Selection info
        self.selection_info_id = tree.add_node(
            bg, self.layout.selection_info, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
            Some(&selection_info), UIFlags::empty(),
        ) as i32;

        // Quantize
        self.quantize_label_id = tree.add_node(
            bg, self.layout.quantize_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("Q:"), UIFlags::empty(),
        ) as i32;

        self.quantize_button_id = tree.add_button(
            bg,
            self.layout.quantize_button.x, self.layout.quantize_button.y,
            self.layout.quantize_button.width, self.layout.quantize_button.height,
            Self::footer_button_style(), &quantize_text,
        ) as i32;

        // Resolution
        self.resolution_label_id = tree.add_node(
            bg, self.layout.resolution_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("RES:"), UIFlags::empty(),
        ) as i32;

        self.resolution_button_id = tree.add_button(
            bg,
            self.layout.resolution_button.x, self.layout.resolution_button.y,
            self.layout.resolution_button.width, self.layout.resolution_button.height,
            Self::footer_button_style(), &resolution_text,
        ) as i32;

        // FPS
        self.fps_label_id = tree.add_node(
            bg, self.layout.fps_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("FPS:"), UIFlags::empty(),
        ) as i32;

        self.fps_field_id = tree.add_button(
            bg,
            self.layout.fps_field.x, self.layout.fps_field.y,
            self.layout.fps_field.width, self.layout.fps_field.height,
            Self::footer_button_style(), &fps_text,
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
    fn build_footer() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();

        panel.build(&mut tree, &layout);

        assert!(panel.selection_info_id >= 0);
        assert!(panel.quantize_button_id >= 0);
        assert!(panel.resolution_button_id >= 0);
        assert!(panel.fps_field_id >= 0);
        assert!(tree.count() >= 8); // bg + 7 elements
    }

    #[test]
    fn handle_click_quantize() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.quantize_button_id as u32);
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::CycleQuantize));
    }

    #[test]
    fn set_fps_text_updates() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        tree.clear_dirty();
        panel.set_fps_text(&mut tree, "30");
        assert!(tree.has_dirty());
        assert_eq!(
            tree.get_node(panel.fps_field_id as u32).text.as_deref(),
            Some("30")
        );
    }
}
