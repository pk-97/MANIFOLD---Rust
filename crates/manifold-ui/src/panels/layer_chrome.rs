//! Layer inspector card on the Chrome API (hybrid — see `master_chrome`).
//!
//! Host owns the declarative chrome (header + collapse chevron, optional name
//! row, dividers) plus an optional `Fill` opacity-slider slot; the `BitmapSlider`
//! is dropped into the recovered slot, byte-identical. Public interface unchanged,
//! so the inspector composite is untouched.

use super::PanelAction;
use crate::chrome::{Align, ChromeHost, Pad, Sizing, SliderSpec, View};
use crate::color;
use crate::node::*;
use crate::slider::{SliderColors, SliderDragState};
use crate::tree::UITree;

// ── Layout constants (from LayerChromeBitmapPanel.cs) ─────────────

const HEADER_ROW_H: f32 = 27.5;
const NAME_ROW_H: f32 = 20.0;
const SLIDER_ROW_H: f32 = 22.5;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = 2.0;
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const CHEVRON_H: f32 = 16.0;
const OPACITY_LABEL_W: f32 = 50.0;
const FONT_SIZE: u16 = color::FONT_BODY;
const NAME_FONT_SIZE: u16 = color::FONT_SUBHEADING;

const KEY_CHEVRON: u64 = 1;
const KEY_OPACITY_SLOT: u64 = 2;

fn fmt_opacity(v: f32) -> String {
    format!("{:.2}", v)
}

// ── LayerChromePanel ─────────────────────────────────────────────

pub struct LayerChromePanel {
    host: ChromeHost,
    chrome_rect: Rect,

    opacity: SliderDragState,

    is_collapsed: bool,
    show_name: bool,
    show_opacity: bool,
    cached_header_text: String,
    cached_name: String,

    first_node: usize,
    node_count: usize,
}

impl LayerChromePanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            chrome_rect: Rect::ZERO,
            opacity: SliderDragState::default(),
            is_collapsed: false,
            show_name: true,
            show_opacity: true,
            cached_header_text: "Layer".into(),
            cached_name: String::new(),
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

    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn is_dragging(&self) -> bool {
        self.opacity.is_dragging()
    }
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn set_collapsed(&mut self, v: bool) {
        self.is_collapsed = v;
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

    // ── View description (chrome only) ───────────────────────────

    fn divider() -> View {
        View::panel()
            .fill_w()
            .h(Sizing::Fixed(DIVIDER_H))
            .bg(color::DIVIDER_C32)
    }

    fn chrome_view(&self) -> View {
        let chevron = View::button(if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" })
            .fixed(CHEVRON_W, CHEVRON_H)
            .style(UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            })
            .inert()
            .key(KEY_CHEVRON);

        let header = View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(HEADER_ROW_H))
            .cross_align(Align::Center)
            .child(
                View::label(self.cached_header_text.as_str())
                    .fill_w()
                    .fill_h()
                    .font(color::FONT_HEADING)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(chevron);

        let mut root = View::column(0.0)
            .fill()
            .pad(Pad { l: PAD_H, t: PAD_V, r: PAD_H, b: PAD_V })
            .child(header);
        if self.is_collapsed {
            return root;
        }

        if self.show_name {
            root = root.child(Self::divider()).child(
                View::label(self.cached_name.as_str())
                    .fill_w()
                    .h(Sizing::Fixed(NAME_ROW_H))
                    .font(NAME_FONT_SIZE)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Center),
            );
        }
        if self.show_opacity {
            let v = self.opacity.cached_value();
            let v = if v.is_nan() { 1.0 } else { v };
            let spec = SliderSpec {
                label: Some("Opacity".to_string()),
                value: v,
                value_text: fmt_opacity(v),
                colors: SliderColors::default_slider(),
                font_size: FONT_SIZE,
                label_width: OPACITY_LABEL_W,
            };
            root = root.child(Self::divider()).child(
                View::slider_row(spec)
                    .fill_w()
                    .h(Sizing::Fixed(SLIDER_ROW_H))
                    .key(KEY_OPACITY_SLOT),
            );
        }
        root.child(Self::divider())
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.chrome_rect = rect;
        let view = self.chrome_view();
        self.host.build(tree, &view, rect);
        self.first_node = self.host.first_node();

        // The host materialised the opacity slider (when shown); wire its ids.
        match self.host.slider_ids(KEY_OPACITY_SLOT) {
            Some(ids) => self.opacity.set_ids(ids),
            None => self.opacity.clear(),
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn reconcile_chrome(&mut self, tree: &mut UITree) {
        if !self.host.is_built() {
            return;
        }
        let view = self.chrome_view();
        let _ = self.host.update(tree, &view, self.chrome_rect);
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_header_text(&mut self, tree: &mut UITree, text: &str) {
        if self.cached_header_text == text {
            return;
        }
        self.cached_header_text = text.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_name(&mut self, tree: &mut UITree, name: &str) {
        if self.cached_name == name {
            return;
        }
        self.cached_name = name.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_opacity(&mut self, tree: &mut UITree, value: f32) {
        self.opacity.sync(tree, value, &fmt_opacity);
    }

    pub fn sync_collapsed(&mut self, _tree: &mut UITree, collapsed: bool) {
        self.is_collapsed = collapsed;
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        if self.host.node_id_for_key(KEY_CHEVRON) == Some(node_id) {
            return vec![PanelAction::LayerChromeCollapseToggle];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2) -> Vec<PanelAction> {
        if let Some(val) = self.opacity.try_start_drag(node_id, pos.x) {
            return vec![
                PanelAction::LayerOpacitySnapshot,
                PanelAction::LayerOpacityChanged(val),
            ];
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if let Some(val) = self.opacity.apply_drag(pos.x, tree, &fmt_opacity) {
            return vec![PanelAction::LayerOpacityChanged(val)];
        }
        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.opacity.end_drag() {
            return vec![PanelAction::LayerOpacityCommit];
        }
        Vec::new()
    }

    /// Node-intent dispatch for the layer opacity slider's right-click reset.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        if let Some(ids) = self.opacity.ids() {
            intents.on(
                ids.track,
                crate::intent::Gesture::RightClick,
                PanelAction::LayerOpacityRightClick,
            );
        }
    }
}

impl Default for LayerChromePanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn golden_opacity_slot(rect: Rect, show_name: bool) -> Rect {
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let mut cy = rect.y + PAD_V + HEADER_ROW_H;
        if show_name {
            cy += DIVIDER_H + NAME_ROW_H;
        }
        cy += DIVIDER_H;
        Rect::new(cx, cy, content_w, SLIDER_ROW_H)
    }

    #[test]
    fn slot_rect_matches_golden() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        let rect = Rect::new(0.0, 0.0, 280.0, 200.0);
        panel.build(&mut tree, rect);

        let got = tree.get_bounds(panel.host.node_id_for_key(KEY_OPACITY_SLOT).unwrap());
        let want = golden_opacity_slot(rect, true);
        assert!(
            (got.x - want.x).abs() < 0.01
                && (got.y - want.y).abs() < 0.01
                && (got.width - want.width).abs() < 0.01
                && (got.height - want.height).abs() < 0.01,
            "opacity slot {got:?} != golden {want:?}"
        );
    }

    #[test]
    fn build_makes_chrome_and_slider() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        assert!(panel.opacity.ids().is_some());
        assert!(panel.host.node_id_for_key(KEY_CHEVRON).is_some());
    }

    #[test]
    fn visibility_hides_rows() {
        let mut panel = LayerChromePanel::new();
        let full_h = panel.compute_height();
        panel.set_visibility(false, false);
        assert!(panel.compute_height() < full_h);
    }

    #[test]
    fn opacity_hidden_drops_slider() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.set_visibility(true, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        assert!(panel.opacity.ids().is_none());
        assert!(panel.host.node_id_for_key(KEY_OPACITY_SLOT).is_none());
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        let chev = panel.host.node_id_for_key(KEY_CHEVRON).unwrap();
        assert!(matches!(
            panel.handle_click(chev).as_slice(),
            [PanelAction::LayerChromeCollapseToggle]
        ));
    }

    #[test]
    fn sync_name_updates_in_place() {
        let mut tree = UITree::new();
        let mut panel = LayerChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        let sv = tree.structure_version();
        panel.sync_name(&mut tree, "Drums Layer");
        assert_eq!(tree.structure_version(), sv);
        // Name node carries the new text.
        let found = (0..tree.count())
            .map(|i| tree.get_node(NodeId(i as u32)))
            .any(|n| n.text.as_deref() == Some("Drums Layer"));
        assert!(found);
    }
}
