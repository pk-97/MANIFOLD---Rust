//! Header bar on the declarative Chrome API.
//!
//! Three positioning regimes — a left-flowing group, an absolutely-centered
//! time display, and a right-to-left button group — expressed as a `Stack` of
//! three `Fill` rows (left/center/right aligned) inset by a symmetric padding,
//! so the centered group lands at true screen-centre. See the footer for the
//! integration pattern and `docs/CHROME_API_DESIGN.md`.

use crate::{RootAction, TransportAction};
use super::{Panel, PanelAction};
use crate::chrome::{Align, ChromeHost, Pad, Reconcile, Sizing, View, components};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants (from HeaderLayout.cs) ────────────────────────

const INSET: f32 = color::SPACE_M;
const GROUP_Y_PAD: f32 = color::SPACE_S; // section 14.4: 5 → 4
const GROUP_SPACING: f32 = color::SPACE_S; // section 14.4: 5 → 4

const PROJECT_NAME_W: f32 = 200.0;
const SPACER: f32 = color::SPACE_M;
const IMPORT_STATUS_W: f32 = 180.0;
const PROGRESS_BAR_W: f32 = 140.0;
const PROGRESS_BAR_H: f32 = 10.0;
const PROGRESS_BAR_INSET: f32 = 5.0;

const ZOOM_BUTTON_W: f32 = 28.0;
const ZOOM_LABEL_W: f32 = 70.0;
const ZOOM_CLUSTER_W: f32 = ZOOM_BUTTON_W * 2.0 + ZOOM_LABEL_W;

const TIME_DISPLAY_W: f32 = 260.0;

// ── Panel-specific colors ──────────────────────────────────────────

const PROGRESS_FILL: Color32 = color::HEADER_PROGRESS_FILL;

const PROGRESS_RADIUS: f32 = 2.0;

// ── HeaderPanel ────────────────────────────────────────────────────

pub struct HeaderPanel {
    host: ChromeHost,
    rect: Rect,

    // Display state.
    project_name: String,
    import_status: String,
    import_progress: f32,
    import_progress_visible: bool,
    time_display: String,
    zoom_label: String,
    /// Whether the Audio Setup / Scene Setup docks are open — drives the two
    /// header toggle buttons' active-state highlight (D2: "beside the Audio
    /// button", mutually exclusive so at most one is ever true).
    audio_setup_open: bool,
    scene_setup_open: bool,
}

impl HeaderPanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            rect: Rect::ZERO,
            project_name: "My Project".into(),
            import_status: String::new(),
            import_progress: 0.0,
            import_progress_visible: false,
            time_display: "00:00.00 / 00:00.00  |  1.1.1".into(),
            zoom_label: "120 px/beat".into(),
            audio_setup_open: false,
            scene_setup_open: false,
        }
    }

    // ── State setters (store only; the reconcile applies them) ──────

    pub fn set_project_name(&mut self, name: &str) {
        self.project_name = name.into();
    }

    pub fn set_import_status(&mut self, status: &str, progress: f32, show: bool) {
        self.import_status = status.into();
        self.import_progress = progress.clamp(0.0, 1.0);
        self.import_progress_visible = show;
    }

    pub fn set_time_display(&mut self, text: &str) {
        self.time_display = text.into();
    }

    pub fn set_zoom_label(&mut self, text: &str) {
        self.zoom_label = text.into();
    }

    /// Set the two utility-dock toggle buttons' active state (D2: mutually
    /// exclusive — the app dispatch guarantees at most one is ever true, this
    /// setter doesn't enforce it, it just paints whatever it's told).
    pub fn set_dock_toggle_state(&mut self, audio_setup_open: bool, scene_setup_open: bool) {
        self.audio_setup_open = audio_setup_open;
        self.scene_setup_open = scene_setup_open;
    }

    // ── Styles ──────────────────────────────────────────────────────

    // The zoom −/+ are neutral chrome chips — no state colour — sharing the kit's
    // off-state chip (the same `BUTTON_DIM` 71-grey as the transport bar and
    // layer-card mixer). One neutral chip across every top chrome bar.
    fn zoom_button_style() -> UIStyle {
        UIStyle {
            font_size: color::FONT_TITLE,
            ..components::state_button_style(color::BUTTON_DIM, false)
        }
    }

    /// Style for the Audio/Scene dock toggle buttons — the neutral chip,
    /// raised to the active-state colour while its dock is open.
    fn dock_toggle_style(active: bool) -> UIStyle {
        UIStyle {
            font_size: color::FONT_LABEL,
            ..components::state_button_style(color::BUTTON_DIM, active)
        }
    }

    // ── View description ────────────────────────────────────────────

    fn spacer_fixed(w: f32) -> View {
        View::panel().w(Sizing::Fixed(w)).fill_h()
    }

    fn left_group(&self, available_w: f32, right_w: f32) -> (View, f32) {
        let room = (available_w - right_w).max(0.0);
        let compact = room
            < PROJECT_NAME_W + SPACER + IMPORT_STATUS_W + PROGRESS_BAR_INSET + PROGRESS_BAR_W;
        let project_w = if compact { room.min(140.0) } else { PROJECT_NAME_W };
        let detail_visible = !compact;
        let spacer_w = if compact { 0.0 } else { SPACER };
        let progress_inset = if detail_visible { PROGRESS_BAR_INSET } else { 0.0 };
        let status_w = if detail_visible { IMPORT_STATUS_W } else { 0.0 };
        let progress_w = if detail_visible { PROGRESS_BAR_W } else { 0.0 };
        let used_w = project_w + spacer_w + status_w + progress_inset + progress_w;

        // Progress bar: fixed track with an inset fill scaled by progress, both
        // hidden until an import is running.
        let visible = self.import_progress_visible;
        let fill_w = (progress_w - 2.0).max(0.0) * self.import_progress;
        let progress = View::panel()
            .fixed(progress_w, PROGRESS_BAR_H)
            .bg(color::SLIDER_TRACK_PRESSED_C32)
            .radius(PROGRESS_RADIUS)
            .visible(visible && detail_visible)
            .pad(Pad::all(1.0))
            .child(
                View::panel()
                    .w(Sizing::Fixed(fill_w))
                    .fill_h()
                    .bg(PROGRESS_FILL)
                    .radius(color::HAIRLINE_RADIUS)
                    .visible(visible),
            );

        (
            View::row(0.0)
                .fill()
                .main_align(Align::Start)
                .cross_align(Align::Center)
                .child(
                    View::label(self.project_name.as_str())
                        .w(Sizing::Fixed(project_w))
                        .fill_h()
                        .font(color::FONT_SUBHEADING)
                        .text_color(color::TEXT_DIMMED_C32),
                )
                .child(Self::spacer_fixed(spacer_w))
                .child(
                    View::label(self.import_status.as_str())
                        .w(Sizing::Fixed(status_w))
                        .fill_h()
                        .font(color::FONT_LABEL)
                        .text_color(color::TEXT_DIMMED_C32),
                )
                .child(Self::spacer_fixed(progress_inset))
                .child(progress),
            used_w,
        )
    }

    fn center_group(&self, available_w: f32, left_w: f32, right_w: f32) -> View {
        let center_w = TIME_DISPLAY_W.min(
            (available_w - 2.0 * left_w)
                .min(available_w - 2.0 * right_w)
                .max(0.0),
        );
        let time_text = if center_w < 72.0 {
            ""
        } else if center_w < TIME_DISPLAY_W {
            self.time_display
                .split(" / ")
                .next()
                .unwrap_or(self.time_display.as_str())
        } else {
            self.time_display.as_str()
        };
        View::row(0.0).fill().main_align(Align::Center).child(
            View::label(time_text)
                .w(Sizing::Fixed(center_w))
                .fill_h()
                .font(color::FONT_HEADING)
                .text_color(color::TEXT_PRIMARY_C32)
                .align_text(TextAlign::Center),
        )
    }

    fn right_group(&self, available_w: f32) -> (View, f32) {
        let dock_button_w = ((available_w - ZOOM_CLUSTER_W - GROUP_SPACING) * 0.5)
            .clamp(0.0, 60.0);
        let dock_visible = dock_button_w >= 24.0;
        let dock_gap = if dock_visible { color::SPACE_XS } else { 0.0 };
        let audio_label = if dock_button_w >= 50.0 { "Audio" } else { "A" };
        let scene_label = if dock_button_w >= 50.0 { "Scene" } else { "S" };
        // Utility-dock toggles (SCENE_SETUP_PANEL_DESIGN D2): "Audio" and
        // "Scene" sit side by side while there is room for their compact
        // labels; the zoom cluster keeps its full button affordances first.
        // They remain highlighted when their dock is open and are also
        // reachable via the View menu (⌘⇧A for Audio).
        let dock_toggles = View::row(dock_gap)
            .fill_h()
            .child(
                View::button(audio_label)
                    .w(Sizing::Fixed(dock_button_w))
                    .fill_h()
                    .visible(dock_visible)
                    .style(Self::dock_toggle_style(self.audio_setup_open))
                    .on_click(PanelAction::Root(RootAction::OpenAudioSetup)),
            )
            .child(
                View::button(scene_label)
                    .w(Sizing::Fixed(dock_button_w))
                    .fill_h()
                    .visible(dock_visible)
                    .style(Self::dock_toggle_style(self.scene_setup_open))
                    .on_click(PanelAction::Root(RootAction::OpenSceneSetup)),
            );

        // Tight zoom cluster [−][label][+], end-aligned to the inset right edge.
        let zoom_label_w = (available_w
            - dock_button_w * 2.0
            - dock_gap
            - GROUP_SPACING
            - ZOOM_BUTTON_W * 2.0)
            .clamp(0.0, ZOOM_LABEL_W);
        let zoom_cluster = View::row(0.0)
            .fill_h()
            .child(
                View::button("\u{2212}")
                    .w(Sizing::Fixed(ZOOM_BUTTON_W))
                    .fill_h()
                    .style(Self::zoom_button_style())
                    .on_click(PanelAction::Transport(TransportAction::ZoomOut)),
            )
            .child(
                View::label(self.zoom_label.as_str())
                    .w(Sizing::Fixed(zoom_label_w))
                    .fill_h()
                    .font(color::FONT_SUBHEADING)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Center),
            )
            .child(
                View::button("+")
                    .w(Sizing::Fixed(ZOOM_BUTTON_W))
                    .fill_h()
                    .style(Self::zoom_button_style())
                    .on_click(PanelAction::Transport(TransportAction::ZoomIn)),
            );

        let used_w = dock_button_w * 2.0
            + dock_gap
            + GROUP_SPACING
            + ZOOM_BUTTON_W * 2.0
            + zoom_label_w;
        (
            View::row(GROUP_SPACING)
                .fill()
                .main_align(Align::End)
                .child(dock_toggles)
                .child(zoom_cluster),
            used_w,
        )
    }

    fn view(&self) -> View {
        let available_w = (self.rect.width - 2.0 * INSET).max(0.0);
        let (right, right_w) = self.right_group(available_w);
        let (left, left_w) = self.left_group(available_w, right_w);
        View::stack()
            .fill()
            .bg(color::PANEL_BG_DARK)
            .border(color::BORDER, 1.0)
            .pad(Pad { l: INSET, t: GROUP_Y_PAD, r: INSET, b: GROUP_Y_PAD })
            .child(left)
            .child(self.center_group(available_w, left_w, right_w))
            .child(right)
    }
}

impl Default for HeaderPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for HeaderPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.rect = layout.header();
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
            "header structure is invariant per frame — value/visibility changes update in place"
        );
    }

    /// Header is fully intent-dispatched (see `register_intents`). Required no-op.
    fn handle_event(&mut self, _event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        Vec::new()
    }

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

    // Golden oracle: the original right-to-left button positions. The Chrome
    // `view()` must reproduce every interactive cell at the same rect.
    #[derive(Default)]
    struct HeaderGolden {
        time_display: Rect,
        zoom_out: Rect,
        zoom_in: Rect,
    }

    impl HeaderGolden {
        fn compute(&mut self, bounds: Rect) {
            let elem_h = bounds.height - GROUP_Y_PAD * 2.0;
            let elem_y = bounds.y + GROUP_Y_PAD;

            let cx = bounds.x + (bounds.width - TIME_DISPLAY_W) * 0.5;
            self.time_display = Rect::new(cx, elem_y, TIME_DISPLAY_W, elem_h);

            // Right edge: [−][label][+] zoom cluster, inset from the bar end.
            let mut rx = bounds.x_max() - INSET;
            rx -= ZOOM_BUTTON_W;
            self.zoom_in = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);
            rx -= ZOOM_LABEL_W;
            rx -= ZOOM_BUTTON_W;
            self.zoom_out = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);
        }
    }

    fn node_with_text<'a>(tree: &'a UITree, text: &str) -> &'a crate::node::UINode {
        (0..tree.count())
            .filter_map(|i| tree.get_node(tree.id_at(i)))
            .find(|n| n.text.as_deref() == Some(text))
            .unwrap_or_else(|| panic!("no node with text {text:?}"))
    }

    fn assert_rect(a: Rect, b: Rect, what: &str) {
        assert!(
            (a.x - b.x).abs() < 0.01
                && (a.y - b.y).abs() < 0.01
                && (a.width - b.width).abs() < 0.01
                && (a.height - b.height).abs() < 0.01,
            "{what}: {a:?} != golden {b:?}"
        );
    }

    #[test]
    fn chrome_layout_matches_golden() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let mut g = HeaderGolden::default();
        g.compute(layout.header());

        assert_rect(node_with_text(&tree, "\u{2212}").bounds, g.zoom_out, "zoom_out");
        assert_rect(node_with_text(&tree, "+").bounds, g.zoom_in, "zoom_in");
        // Centered time display lands at true screen centre despite the inset.
        assert_rect(
            node_with_text(&tree, "00:00.00 / 00:00.00  |  1.1.1").bounds,
            g.time_display,
            "time_display",
        );
    }

    #[test]
    fn intents_resolve_through_registry() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        let zin = node_with_text(&tree, "+").id;
        assert!(matches!(
            intents.resolve(&tree, Some(zin), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::ZoomIn))
        ));
    }

    #[test]
    fn value_change_updates_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);
        let count = tree.count();
        let sv = tree.structure_version();

        panel.set_time_display("01:30.50  |  4.2.3");
        panel.set_project_name("Live Set");
        panel.update(&mut tree);

        assert_eq!(tree.count(), count, "no nodes added");
        assert_eq!(tree.structure_version(), sv, "no structure bump");
        assert_eq!(
            node_with_text(&tree, "01:30.50  |  4.2.3").text.as_deref(),
            Some("01:30.50  |  4.2.3")
        );
    }

    #[test]
    fn progress_toggle_is_in_place() {
        // Showing/hiding the import progress bar is a visibility change, not a
        // structural one (the nodes are always emitted).
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        panel.set_import_status("Decoding…", 0.5, true);
        panel.update(&mut tree);

        assert_eq!(tree.structure_version(), sv, "progress toggle must not rebuild");
    }

    #[test]
    fn narrow_content_keeps_zoom_cluster_inside_header() {
        let mut tree = UITree::new();
        let mut layout = ScreenLayout::new(1440.0, 900.0);
        layout.scene_setup_width = color::DEFAULT_SCENE_SETUP_WIDTH;
        let header = layout.header();
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let zoom_out = node_with_text(&tree, "\u{2212}").bounds;
        let zoom_in = node_with_text(&tree, "+").bounds;
        let project = node_with_text(&tree, "My Project").bounds;
        assert!(zoom_out.x >= header.x);
        assert!(zoom_in.x + zoom_in.width <= header.x + header.width);
        assert!(project.x + project.width <= zoom_out.x);
        assert!(zoom_in.x > zoom_out.x, "zoom controls retain left-to-right order");
    }

}
