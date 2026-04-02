use manifold_core::TonemapCurve;
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
const SCALE_BUTTON_W: f32 = 28.0;
const SCALE_BTN_GAP: f32 = 2.0;
const TONEMAP_LABEL_W: f32 = 24.0;
const TONEMAP_BUTTON_W: f32 = 36.0;
const TONEMAP_BTN_GAP: f32 = 2.0;
const FPS_LABEL_W: f32 = 32.0;
const FPS_FIELD_W: f32 = 46.0;
const VSYNC_BTN_W: f32 = 40.0;
const VSYNC_ACTUAL_W: f32 = 42.0;
const RIGHT_GUTTER: f32 = 10.0;

// ── Panel-specific colors ──────────────────────────────────────────

const FOOTER_BTN_HOVER: Color32 = color::FOOTER_BTN_HOVER;
const FOOTER_BTN_PRESSED: Color32 = color::FOOTER_BTN_PRESSED;
const FOOTER_SCALE_ACTIVE: Color32 = color::HEADER_BUTTON_ACTIVE;

const FOOTER_FONT: u16 = color::FONT_LABEL;

// ── FooterLayout ───────────────────────────────────────────────────

#[derive(Default)]
struct FooterLayout {
    selection_info: Rect,
    quantize_label: Rect,
    quantize_button: Rect,
    resolution_label: Rect,
    resolution_button: Rect,
    scale_100: Rect,
    scale_75: Rect,
    scale_50: Rect,
    tonemap_label: Rect,
    tonemap_aces: Rect,
    tonemap_hill: Rect,
    tonemap_agx: Rect,
    fps_label: Rect,
    fps_field: Rect,
    vsync_btn: Rect,
    vsync_actual: Rect,
}

impl FooterLayout {
    fn compute(&mut self, bounds: Rect) {
        let elem_h = bounds.height - ELEM_Y_PAD * 2.0;
        let y = bounds.y + ELEM_Y_PAD;

        // Right-to-left
        let mut rx = bounds.x_max() - RIGHT_GUTTER;

        // VSync actual FPS (rightmost)
        rx -= VSYNC_ACTUAL_W;
        self.vsync_actual = Rect::new(rx, y, VSYNC_ACTUAL_W, elem_h);
        rx -= LABEL_GAP;
        // VSync toggle button
        rx -= VSYNC_BTN_W;
        self.vsync_btn = Rect::new(rx, y, VSYNC_BTN_W, elem_h);
        rx -= SECTION_SPACER;

        rx -= FPS_FIELD_W;
        self.fps_field = Rect::new(rx, y, FPS_FIELD_W, elem_h);
        rx -= LABEL_GAP;
        rx -= FPS_LABEL_W;
        self.fps_label = Rect::new(rx, y, FPS_LABEL_W, elem_h);
        rx -= SECTION_SPACER;

        // Tonemap curve buttons: [ACES] [Hill] [AgX]
        rx -= TONEMAP_BUTTON_W;
        self.tonemap_agx = Rect::new(rx, y, TONEMAP_BUTTON_W, elem_h);
        rx -= TONEMAP_BTN_GAP + TONEMAP_BUTTON_W;
        self.tonemap_hill = Rect::new(rx, y, TONEMAP_BUTTON_W, elem_h);
        rx -= TONEMAP_BTN_GAP + TONEMAP_BUTTON_W;
        self.tonemap_aces = Rect::new(rx, y, TONEMAP_BUTTON_W, elem_h);
        rx -= LABEL_GAP;
        rx -= TONEMAP_LABEL_W;
        self.tonemap_label = Rect::new(rx, y, TONEMAP_LABEL_W, elem_h);
        rx -= SECTION_SPACER;

        // Render scale buttons: [1x] [75%] [50%]
        // Right-to-left → subtract 50% first (rightmost), 1x last (leftmost)
        rx -= SCALE_BUTTON_W;
        self.scale_50 = Rect::new(rx, y, SCALE_BUTTON_W, elem_h);
        rx -= SCALE_BTN_GAP + SCALE_BUTTON_W;
        self.scale_75 = Rect::new(rx, y, SCALE_BUTTON_W, elem_h);
        rx -= SCALE_BTN_GAP + SCALE_BUTTON_W;
        self.scale_100 = Rect::new(rx, y, SCALE_BUTTON_W, elem_h);
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
    scale_100_id: i32,
    scale_75_id: i32,
    scale_50_id: i32,
    tonemap_label_id: i32,
    tonemap_aces_id: i32,
    tonemap_hill_id: i32,
    tonemap_agx_id: i32,
    fps_label_id: i32,
    fps_field_id: i32,
    vsync_btn_id: i32,
    vsync_actual_id: i32,

    // State
    vsync_enabled: bool,
    vsync_actual_fps: f32,
    selection_info: String,
    quantize_text: String,
    resolution_text: String,
    fps_text: String,
    current_render_scale: f32,
    current_tonemap_curve: TonemapCurve,

    // Cache tracking
    cache_first_node: usize,
    cache_node_count: usize,
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
            scale_100_id: -1,
            scale_75_id: -1,
            scale_50_id: -1,
            tonemap_label_id: -1,
            tonemap_aces_id: -1,
            tonemap_hill_id: -1,
            tonemap_agx_id: -1,
            fps_label_id: -1,
            fps_field_id: -1,
            vsync_btn_id: -1,
            vsync_actual_id: -1,
            vsync_enabled: true,
            vsync_actual_fps: 0.0,
            selection_info: String::new(),
            quantize_text: "Off".into(),
            resolution_text: "1080p".into(),
            fps_text: "60".into(),
            current_render_scale: 1.0,
            current_tonemap_curve: TonemapCurve::AcesNarkowicz,
            cache_first_node: usize::MAX,
            cache_node_count: 0,
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

    /// Update VSync toggle state and actual resolved FPS display.
    ///
    /// - `enabled`: from project settings (button highlight, instant on click)
    /// - `active`: from content thread (whether vsync mode is actually running)
    /// - `actual_fps`: from content thread (display_hz / divisor when active)
    pub fn set_vsync_state(
        &mut self, tree: &mut UITree,
        enabled: bool, active: bool, actual_fps: f32,
    ) {
        let state_changed = enabled != self.vsync_enabled;
        let fps_changed = (actual_fps - self.vsync_actual_fps).abs() > 0.1;
        if state_changed {
            self.vsync_enabled = enabled;
            if self.vsync_btn_id >= 0 {
                tree.set_style(self.vsync_btn_id as u32, self.vsync_btn_style());
            }
        }
        if state_changed || fps_changed {
            self.vsync_actual_fps = actual_fps;
            if self.vsync_actual_id >= 0 {
                // Only show resolved FPS when vsync is actually active on the
                // content thread (not just enabled in settings but waiting to activate).
                let text = if active && actual_fps > 0.0 {
                    format!("→{:.0}", actual_fps)
                } else {
                    String::new()
                };
                tree.set_text(self.vsync_actual_id as u32, &text);
            }
        }
    }

    /// Highlight the active render scale button. No-op if scale unchanged.
    pub fn set_render_scale(&mut self, tree: &mut UITree, scale: f32) {
        if (scale - self.current_render_scale).abs() < 0.01 { return; }
        self.current_render_scale = scale;
        self.refresh_scale_button_styles(tree);
    }

    /// Highlight the active tonemap curve button. No-op if curve unchanged.
    pub fn set_tonemap_curve(&mut self, tree: &mut UITree, curve: TonemapCurve) {
        if curve == self.current_tonemap_curve { return; }
        self.current_tonemap_curve = curve;
        self.refresh_tonemap_button_styles(tree);
    }

    fn refresh_tonemap_button_styles(&self, tree: &mut UITree) {
        let ids = [
            (self.tonemap_aces_id, TonemapCurve::AcesNarkowicz),
            (self.tonemap_hill_id, TonemapCurve::AcesHill),
            (self.tonemap_agx_id,  TonemapCurve::Agx),
        ];
        for (id, val) in ids {
            if id < 0 { continue; }
            let active = val == self.current_tonemap_curve;
            tree.set_style(id as u32, UIStyle {
                bg_color: if active { FOOTER_SCALE_ACTIVE } else { color::BUTTON_INACTIVE_C32 },
                ..Self::footer_button_style()
            });
        }
    }

    fn tonemap_button_style_for(&self, curve: TonemapCurve) -> UIStyle {
        let active = curve == self.current_tonemap_curve;
        UIStyle {
            bg_color: if active { FOOTER_SCALE_ACTIVE } else { color::BUTTON_INACTIVE_C32 },
            ..Self::footer_button_style()
        }
    }

    fn refresh_scale_button_styles(&self, tree: &mut UITree) {
        let ids = [
            (self.scale_100_id, 1.0f32),
            (self.scale_75_id,  0.75),
            (self.scale_50_id,  0.5),
        ];
        for (id, val) in ids {
            if id < 0 { continue; }
            let active = (val - self.current_render_scale).abs() < 0.01;
            tree.set_style(id as u32, UIStyle {
                bg_color: if active { FOOTER_SCALE_ACTIVE } else { color::BUTTON_INACTIVE_C32 },
                ..Self::footer_button_style()
            });
        }
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

    fn scale_button_style_for(&self, scale: f32) -> UIStyle {
        let active = (scale - self.current_render_scale).abs() < 0.01;
        UIStyle {
            bg_color: if active { FOOTER_SCALE_ACTIVE } else { color::BUTTON_INACTIVE_C32 },
            ..Self::footer_button_style()
        }
    }

    fn vsync_btn_style(&self) -> UIStyle {
        if self.vsync_enabled {
            UIStyle {
                bg_color: FOOTER_SCALE_ACTIVE,
                hover_bg_color: FOOTER_BTN_HOVER,
                pressed_bg_color: FOOTER_BTN_PRESSED,
                text_color: color::TEXT_WHITE_C32,
                font_size: FOOTER_FONT,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        } else {
            Self::footer_button_style()
        }
    }

    fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.quantize_button_id  { return vec![PanelAction::CycleQuantize]; }
        if id == self.resolution_button_id { return vec![PanelAction::ResolutionClicked]; }
        if id == self.fps_field_id         { return vec![PanelAction::FpsFieldClicked]; }
        if id == self.vsync_btn_id         { return vec![PanelAction::ToggleVsync]; }
        if id == self.scale_100_id         { return vec![PanelAction::SetRenderScale(1.0)]; }
        if id == self.scale_75_id          { return vec![PanelAction::SetRenderScale(0.75)]; }
        if id == self.scale_50_id          { return vec![PanelAction::SetRenderScale(0.5)]; }
        if id == self.tonemap_aces_id      { return vec![PanelAction::SetTonemapCurve(TonemapCurve::AcesNarkowicz)]; }
        if id == self.tonemap_hill_id      { return vec![PanelAction::SetTonemapCurve(TonemapCurve::AcesHill)]; }
        if id == self.tonemap_agx_id       { return vec![PanelAction::SetTonemapCurve(TonemapCurve::Agx)]; }
        Vec::new()
    }
}

impl Default for FooterPanel {
    fn default() -> Self { Self::new() }
}

impl Panel for FooterPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.cache_first_node = tree.count();

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

        // Render scale buttons: [1x] [75%] [50%]
        self.scale_100_id = tree.add_button(
            bg,
            self.layout.scale_100.x, self.layout.scale_100.y,
            self.layout.scale_100.width, self.layout.scale_100.height,
            self.scale_button_style_for(1.0), "1×",
        ) as i32;

        self.scale_75_id = tree.add_button(
            bg,
            self.layout.scale_75.x, self.layout.scale_75.y,
            self.layout.scale_75.width, self.layout.scale_75.height,
            self.scale_button_style_for(0.75), "75%",
        ) as i32;

        self.scale_50_id = tree.add_button(
            bg,
            self.layout.scale_50.x, self.layout.scale_50.y,
            self.layout.scale_50.width, self.layout.scale_50.height,
            self.scale_button_style_for(0.5), "50%",
        ) as i32;

        // Tonemap curve
        self.tonemap_label_id = tree.add_node(
            bg, self.layout.tonemap_label, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("TM:"), UIFlags::empty(),
        ) as i32;

        self.tonemap_aces_id = tree.add_button(
            bg,
            self.layout.tonemap_aces.x, self.layout.tonemap_aces.y,
            self.layout.tonemap_aces.width, self.layout.tonemap_aces.height,
            self.tonemap_button_style_for(TonemapCurve::AcesNarkowicz), "ACE",
        ) as i32;

        self.tonemap_hill_id = tree.add_button(
            bg,
            self.layout.tonemap_hill.x, self.layout.tonemap_hill.y,
            self.layout.tonemap_hill.width, self.layout.tonemap_hill.height,
            self.tonemap_button_style_for(TonemapCurve::AcesHill), "Hill",
        ) as i32;

        self.tonemap_agx_id = tree.add_button(
            bg,
            self.layout.tonemap_agx.x, self.layout.tonemap_agx.y,
            self.layout.tonemap_agx.width, self.layout.tonemap_agx.height,
            self.tonemap_button_style_for(TonemapCurve::Agx), "AgX",
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

        // VSync toggle button
        self.vsync_btn_id = tree.add_button(
            bg,
            self.layout.vsync_btn.x, self.layout.vsync_btn.y,
            self.layout.vsync_btn.width, self.layout.vsync_btn.height,
            self.vsync_btn_style(), "VSYNC",
        ) as i32;

        // VSync actual FPS (shows resolved FPS when vsync is active)
        let vsync_text = if self.vsync_enabled {
            format!("→{:.0}", self.vsync_actual_fps)
        } else {
            String::new()
        };
        self.vsync_actual_id = tree.add_node(
            bg, self.layout.vsync_actual, UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
            Some(&vsync_text), UIFlags::empty(),
        ) as i32;

        self.cache_node_count = tree.count() - self.cache_first_node;
    }

    fn update(&mut self, _tree: &mut UITree) {}

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click { node_id, .. } => self.handle_click(*node_id),
            _ => Vec::new(),
        }
    }

    fn first_node(&self) -> usize { self.cache_first_node }
    fn node_count(&self) -> usize { self.cache_node_count }
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
        assert!(panel.scale_100_id >= 0);
        assert!(panel.scale_75_id >= 0);
        assert!(panel.scale_50_id >= 0);
        assert!(panel.fps_field_id >= 0);
        assert!(tree.count() >= 11); // bg + 10 elements
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
    fn handle_click_scale_buttons() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.scale_100_id as u32);
        assert!(matches!(a[0], PanelAction::SetRenderScale(s) if (s - 1.0).abs() < 0.01));
        let b = panel.handle_click(panel.scale_75_id as u32);
        assert!(matches!(b[0], PanelAction::SetRenderScale(s) if (s - 0.75).abs() < 0.01));
        let c = panel.handle_click(panel.scale_50_id as u32);
        assert!(matches!(c[0], PanelAction::SetRenderScale(s) if (s - 0.5).abs() < 0.01));
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

    #[test]
    fn set_render_scale_updates_style() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        // Default is 1.0 — scale_100 should be active
        assert_eq!(panel.current_render_scale, 1.0);

        // Switch to 0.75 — should not panic and should update state
        panel.set_render_scale(&mut tree, 0.75);
        assert!((panel.current_render_scale - 0.75).abs() < 0.01);
    }
}
