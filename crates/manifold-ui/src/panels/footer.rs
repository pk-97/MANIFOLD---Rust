use super::{Panel, PanelAction};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use manifold_core::TonemapCurve;

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
    tonemap_khr: Rect,
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

        // Tonemap curve buttons: [ACES] [Hill] [AgX] [Khr]
        rx -= TONEMAP_BUTTON_W;
        self.tonemap_khr = Rect::new(rx, y, TONEMAP_BUTTON_W, elem_h);
        rx -= TONEMAP_BTN_GAP + TONEMAP_BUTTON_W;
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

    // Node IDs (None until `build` runs)
    selection_info_id: Option<NodeId>,
    quantize_label_id: Option<NodeId>,
    quantize_button_id: Option<NodeId>,
    resolution_label_id: Option<NodeId>,
    resolution_button_id: Option<NodeId>,
    scale_100_id: Option<NodeId>,
    scale_75_id: Option<NodeId>,
    scale_50_id: Option<NodeId>,
    tonemap_label_id: Option<NodeId>,
    tonemap_aces_id: Option<NodeId>,
    tonemap_hill_id: Option<NodeId>,
    tonemap_agx_id: Option<NodeId>,
    tonemap_khr_id: Option<NodeId>,
    fps_label_id: Option<NodeId>,
    fps_field_id: Option<NodeId>,

    // State
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
            selection_info_id: None,
            quantize_label_id: None,
            quantize_button_id: None,
            resolution_label_id: None,
            resolution_button_id: None,
            scale_100_id: None,
            scale_75_id: None,
            scale_50_id: None,
            tonemap_label_id: None,
            tonemap_aces_id: None,
            tonemap_hill_id: None,
            tonemap_agx_id: None,
            tonemap_khr_id: None,
            fps_label_id: None,
            fps_field_id: None,
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

    pub fn fps_field_id(&self) -> Option<NodeId> {
        self.fps_field_id
    }
    pub fn resolution_button_id(&self) -> Option<NodeId> {
        self.resolution_button_id
    }

    // ── Push-based setters ─────────────────────────────────────────

    pub fn set_selection_info(&mut self, tree: &mut UITree, text: &str) {
        self.selection_info = text.into();
        if let Some(id) = self.selection_info_id {
            tree.set_text(id, text);
        }
    }

    pub fn set_quantize_text(&mut self, tree: &mut UITree, text: &str) {
        self.quantize_text = text.into();
        if let Some(id) = self.quantize_button_id {
            tree.set_text(id, text);
        }
    }

    pub fn set_resolution_text(&mut self, tree: &mut UITree, text: &str) {
        self.resolution_text = text.into();
        if let Some(id) = self.resolution_button_id {
            tree.set_text(id, text);
        }
    }

    pub fn set_fps_text(&mut self, tree: &mut UITree, text: &str) {
        self.fps_text = text.into();
        if let Some(id) = self.fps_field_id {
            tree.set_text(id, text);
        }
    }

    /// Highlight the active render scale button. No-op if scale unchanged.
    pub fn set_render_scale(&mut self, tree: &mut UITree, scale: f32) {
        if (scale - self.current_render_scale).abs() < 0.01 {
            return;
        }
        self.current_render_scale = scale;
        self.refresh_scale_button_styles(tree);
    }

    /// Highlight the active tonemap curve button. No-op if curve unchanged.
    pub fn set_tonemap_curve(&mut self, tree: &mut UITree, curve: TonemapCurve) {
        if curve == self.current_tonemap_curve {
            return;
        }
        self.current_tonemap_curve = curve;
        self.refresh_tonemap_button_styles(tree);
    }

    fn refresh_tonemap_button_styles(&self, tree: &mut UITree) {
        let ids = [
            (self.tonemap_aces_id, TonemapCurve::AcesNarkowicz),
            (self.tonemap_hill_id, TonemapCurve::AcesHill),
            (self.tonemap_agx_id, TonemapCurve::Agx),
            (self.tonemap_khr_id, TonemapCurve::KhronosPbrNeutral),
        ];
        for (id, val) in ids {
            let Some(id) = id else {
                continue;
            };
            let active = val == self.current_tonemap_curve;
            tree.set_style(
                id,
                UIStyle {
                    bg_color: if active {
                        FOOTER_SCALE_ACTIVE
                    } else {
                        color::BUTTON_INACTIVE_C32
                    },
                    ..Self::footer_button_style()
                },
            );
        }
    }

    fn tonemap_button_style_for(&self, curve: TonemapCurve) -> UIStyle {
        let active = curve == self.current_tonemap_curve;
        UIStyle {
            bg_color: if active {
                FOOTER_SCALE_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            },
            ..Self::footer_button_style()
        }
    }

    fn refresh_scale_button_styles(&self, tree: &mut UITree) {
        let ids = [
            (self.scale_100_id, 1.0f32),
            (self.scale_75_id, 0.75),
            (self.scale_50_id, 0.5),
        ];
        for (id, val) in ids {
            let Some(id) = id else {
                continue;
            };
            let active = (val - self.current_render_scale).abs() < 0.01;
            tree.set_style(
                id,
                UIStyle {
                    bg_color: if active {
                        FOOTER_SCALE_ACTIVE
                    } else {
                        color::BUTTON_INACTIVE_C32
                    },
                    ..Self::footer_button_style()
                },
            );
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
            bg_color: if active {
                FOOTER_SCALE_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            },
            ..Self::footer_button_style()
        }
    }

    fn handle_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        let id = Some(node_id);
        if id == self.quantize_button_id {
            return vec![PanelAction::CycleQuantize];
        }
        if id == self.resolution_button_id {
            return vec![PanelAction::ResolutionClicked];
        }
        if id == self.fps_field_id {
            return vec![PanelAction::FpsFieldClicked];
        }
        if id == self.scale_100_id {
            return vec![PanelAction::SetRenderScale(1.0)];
        }
        if id == self.scale_75_id {
            return vec![PanelAction::SetRenderScale(0.75)];
        }
        if id == self.scale_50_id {
            return vec![PanelAction::SetRenderScale(0.5)];
        }
        if id == self.tonemap_aces_id {
            return vec![PanelAction::SetTonemapCurve(TonemapCurve::AcesNarkowicz)];
        }
        if id == self.tonemap_hill_id {
            return vec![PanelAction::SetTonemapCurve(TonemapCurve::AcesHill)];
        }
        if id == self.tonemap_agx_id {
            return vec![PanelAction::SetTonemapCurve(TonemapCurve::Agx)];
        }
        if id == self.tonemap_khr_id {
            return vec![PanelAction::SetTonemapCurve(
                TonemapCurve::KhronosPbrNeutral,
            )];
        }
        Vec::new()
    }

    /// Node-intent dispatch for the footer buttons' clicks. Mirrors
    /// `handle_click`. See `docs/NODE_INTENT_DISPATCH.md`.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::intent::Gesture::Click;
        let mut on = |id: Option<NodeId>, a: PanelAction| {
            if let Some(id) = id {
                intents.on(id, Click, a);
            }
        };
        on(self.quantize_button_id, PanelAction::CycleQuantize);
        on(self.resolution_button_id, PanelAction::ResolutionClicked);
        on(self.fps_field_id, PanelAction::FpsFieldClicked);
        on(self.scale_100_id, PanelAction::SetRenderScale(1.0));
        on(self.scale_75_id, PanelAction::SetRenderScale(0.75));
        on(self.scale_50_id, PanelAction::SetRenderScale(0.5));
        on(self.tonemap_aces_id, PanelAction::SetTonemapCurve(TonemapCurve::AcesNarkowicz));
        on(self.tonemap_hill_id, PanelAction::SetTonemapCurve(TonemapCurve::AcesHill));
        on(self.tonemap_agx_id, PanelAction::SetTonemapCurve(TonemapCurve::Agx));
        on(self.tonemap_khr_id, PanelAction::SetTonemapCurve(TonemapCurve::KhronosPbrNeutral));
    }
}

impl Default for FooterPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Shrink a right-aligned label cell to its measured text width, keeping the
/// right edge fixed so the glyphs render unchanged. The Phase 1.4 build-time
/// size-to-content path: the width comes from `tree.measure_text`, not a guess.
/// Clamped to the reserved cell width so a long string can't overflow its slot.
fn fit_label_right(tree: &UITree, cell: Rect, text: &str, font: u16, weight: FontWeight) -> Rect {
    let w = tree.text_width(text, font, weight).min(cell.width);
    Rect::new(cell.x_max() - w, cell.y, w, cell.height)
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
            None,
            footer.x,
            footer.y,
            footer.width,
            footer.height,
            UIStyle {
                bg_color: color::PANEL_BG_DARK,
                ..UIStyle::default()
            },
        );

        // Selection info
        self.selection_info_id = Some(tree.add_node(
            Some(bg),
            self.layout.selection_info,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
            Some(&selection_info),
            UIFlags::empty(),
        ));

        // Quantize. The "Q:" label cell is sized to its measured text at build
        // time (Phase 1.4 size-to-content) rather than the old magic width, then
        // right-anchored so the right-aligned glyphs land in the exact same spot.
        let quantize_label_rect =
            fit_label_right(tree, self.layout.quantize_label, "Q:", FOOTER_FONT, FontWeight::Regular);
        self.quantize_label_id = Some(tree.add_node(
            Some(bg),
            quantize_label_rect,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("Q:"),
            UIFlags::empty(),
        ));

        self.quantize_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.quantize_button.x,
            self.layout.quantize_button.y,
            self.layout.quantize_button.width,
            self.layout.quantize_button.height,
            Self::footer_button_style(),
            &quantize_text,
        ));

        // Resolution
        self.resolution_label_id = Some(tree.add_node(
            Some(bg),
            self.layout.resolution_label,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("RES:"),
            UIFlags::empty(),
        ));

        self.resolution_button_id = Some(tree.add_button(
            Some(bg),
            self.layout.resolution_button.x,
            self.layout.resolution_button.y,
            self.layout.resolution_button.width,
            self.layout.resolution_button.height,
            Self::footer_button_style(),
            &resolution_text,
        ));

        // Render scale buttons: [1x] [75%] [50%]
        self.scale_100_id = Some(tree.add_button(
            Some(bg),
            self.layout.scale_100.x,
            self.layout.scale_100.y,
            self.layout.scale_100.width,
            self.layout.scale_100.height,
            self.scale_button_style_for(1.0),
            "1×",
        ));

        self.scale_75_id = Some(tree.add_button(
            Some(bg),
            self.layout.scale_75.x,
            self.layout.scale_75.y,
            self.layout.scale_75.width,
            self.layout.scale_75.height,
            self.scale_button_style_for(0.75),
            "75%",
        ));

        self.scale_50_id = Some(tree.add_button(
            Some(bg),
            self.layout.scale_50.x,
            self.layout.scale_50.y,
            self.layout.scale_50.width,
            self.layout.scale_50.height,
            self.scale_button_style_for(0.5),
            "50%",
        ));

        // Tonemap curve
        self.tonemap_label_id = Some(tree.add_node(
            Some(bg),
            self.layout.tonemap_label,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("TM:"),
            UIFlags::empty(),
        ));

        self.tonemap_aces_id = Some(tree.add_button(
            Some(bg),
            self.layout.tonemap_aces.x,
            self.layout.tonemap_aces.y,
            self.layout.tonemap_aces.width,
            self.layout.tonemap_aces.height,
            self.tonemap_button_style_for(TonemapCurve::AcesNarkowicz),
            "ACE",
        ));

        self.tonemap_hill_id = Some(tree.add_button(
            Some(bg),
            self.layout.tonemap_hill.x,
            self.layout.tonemap_hill.y,
            self.layout.tonemap_hill.width,
            self.layout.tonemap_hill.height,
            self.tonemap_button_style_for(TonemapCurve::AcesHill),
            "Hill",
        ));

        self.tonemap_agx_id = Some(tree.add_button(
            Some(bg),
            self.layout.tonemap_agx.x,
            self.layout.tonemap_agx.y,
            self.layout.tonemap_agx.width,
            self.layout.tonemap_agx.height,
            self.tonemap_button_style_for(TonemapCurve::Agx),
            "AgX",
        ));

        self.tonemap_khr_id = Some(tree.add_button(
            Some(bg),
            self.layout.tonemap_khr.x,
            self.layout.tonemap_khr.y,
            self.layout.tonemap_khr.width,
            self.layout.tonemap_khr.height,
            self.tonemap_button_style_for(TonemapCurve::KhronosPbrNeutral),
            "Khr",
        ));

        // FPS
        self.fps_label_id = Some(tree.add_node(
            Some(bg),
            self.layout.fps_label,
            UINodeType::Label,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FOOTER_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
            Some("FPS:"),
            UIFlags::empty(),
        ));

        self.fps_field_id = Some(tree.add_button(
            Some(bg),
            self.layout.fps_field.x,
            self.layout.fps_field.y,
            self.layout.fps_field.width,
            self.layout.fps_field.height,
            Self::footer_button_style(),
            &fps_text,
        ));

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
    fn build_footer() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();

        panel.build(&mut tree, &layout);

        assert!(panel.selection_info_id.is_some());
        assert!(panel.quantize_button_id.is_some());
        assert!(panel.resolution_button_id.is_some());
        assert!(panel.scale_100_id.is_some());
        assert!(panel.scale_75_id.is_some());
        assert!(panel.scale_50_id.is_some());
        assert!(panel.fps_field_id.is_some());
        assert!(tree.count() >= 12); // bg + 11 elements
    }

    #[test]
    fn quantize_label_sized_to_text() {
        // Phase 1.4: the label cell width comes from build-time text measurement,
        // right-anchored. Its width tracks the measured glyphs and its right edge
        // stays put (so the right-aligned text doesn't move).
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let measured = tree.text_width("Q:", FOOTER_FONT, FontWeight::Regular);
        let node = tree.get_node(panel.quantize_label_id.unwrap());
        assert!((node.bounds.width - measured).abs() < 0.01);
        // Right edge unchanged vs the reserved cell — glyphs render in place.
        assert!((node.bounds.x_max() - panel.layout.quantize_label.x_max()).abs() < 0.01);
    }

    #[test]
    fn handle_click_quantize() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.quantize_button_id.unwrap());
        assert_eq!(a.len(), 1);
        assert!(matches!(a[0], PanelAction::CycleQuantize));
    }

    #[test]
    fn handle_click_scale_buttons() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let a = panel.handle_click(panel.scale_100_id.unwrap());
        assert!(matches!(a[0], PanelAction::SetRenderScale(s) if (s - 1.0).abs() < 0.01));
        let b = panel.handle_click(panel.scale_75_id.unwrap());
        assert!(matches!(b[0], PanelAction::SetRenderScale(s) if (s - 0.75).abs() < 0.01));
        let c = panel.handle_click(panel.scale_50_id.unwrap());
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
            tree.get_node(panel.fps_field_id.unwrap()).text.as_deref(),
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
