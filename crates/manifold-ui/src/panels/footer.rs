//! Footer status bar — the first chrome panel on the declarative Chrome API.
//!
//! The panel describes itself once in [`FooterPanel::view`]; the [`ChromeHost`]
//! reconciler builds it and reconciles per-frame value changes in place. There
//! is no `build()`/`update()` dual write and no hand-stored `self.*_id` fields.
//! See `docs/CHROME_API_DESIGN.md`.

use super::{Panel, PanelAction};
use crate::chrome::{ChromeHost, Pad, Reconcile, Sizing, View};
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

/// Stable key for the FPS field — the app anchors a text-input session to this
/// node ([`FooterPanel::fps_field_id`]), so it carries a key instead of a
/// hand-stored id.
const KEY_FPS_FIELD: u64 = 1;

// ── FooterPanel ────────────────────────────────────────────────────

pub struct FooterPanel {
    host: ChromeHost,
    /// Footer rect captured at build, reused by the per-frame reconcile.
    rect: Rect,

    // Display state — the single source the `view()` description reads.
    selection_info: String,
    quantize_text: String,
    resolution_text: String,
    fps_text: String,
    current_render_scale: f32,
    current_tonemap_curve: TonemapCurve,
}

impl FooterPanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            rect: Rect::ZERO,
            selection_info: String::new(),
            quantize_text: "Off".into(),
            resolution_text: "1080p".into(),
            fps_text: "60".into(),
            current_render_scale: 1.0,
            current_tonemap_curve: TonemapCurve::AcesNarkowicz,
        }
    }

    // ── Public accessors ───────────────────────────────────────────

    /// Tree node id of the FPS field, for anchoring its text-input overlay.
    /// Resolved from the live description by stable key — survives reconciles.
    pub fn fps_field_id(&self) -> Option<NodeId> {
        self.host.node_id_for_key(KEY_FPS_FIELD)
    }

    // ── State setters (store only; the reconcile applies them) ──────

    pub fn set_selection_info(&mut self, text: &str) {
        self.selection_info = text.into();
    }

    pub fn set_quantize_text(&mut self, text: &str) {
        self.quantize_text = text.into();
    }

    pub fn set_resolution_text(&mut self, text: &str) {
        self.resolution_text = text.into();
    }

    pub fn set_fps_text(&mut self, text: &str) {
        self.fps_text = text.into();
    }

    pub fn set_render_scale(&mut self, scale: f32) {
        self.current_render_scale = scale;
    }

    pub fn set_tonemap_curve(&mut self, curve: TonemapCurve) {
        self.current_tonemap_curve = curve;
    }

    // ── Styles ──────────────────────────────────────────────────────

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

    /// Footer button with an active-state background (render scale / tonemap).
    fn active_button_style(active: bool) -> UIStyle {
        UIStyle {
            bg_color: if active {
                FOOTER_SCALE_ACTIVE
            } else {
                color::BUTTON_INACTIVE_C32
            },
            ..Self::footer_button_style()
        }
    }

    // ── View description ────────────────────────────────────────────

    fn dim_label(text: &str, w: f32) -> View {
        View::label(text)
            .w(Sizing::Fixed(w))
            .fill_h()
            .font(FOOTER_FONT)
            .text_color(color::TEXT_DIMMED_C32)
            .align_text(TextAlign::Right)
    }

    fn scale_btn(&self, label: &str, scale: f32) -> View {
        let active = (scale - self.current_render_scale).abs() < 0.01;
        View::button(label)
            .w(Sizing::Fixed(SCALE_BUTTON_W))
            .fill_h()
            .style(Self::active_button_style(active))
            .on_click(PanelAction::SetRenderScale(scale))
    }

    fn tonemap_btn(&self, label: &str, curve: TonemapCurve) -> View {
        let active = curve == self.current_tonemap_curve;
        View::button(label)
            .w(Sizing::Fixed(TONEMAP_BUTTON_W))
            .fill_h()
            .style(Self::active_button_style(active))
            .on_click(PanelAction::SetTonemapCurve(curve))
    }

    /// The whole footer, described once. The outer row insets by the footer's
    /// padding; a left-filling selection label pushes the right-hand groups to
    /// the edge, each group a nested row holding its own fixed-width cells —
    /// reproducing the old right-to-left pixel layout through the layout engine.
    fn view(&self) -> View {
        let quantize = View::row(LABEL_GAP)
            .fill_h()
            .child(Self::dim_label("Q:", QUANTIZE_LABEL_W))
            .child(
                View::button(self.quantize_text.as_str())
                    .w(Sizing::Fixed(QUANTIZE_BUTTON_W))
                    .fill_h()
                    .style(Self::footer_button_style())
                    .on_click(PanelAction::CycleQuantize),
            );

        let resolution = View::row(LABEL_GAP)
            .fill_h()
            .child(Self::dim_label("RES:", RESOLUTION_LABEL_W))
            .child(
                View::button(self.resolution_text.as_str())
                    .w(Sizing::Fixed(RESOLUTION_BUTTON_W))
                    .fill_h()
                    .style(Self::footer_button_style())
                    .on_click(PanelAction::ResolutionClicked),
            );

        let scale = View::row(SCALE_BTN_GAP)
            .fill_h()
            .child(self.scale_btn("1\u{00D7}", 1.0))
            .child(self.scale_btn("75%", 0.75))
            .child(self.scale_btn("50%", 0.5));

        let tonemap = View::row(LABEL_GAP)
            .fill_h()
            .child(Self::dim_label("TM:", TONEMAP_LABEL_W))
            .child(
                View::row(TONEMAP_BTN_GAP)
                    .fill_h()
                    .child(self.tonemap_btn("ACE", TonemapCurve::AcesNarkowicz))
                    .child(self.tonemap_btn("Hill", TonemapCurve::AcesHill))
                    .child(self.tonemap_btn("AgX", TonemapCurve::Agx))
                    .child(self.tonemap_btn("Khr", TonemapCurve::KhronosPbrNeutral)),
            );

        let fps = View::row(LABEL_GAP)
            .fill_h()
            .child(Self::dim_label("FPS:", FPS_LABEL_W))
            .child(
                View::button(self.fps_text.as_str())
                    .w(Sizing::Fixed(FPS_FIELD_W))
                    .fill_h()
                    .style(Self::footer_button_style())
                    .on_click(PanelAction::FpsFieldClicked)
                    .key(KEY_FPS_FIELD),
            );

        View::row(SECTION_SPACER)
            .fill()
            .bg(color::PANEL_BG_DARK)
            .pad(Pad { l: PAD, t: ELEM_Y_PAD, r: RIGHT_GUTTER, b: ELEM_Y_PAD })
            .child(
                View::label(self.selection_info.as_str())
                    .fill_w()
                    .fill_h()
                    .font(FOOTER_FONT)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(quantize)
            .child(resolution)
            .child(scale)
            .child(tonemap)
            .child(fps)
    }
}

impl Default for FooterPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for FooterPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.rect = layout.footer();
        let view = self.view();
        self.host.build(tree, &view, self.rect);
    }

    fn update(&mut self, tree: &mut UITree) {
        if !self.host.is_built() {
            return;
        }
        let view = self.view();
        let reconcile = self.host.update(tree, &view, self.rect);
        debug_assert_eq!(
            reconcile,
            Reconcile::Updated,
            "footer structure is invariant per frame — value changes update in place"
        );
    }

    /// Footer is fully intent-dispatched (see `register_intents`); clicks resolve
    /// centrally and never reach a panel handler. Required trait no-op.
    fn handle_event(&mut self, _event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        Vec::new()
    }

    /// Intent dispatch is baked into the `view()` (`.on_click(...)` per button);
    /// the host copies it into the registry. The sole click path.
    fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        self.host.register_intents(intents);
    }

    fn first_node(&self) -> usize {
        self.host.first_node()
    }
    fn node_count(&self) -> usize {
        self.host.node_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{Gesture, IntentRegistry};

    // ── Golden oracle: the original constant-based right-to-left layout ──
    // Retained as the regression check the Chrome `view()` must reproduce
    // exactly. The live panel no longer uses it — it exists only to prove the
    // declarative layout lands every interactive cell where the hand-tuned
    // pixel math did.
    #[derive(Default)]
    struct FooterGolden {
        quantize_button: Rect,
        resolution_button: Rect,
        scale_100: Rect,
        scale_75: Rect,
        scale_50: Rect,
        tonemap_aces: Rect,
        tonemap_hill: Rect,
        tonemap_agx: Rect,
        tonemap_khr: Rect,
        fps_field: Rect,
    }

    impl FooterGolden {
        fn compute(&mut self, bounds: Rect) {
            let elem_h = bounds.height - ELEM_Y_PAD * 2.0;
            let y = bounds.y + ELEM_Y_PAD;
            let mut rx = bounds.x_max() - RIGHT_GUTTER;

            rx -= FPS_FIELD_W;
            self.fps_field = Rect::new(rx, y, FPS_FIELD_W, elem_h);
            rx -= LABEL_GAP;
            rx -= FPS_LABEL_W;
            rx -= SECTION_SPACER;

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
            rx -= SECTION_SPACER;

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
            rx -= SECTION_SPACER;

            rx -= QUANTIZE_BUTTON_W;
            self.quantize_button = Rect::new(rx, y, QUANTIZE_BUTTON_W, elem_h);
        }
    }

    /// Every Button node in the tree, as (bounds, text), sorted left-to-right.
    fn buttons(tree: &UITree) -> Vec<(Rect, String)> {
        let mut v: Vec<(Rect, String)> = (0..tree.count())
            .map(|i| tree.get_node(NodeId(i as u32)))
            .filter(|n| n.node_type == UINodeType::Button)
            .map(|n| (n.bounds, n.text.clone().unwrap_or_default()))
            .collect();
        v.sort_by(|a, b| a.0.x.partial_cmp(&b.0.x).unwrap());
        v
    }

    #[test]
    fn build_creates_panel() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        assert!(panel.host.is_built());
        // bg + selection + 5 groups (with the tonemap sub-row) + 10 buttons + 4 labels.
        assert_eq!(buttons(&tree).len(), 10, "10 footer buttons");
        assert!(panel.fps_field_id().is_some(), "fps field resolves by key");
    }

    #[test]
    fn chrome_layout_matches_golden() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let mut g = FooterGolden::default();
        g.compute(layout.footer());

        let want = [
            (g.quantize_button, "Off"),
            (g.resolution_button, "1080p"),
            (g.scale_100, "1\u{00D7}"),
            (g.scale_75, "75%"),
            (g.scale_50, "50%"),
            (g.tonemap_aces, "ACE"),
            (g.tonemap_hill, "Hill"),
            (g.tonemap_agx, "AgX"),
            (g.tonemap_khr, "Khr"),
            (g.fps_field, "60"),
        ];
        let mut want: Vec<(Rect, String)> =
            want.iter().map(|(r, t)| (*r, t.to_string())).collect();
        want.sort_by(|a, b| a.0.x.partial_cmp(&b.0.x).unwrap());

        let got = buttons(&tree);
        assert_eq!(got.len(), want.len());
        for ((gr, gt), (wr, wt)) in got.iter().zip(want.iter()) {
            assert_eq!(gt, wt, "button text mismatch");
            assert!(
                (gr.x - wr.x).abs() < 0.01
                    && (gr.y - wr.y).abs() < 0.01
                    && (gr.width - wr.width).abs() < 0.01
                    && (gr.height - wr.height).abs() < 0.01,
                "button '{gt}' at {gr:?} != golden {wr:?}"
            );
        }
    }

    #[test]
    fn intents_resolve_through_registry() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        let fps = intents.resolve(&tree, panel.fps_field_id(), Gesture::Click);
        assert!(matches!(fps, Some(PanelAction::FpsFieldClicked)));
        assert!(intents.resolve(&tree, None, Gesture::Click).is_none());
    }

    #[test]
    fn value_change_updates_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);
        let count_after_build = tree.count();
        let fps_id = panel.fps_field_id().unwrap();
        let sv = tree.structure_version();

        panel.set_fps_text("30 FPS");
        panel.update(&mut tree);

        assert_eq!(tree.count(), count_after_build, "no nodes added");
        assert_eq!(
            tree.structure_version(),
            sv,
            "value change must not bump structure_version"
        );
        assert_eq!(tree.get_node(fps_id).text.as_deref(), Some("30 FPS"));
    }

    #[test]
    fn render_scale_highlight_is_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = FooterPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        panel.set_render_scale(0.5);
        panel.update(&mut tree);

        // Structure unchanged (only the active button's bg color moved).
        assert_eq!(tree.structure_version(), sv);
    }
}
