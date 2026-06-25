//! Header bar on the declarative Chrome API.
//!
//! Three positioning regimes — a left-flowing group, an absolutely-centered
//! time display, and a right-to-left button group — expressed as a `Stack` of
//! three `Fill` rows (left/center/right aligned) inset by a symmetric padding,
//! so the centered group lands at true screen-centre. See the footer for the
//! integration pattern and `docs/CHROME_API_DESIGN.md`.

use super::{Panel, PanelAction};
use crate::chrome::{Align, ChromeHost, Pad, Reconcile, Sizing, View};
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

const ZOOM_BUTTON_W: f32 = 28.0;
const ZOOM_LABEL_W: f32 = 70.0;
const AUDIO_BUTTON_W: f32 = 60.0;
const MONITOR_BUTTON_W: f32 = 60.0;
const PERFORM_BUTTON_W: f32 = 60.0;

const TIME_DISPLAY_W: f32 = 260.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_DIM: Color32 = color::HEADER_BUTTON_DIM;
const BUTTON_HOVER_H: Color32 = color::HEADER_BUTTON_HOVER;
const BUTTON_PRESSED_H: Color32 = color::HEADER_BUTTON_PRESSED;
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

    // ── Styles ──────────────────────────────────────────────────────

    fn action_button_style() -> UIStyle {
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

    fn zoom_button_style() -> UIStyle {
        UIStyle {
            bg_color: BUTTON_DIM,
            hover_bg_color: BUTTON_HOVER_H,
            pressed_bg_color: BUTTON_PRESSED_H,
            text_color: color::TEXT_WHITE_C32,
            font_size: color::FONT_TITLE,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
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
        // Tight zoom cluster [−][label][+] with no inter-gaps, then the spaced
        // Audio / Perform / Monitor buttons. End-aligned to the inset right edge.
        let zoom_cluster = View::row(0.0)
            .fill_h()
            .child(
                View::button("\u{2212}")
                    .w(Sizing::Fixed(ZOOM_BUTTON_W))
                    .fill_h()
                    .style(Self::zoom_button_style())
                    .on_click(PanelAction::ZoomOut),
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
                    .on_click(PanelAction::ZoomIn),
            );

        let action = |label: &str, w: f32, act: PanelAction| {
            View::button(label)
                .w(Sizing::Fixed(w))
                .fill_h()
                .style(Self::action_button_style())
                .on_click(act)
        };

        View::row(GROUP_SPACING)
            .fill()
            .main_align(Align::End)
            .child(zoom_cluster)
            .child(action("Audio", AUDIO_BUTTON_W, PanelAction::OpenAudioSetup))
            .child(action("Perform", PERFORM_BUTTON_W, PanelAction::EnterPerformMode))
            .child(action("Monitor", MONITOR_BUTTON_W, PanelAction::ToggleMonitor))
    }

    fn view(&self) -> View {
        View::stack()
            .fill()
            .bg(color::PANEL_BG_DARK)
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
        audio: Rect,
        perform: Rect,
        monitor: Rect,
    }

    impl HeaderGolden {
        fn compute(&mut self, bounds: Rect) {
            let elem_h = bounds.height - GROUP_Y_PAD * 2.0;
            let elem_y = bounds.y + GROUP_Y_PAD;

            let cx = bounds.x + (bounds.width - TIME_DISPLAY_W) * 0.5;
            self.time_display = Rect::new(cx, elem_y, TIME_DISPLAY_W, elem_h);

            let mut rx = bounds.x_max() - INSET;
            rx -= MONITOR_BUTTON_W;
            self.monitor = Rect::new(rx, elem_y, MONITOR_BUTTON_W, elem_h);
            rx -= GROUP_SPACING;
            rx -= PERFORM_BUTTON_W;
            self.perform = Rect::new(rx, elem_y, PERFORM_BUTTON_W, elem_h);
            rx -= GROUP_SPACING;
            rx -= AUDIO_BUTTON_W;
            self.audio = Rect::new(rx, elem_y, AUDIO_BUTTON_W, elem_h);
            rx -= GROUP_SPACING;
            rx -= ZOOM_BUTTON_W;
            self.zoom_in = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);
            rx -= ZOOM_LABEL_W;
            rx -= ZOOM_BUTTON_W;
            self.zoom_out = Rect::new(rx, elem_y, ZOOM_BUTTON_W, elem_h);
        }
    }

    fn node_with_text<'a>(tree: &'a UITree, text: &str) -> &'a crate::node::UINode {
        (0..tree.count())
            .map(|i| tree.get_node(tree.id_at(i)))
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
        assert_rect(node_with_text(&tree, "Audio").bounds, g.audio, "audio");
        assert_rect(node_with_text(&tree, "Perform").bounds, g.perform, "perform");
        assert_rect(node_with_text(&tree, "Monitor").bounds, g.monitor, "monitor");
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

        let audio_id = node_with_text(&tree, "Audio").id;
        assert!(matches!(
            intents.resolve(&tree, Some(audio_id), Gesture::Click),
            Some(PanelAction::OpenAudioSetup)
        ));
        let zin = node_with_text(&tree, "+").id;
        assert!(matches!(
            intents.resolve(&tree, Some(zin), Gesture::Click),
            Some(PanelAction::ZoomIn)
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
}
