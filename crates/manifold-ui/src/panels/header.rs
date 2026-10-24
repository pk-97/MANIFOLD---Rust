//! Header bar on the declarative Chrome API.
//!
//! Three positioning regimes — a left-flowing group, an absolutely-centered
//! time display, and a right-to-left button group — expressed as a `Stack` of
//! three `Fill` rows (left/center/right aligned) inset by a symmetric padding,
//! so the centered group lands at true screen-centre. See the footer for the
//! integration pattern and `docs/CHROME_API_DESIGN.md`.

use super::{Panel, PanelAction};
use crate::chrome::{Align, ChromeHost, Pad, Reconcile, Sizing, View, components};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use crate::{RootAction, TransportAction};

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
/// The clock label's host key, for its in-place text writes.
const CLOCK_KEY: u64 = 1;
const ZOOM_KEY: u64 = 2;

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
    /// The clock slot's width at the last layout, which picks how much of
    /// `time_display` it shows.
    clock_w: f32,
    zoom_label: String,
    /// The label being formatted, so a zoom writes no new string.
    zoom_scratch: String,
    /// Whether the Audio Setup dock is open — drives its button's highlight.
    audio_setup_open: bool,
}

impl HeaderPanel {
    /// The bar's region was translated or resized (UI_INVALIDATION D7): the next
    /// in-place update lays out at the rect the nodes moved to.
    pub fn follow_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Lay the bar out in place at its rect and values. `NeedsRebuild` when
    /// its structure changed; the caller rebuilds the bar then.
    pub fn relayout(&mut self, tree: &mut UITree) -> Reconcile {
        if !self.host.is_built() {
            return Reconcile::Updated;
        }
        let (view, clock_w) = self.view();
        self.clock_w = clock_w;
        self.host.update(tree, &view, self.rect)
    }

    /// The rect the bar last laid out at.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            rect: Rect::ZERO,
            project_name: "My Project".into(),
            import_status: String::new(),
            import_progress: 0.0,
            import_progress_visible: false,
            time_display: "00:00.00 / 00:00.00  |  1.1.1".into(),
            clock_w: 0.0,
            zoom_label: "120 px/beat".into(),
            zoom_scratch: String::new(),
            audio_setup_open: false,
        }
    }

    // ── State setters (store only; the reconcile applies them) ──────

    pub fn set_project_name(&mut self, name: &str) {
        self.project_name = name.into();
    }

    /// Called every frame; reuses the held string.
    pub fn set_import_status(&mut self, status: &str, progress: f32, show: bool) {
        if self.import_status != status {
            self.import_status.clear();
            self.import_status.push_str(status);
        }
        self.import_progress = progress.clamp(0.0, 1.0);
        self.import_progress_visible = show;
    }

    /// The clock moves several times a second while playing, so it writes its
    /// fixed-width label in place instead of waiting for the chrome layout,
    /// reusing the strings it already holds.
    pub fn set_time_display(&mut self, tree: &mut UITree, text: &str) {
        if self.time_display == text {
            return;
        }
        self.time_display.clear();
        self.time_display.push_str(text);
        let shown = Self::clock_text(&self.time_display, self.clock_w);
        self.host.set_text_by_key(tree, CLOCK_KEY, shown);
    }

    /// The zoom label writes its fixed-width text in place, like the clock,
    /// reusing the strings it holds.
    pub fn set_zoom_label(&mut self, tree: &mut UITree, pixels_per_beat: f32) {
        use std::fmt::Write as _;
        let decimals = if pixels_per_beat >= 1.0 { 0 } else { 2 };
        self.zoom_scratch.clear();
        let _ = write!(self.zoom_scratch, "{pixels_per_beat:.decimals$} px/beat");
        if self.zoom_label == self.zoom_scratch {
            return;
        }
        std::mem::swap(&mut self.zoom_label, &mut self.zoom_scratch);
        self.host.set_text_by_key(tree, ZOOM_KEY, &self.zoom_label);
    }

    /// Paint the Audio dock toggle's active state.
    pub fn set_audio_dock_open(&mut self, audio_setup_open: bool) {
        self.audio_setup_open = audio_setup_open;
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

    fn left_group(&self, room: f32) -> (View, f32) {
        let compact =
            room < PROJECT_NAME_W + SPACER + IMPORT_STATUS_W + PROGRESS_BAR_INSET + PROGRESS_BAR_W;
        let project_w = if compact {
            room.min(140.0)
        } else {
            PROJECT_NAME_W
        };
        let detail_visible = !compact;
        let spacer_w = if compact { 0.0 } else { SPACER };
        let progress_inset = if detail_visible {
            PROGRESS_BAR_INSET
        } else {
            0.0
        };
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

    /// What the clock slot shows of the full time display at its width.
    fn clock_text(full: &str, clock_w: f32) -> &str {
        if clock_w < 72.0 {
            ""
        } else if clock_w < TIME_DISPLAY_W {
            full.split(" / ").next().unwrap_or(full)
        } else {
            full
        }
    }

    fn center_group(&self, center_w: f32) -> View {
        View::row(0.0).fill().main_align(Align::Center).child(
            View::label(Self::clock_text(&self.time_display, center_w))
                .key(CLOCK_KEY)
                .w(Sizing::Fixed(center_w))
                .fill_h()
                .font(color::FONT_HEADING)
                .text_color(color::TEXT_PRIMARY_C32)
                .align_text(TextAlign::Center),
        )
    }

    fn right_group(&self, room: f32) -> (View, f32) {
        let dock_button_w = (room - ZOOM_CLUSTER_W - GROUP_SPACING).clamp(0.0, 60.0);
        let dock_visible = dock_button_w >= 24.0;
        let audio_label = if dock_button_w >= 50.0 { "Audio" } else { "A" };
        // The Audio dock toggle, highlighted while the dock is open; the zoom
        // cluster keeps its full button affordances first. The Scene tab
        // lives in the inspector (SCENE_SURFACES D16), so it has no button here.
        let dock_toggles = View::button(audio_label)
            .w(Sizing::Fixed(dock_button_w))
            .fill_h()
            .visible(dock_visible)
            .style(Self::dock_toggle_style(self.audio_setup_open))
            .on_click(PanelAction::Root(RootAction::OpenAudioSetup));

        // Tight zoom cluster [−][label][+], end-aligned to the inset right edge.
        let zoom_label_w =
            (room - dock_button_w - GROUP_SPACING - ZOOM_BUTTON_W * 2.0).clamp(0.0, ZOOM_LABEL_W);
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
                    .key(ZOOM_KEY)
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

        let used_w = dock_button_w + GROUP_SPACING + ZOOM_BUTTON_W * 2.0 + zoom_label_w;
        (
            View::row(GROUP_SPACING)
                .fill()
                .main_align(Align::End)
                .child(dock_toggles)
                .child(zoom_cluster),
            used_w,
        )
    }

    /// The description, and the clock slot's width in it.
    fn view(&self) -> (View, f32) {
        let available_w = (self.rect.width - 2.0 * INSET).max(0.0);
        // The clock has first claim on width: it is the one thing read on stage.
        // Reserve its centred slot, then each side group compacts into what is
        // left beside it.
        let side_room = ((available_w - TIME_DISPLAY_W) * 0.5 - GROUP_SPACING).max(0.0);
        let (right, right_w) = self.right_group(side_room);
        let (left, left_w) = self.left_group(side_room);
        let clock_w = TIME_DISPLAY_W.min(
            (available_w - 2.0 * left_w)
                .min(available_w - 2.0 * right_w)
                .max(0.0),
        );
        let view = View::stack()
            .fill()
            .bg(color::PANEL_BG_DARK)
            .border(color::BORDER, 1.0)
            .pad(Pad {
                l: INSET,
                t: GROUP_Y_PAD,
                r: INSET,
                b: GROUP_Y_PAD,
            })
            .child(left)
            .child(self.center_group(clock_w))
            .child(right);
        (view, clock_w)
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
        let (view, clock_w) = self.view();
        self.clock_w = clock_w;
        self.host.build(tree, &view, self.rect);
    }

    fn update(&mut self, tree: &mut UITree) {
        let _ = self.relayout(tree);
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

    fn node_with_text<'a>(tree: &'a UITree, text: &str) -> &'a crate::node::UINode {
        (0..tree.count())
            .filter_map(|i| tree.get_node(tree.id_at(i)))
            .find(|n| n.text.as_deref() == Some(text))
            .unwrap_or_else(|| panic!("no node with text {text:?}"))
    }

    /// The clock sits at the true centre of the bar and the zoom cluster
    /// reads [−][+] at the right end.
    #[test]
    fn clock_is_centred_and_zoom_cluster_trails() {
        use crate::panels::layout_order::{assert_centred, assert_left_to_right};
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let clock = node_with_text(&tree, "00:00.00 / 00:00.00  |  1.1.1").bounds;
        let cells = [
            ("clock", clock),
            ("zoom out", node_with_text(&tree, "\u{2212}").bounds),
            ("zoom in", node_with_text(&tree, "+").bounds),
        ];
        assert_left_to_right(&cells, layout.header());
        assert_centred("clock", clock.x, clock.x_max(), layout.header());
    }

    #[test]
    fn value_change_updates_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);
        let count = tree.count();
        let sv = tree.structure_version();

        panel.set_time_display(&mut tree, "01:30.50  |  4.2.3");
        panel.set_project_name("Live Set");
        panel.update(&mut tree);

        assert_eq!(tree.count(), count, "no nodes added");
        assert_eq!(tree.structure_version(), sv, "no structure bump");
        assert_eq!(
            node_with_text(&tree, "01:30.50  |  4.2.3").text.as_deref(),
            Some("01:30.50  |  4.2.3")
        );
    }

    /// The clock writes its label without a layout, and an update after it
    /// shows the same text the write did.
    #[test]
    fn clock_writes_in_place_without_layout() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();
        let before = node_with_text(&tree, "00:00.00 / 00:00.00  |  1.1.1").bounds;

        panel.set_time_display(&mut tree, "00:04.7  |  3.2.1");
        let clock = node_with_text(&tree, "00:04.7  |  3.2.1");
        assert_eq!(clock.bounds, before, "the slot keeps its rect");
        assert_eq!(tree.structure_version(), sv);

        panel.update(&mut tree);
        assert_eq!(node_with_text(&tree, "00:04.7  |  3.2.1").bounds, before);
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

        assert_eq!(
            tree.structure_version(),
            sv,
            "progress toggle must not rebuild"
        );
    }

    /// The clock is the one header element read on stage; docks and the
    /// inspector narrow the header, and the side groups compact before the
    /// clock loses a single character.
    #[test]
    fn clock_survives_open_docks_at_1920() {
        for audio in [false, true] {
            let mut tree = UITree::new();
            let mut layout = ScreenLayout::new(1920.0, 1080.0);
            if audio {
                layout.audio_setup_width = color::DEFAULT_AUDIO_SETUP_WIDTH;
            }
            let mut panel = HeaderPanel::new();
            panel.build(&mut tree, &layout);
            panel.set_time_display(&mut tree, "01:30.50 / 04:00.00  |  4.2.3");
            panel.update(&mut tree);
            let clock = node_with_text(&tree, "01:30.50 / 04:00.00  |  4.2.3");
            assert_eq!(
                clock.bounds.width, TIME_DISPLAY_W,
                "clock keeps its full slot"
            );
            let header = layout.header();
            let centre = clock.bounds.x + clock.bounds.width * 0.5;
            assert!(
                (centre - (header.x + header.width * 0.5)).abs() < 1.0,
                "clock stays centred"
            );
        }
    }

    #[test]
    fn narrow_content_keeps_zoom_cluster_inside_header() {
        let mut tree = UITree::new();
        let mut layout = ScreenLayout::new(1440.0, 900.0);
        layout.audio_setup_width = color::DEFAULT_AUDIO_SETUP_WIDTH;
        let header = layout.header();
        let mut panel = HeaderPanel::new();
        panel.build(&mut tree, &layout);

        let zoom_out = node_with_text(&tree, "\u{2212}").bounds;
        let zoom_in = node_with_text(&tree, "+").bounds;
        let project = node_with_text(&tree, "My Project").bounds;
        assert!(zoom_out.x >= header.x);
        assert!(zoom_in.x + zoom_in.width <= header.x + header.width);
        assert!(project.x + project.width <= zoom_out.x);
        assert!(
            zoom_in.x > zoom_out.x,
            "zoom controls retain left-to-right order"
        );
    }
}
