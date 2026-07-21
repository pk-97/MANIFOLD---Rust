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
const GROUP_Y_PAD: f32 = color::SPACE_S; // §14.4: 5 → 4
const GROUP_SPACING: f32 = color::SPACE_S; // §14.4: 5 → 4

const PROJECT_NAME_W: f32 = 200.0;
const SPACER: f32 = color::SPACE_M;
const IMPORT_STATUS_W: f32 = 180.0;
const PROGRESS_BAR_W: f32 = 140.0;
const PROGRESS_BAR_H: f32 = 10.0;
const PROGRESS_BAR_INSET: f32 = 5.0;

// export progress strip, right of centre (mirrors the import
// status + progress bar above, same visibility-toggle-not-rebuild pattern).
const EXPORT_STATUS_W: f32 = 220.0;

const ZOOM_BUTTON_W: f32 = 28.0;
const ZOOM_LABEL_W: f32 = 70.0;

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
    /// export status text + progress (0.0..1.0), same
    /// always-emit/toggle-visibility pattern as the import fields above.
    export_status: String,
    export_progress: f32,
    export_progress_visible: bool,
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
            export_status: String::new(),
            export_progress: 0.0,
            export_progress_visible: false,
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

    /// export progress strip. `show` is `content_state.is_exporting`;
    /// `status`/`progress` come straight off the content thread's
    /// `send_export_progress` snapshots (`export_status`/`export_progress`).
    pub fn set_export_status(&mut self, status: &str, progress: f32, show: bool) {
        self.export_status = status.into();
        self.export_progress = progress.clamp(0.0, 1.0);
        self.export_progress_visible = show;
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

    fn left_group(&self) -> View {
        // Progress bar: fixed track with an inset fill scaled by progress, both
        // hidden until an import is running.
        let visible = self.import_progress_visible;
        let fill_w = (PROGRESS_BAR_W - 2.0) * self.import_progress;
        let progress = View::panel()
            .fixed(PROGRESS_BAR_W, PROGRESS_BAR_H)
            .bg(color::SLIDER_TRACK_PRESSED_C32)
            .radius(PROGRESS_RADIUS)
            .visible(visible)
            .pad(Pad::all(1.0))
            .child(
                View::panel()
                    .w(Sizing::Fixed(fill_w))
                    .fill_h()
                    .bg(PROGRESS_FILL)
                    .radius(color::HAIRLINE_RADIUS)
                    .visible(visible),
            );

        View::row(0.0)
            .fill()
            .main_align(Align::Start)
            .cross_align(Align::Center)
            .child(
                View::label(self.project_name.as_str())
                    .w(Sizing::Fixed(PROJECT_NAME_W))
                    .fill_h()
                    .font(color::FONT_SUBHEADING)
                    .text_color(color::TEXT_DIMMED_C32),
            )
            .child(Self::spacer_fixed(SPACER))
            .child(
                View::label(self.import_status.as_str())
                    .w(Sizing::Fixed(IMPORT_STATUS_W))
                    .fill_h()
                    .font(color::FONT_LABEL)
                    .text_color(color::TEXT_DIMMED_C32),
            )
            .child(Self::spacer_fixed(PROGRESS_BAR_INSET))
            .child(progress)
    }

    fn center_group(&self) -> View {
        View::row(0.0).fill().main_align(Align::Center).child(
            View::label(self.time_display.as_str())
                .w(Sizing::Fixed(TIME_DISPLAY_W))
                .fill_h()
                .font(color::FONT_HEADING)
                .text_color(color::TEXT_PRIMARY_C32)
                .align_text(TextAlign::Center),
        )
    }

    fn right_group(&self) -> View {
        // export progress strip — same fixed-track/inset-fill
        // pattern as `left_group`'s import progress bar, always emitted and
        // shown/hidden via `.visible()` so toggling it never rebuilds the
        // tree (mirrors `progress_toggle_is_in_place` for the import bar).
        let export_visible = self.export_progress_visible;
        let export_fill_w = (PROGRESS_BAR_W - 2.0) * self.export_progress;
        let export_progress = View::panel()
            .fixed(PROGRESS_BAR_W, PROGRESS_BAR_H)
            .bg(color::SLIDER_TRACK_PRESSED_C32)
            .radius(PROGRESS_RADIUS)
            .visible(export_visible)
            .pad(Pad::all(1.0))
            .child(
                View::panel()
                    .w(Sizing::Fixed(export_fill_w))
                    .fill_h()
                    .bg(PROGRESS_FILL)
                    .radius(color::HAIRLINE_RADIUS)
                    .visible(export_visible),
            );
        let export_group = View::row(0.0)
            .fill_h()
            .cross_align(Align::Center)
            .child(
                View::label(self.export_status.as_str())
                    .w(Sizing::Fixed(EXPORT_STATUS_W))
                    .fill_h()
                    .font(color::FONT_LABEL)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Right),
            )
            .child(Self::spacer_fixed(PROGRESS_BAR_INSET))
            .child(export_progress);

        // Utility-dock toggles (SCENE_SETUP_PANEL_DESIGN D2): "Audio" and
        // "Scene" sit side by side, both always present (never conditionally
        // hidden — `feedback_no_conditionally_visible_ui`), highlighted when
        // their dock is open. Also reachable via the View menu (⌘⇧A for
        // Audio) — this is the on-screen affordance the docs assume exists
        // beside each other.
        let dock_toggles = View::row(color::SPACE_XS)
            .fill_h()
            .child(
                View::button("Audio")
                    .w(Sizing::Fixed(60.0))
                    .fill_h()
                    .style(Self::dock_toggle_style(self.audio_setup_open))
                    .on_click(PanelAction::Root(RootAction::OpenAudioSetup)),
            )
            .child(
                View::button("Scene")
                    .w(Sizing::Fixed(60.0))
                    .fill_h()
                    .style(Self::dock_toggle_style(self.scene_setup_open))
                    .on_click(PanelAction::Root(RootAction::OpenSceneSetup)),
            );

        // Tight zoom cluster [−][label][+], end-aligned to the inset right edge.
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
                    .w(Sizing::Fixed(ZOOM_LABEL_W))
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

        View::row(GROUP_SPACING)
            .fill()
            .main_align(Align::End)
            .child(export_group)
            .child(dock_toggles)
            .child(zoom_cluster)
    }

    fn view(&self) -> View {
        View::stack()
            .fill()
            .bg(color::PANEL_BG_DARK)
            .border(color::BORDER, 1.0)
            .pad(Pad { l: INSET, t: GROUP_Y_PAD, r: INSET, b: GROUP_Y_PAD })
            .child(self.left_group())
            .child(self.center_group())
            .child(self.right_group())
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

    /// the export progress strip follows the same
    /// always-emit/toggle-visibility contract as the import bar above, and
    /// its text is actually reachable via the tree.
    #[test]
    fn export_progress_toggle_is_in_place_and_text_updates() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        panel.set_export_status("Exporting 120/600 (20%)", 0.20, true);
        panel.update(&mut tree);

        assert_eq!(
            tree.structure_version(),
            sv,
            "export progress toggle must not rebuild"
        );
        assert_eq!(
            node_with_text(&tree, "Exporting 120/600 (20%)").text.as_deref(),
            Some("Exporting 120/600 (20%)")
        );
    }
}
