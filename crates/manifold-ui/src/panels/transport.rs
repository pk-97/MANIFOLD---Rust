//! Transport bar on the declarative Chrome API.
//!
//! Three positioning regimes (sources on the left, transport+tempo centred,
//! file+render right) as a `Stack` of three `Fill` rows, plus the thin group
//! dividers folded into the section gaps as cross-centred fixed cells. See the
//! footer/header for the integration pattern and `docs/CHROME_API_DESIGN.md`.

use super::{Panel, PanelAction};
use crate::chrome::{Align, ChromeHost, Pad, Reconcile, Sizing, View};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants (from TransportLayout.cs) ─────────────────────

const INSET: f32 = 8.0;
const GROUP_Y_PAD: f32 = 4.0;
const ITEM_SPACING: f32 = 5.0;
const SECTION_SPACER: f32 = 8.0;
const RIGHT_SPACING: f32 = 4.0;
const CENTER_SPACER: f32 = 12.0;

const DIVIDER_W: f32 = 1.0;
const DIVIDER_V_INSET: f32 = 7.0;

const STATUS_DOT_SIZE: f32 = 8.0;
const STATUS_TEXT_W: f32 = 55.0;

const CLOCK_AUTHORITY_W: f32 = 68.0;
const LINK_BUTTON_W: f32 = 45.0;
const CLK_BUTTON_W: f32 = 35.0;
const CLK_DEVICE_W: f32 = 100.0;
const SYNC_BUTTON_W: f32 = 45.0;

const PLAY_BUTTON_W: f32 = 50.0;
const STOP_BUTTON_W: f32 = 50.0;
const REC_BUTTON_W: f32 = 42.0;
const BPM_LABEL_W: f32 = 28.0;
const BPM_FIELD_W: f32 = 60.0;
const BPM_RESET_W: f32 = 24.0;
const BPM_CLEAR_W: f32 = 32.0;

const NEW_BUTTON_W: f32 = 40.0;
const OPEN_BUTTON_W: f32 = 45.0;
const OPEN_RECENT_W: f32 = 92.0;
const SAVE_BUTTON_W: f32 = 42.0;
const SAVE_AS_W: f32 = 55.0;
const EXPORT_BUTTON_W: f32 = 55.0;
const FRAME_BUTTON_W: f32 = 48.0;
const HDR_BUTTON_W: f32 = 35.0;
const PERC_BUTTON_W: f32 = 48.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_HOVER_C: Color32 = color::TRANSPORT_BUTTON_HOVER;
const SAVE_DIRTY_BG: Color32 = color::TRANSPORT_SAVE_DIRTY_BG;
const BPM_FIELD_HOVER: Color32 = color::TRANSPORT_BPM_FIELD_HOVER;

const BUTTON_FONT: u16 = color::FONT_SUBHEADING;
const STATUS_FONT: u16 = color::FONT_BODY;

/// Stable key for the BPM field — the app anchors a numeric text-input session
/// to this node ([`TransportPanel::bpm_field_id`]).
const KEY_BPM_FIELD: u64 = 1;

// ── Style helpers ──────────────────────────────────────────────────

fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_add(amount),
        c.g.saturating_add(amount),
        c.b.saturating_add(amount),
        c.a,
    )
}

fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_sub(amount),
        c.g.saturating_sub(amount),
        c.b.saturating_sub(amount),
        c.a,
    )
}

fn button_style(bg: Color32) -> UIStyle {
    let is_active = bg != color::BUTTON_INACTIVE_C32;
    let hover = if is_active {
        lighten(bg, 30)
    } else {
        BUTTON_HOVER_C
    };
    let pressed = if is_active {
        darken(bg, 20)
    } else {
        color::BUTTON_PRESSED
    };
    UIStyle {
        bg_color: bg,
        hover_bg_color: hover,
        pressed_bg_color: pressed,
        text_color: color::TEXT_WHITE_C32,
        font_size: BUTTON_FONT,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

fn dot_style(c: Color32) -> UIStyle {
    UIStyle {
        bg_color: c,
        corner_radius: 4.0,
        ..UIStyle::default()
    }
}

fn status_text_style(c: Color32) -> UIStyle {
    UIStyle {
        text_color: c,
        font_size: STATUS_FONT,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

// ── TransportPanel ─────────────────────────────────────────────────

pub struct TransportPanel {
    host: ChromeHost,
    rect: Rect,

    // Dynamic state — the single source the `view()` reads.
    clock_authority_text: String,
    clock_authority_color: Color32,
    link_enabled: bool,
    link_dot_color: Color32,
    link_status_text: String,
    link_status_color: Color32,
    clk_enabled: bool,
    clk_device_text: String,
    clk_dot_color: Color32,
    clk_status_text: String,
    clk_status_color: Color32,
    sync_enabled: bool,
    sync_dot_color: Color32,
    sync_status_text: String,
    sync_status_color: Color32,
    play_text: String,
    play_color: Color32,
    stop_color: Color32,
    rec_active: bool,
    rec_enabled: bool,
    bpm_text: String,
    bpm_reset_active: bool,
    bpm_clear_active: bool,
    save_text: String,
    export_active: bool,
    hdr_active: bool,
    perc_active: bool,
}

impl TransportPanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            rect: Rect::ZERO,
            clock_authority_text: "SRC:INT".into(),
            clock_authority_color: color::BUTTON_INACTIVE_C32,
            link_enabled: false,
            link_dot_color: color::DRIVER_INACTIVE_C32,
            link_status_text: "Off".into(),
            link_status_color: color::TEXT_DIMMED_C32,
            clk_enabled: false,
            clk_device_text: "Select...".into(),
            clk_dot_color: color::DRIVER_INACTIVE_C32,
            clk_status_text: "Off".into(),
            clk_status_color: color::TEXT_DIMMED_C32,
            sync_enabled: false,
            sync_dot_color: color::DRIVER_INACTIVE_C32,
            sync_status_text: "Off".into(),
            sync_status_color: color::TEXT_DIMMED_C32,
            play_text: "PLAY".into(),
            play_color: color::PLAY_GREEN,
            stop_color: color::STOP_RED,
            rec_active: false,
            rec_enabled: true,
            bpm_text: "120.0".into(),
            bpm_reset_active: false,
            bpm_clear_active: false,
            save_text: "SAVE".into(),
            export_active: false,
            hdr_active: false,
            perc_active: false,
        }
    }

    // ── Public accessors ───────────────────────────────────────────

    /// Tree node id of the BPM field, for anchoring its numeric text-input.
    pub fn bpm_field_id(&self) -> Option<NodeId> {
        self.host.node_id_for_key(KEY_BPM_FIELD)
    }

    // ── State setters (store only; the reconcile applies them) ──────

    pub fn set_clock_authority(&mut self, text: &str, c: Color32) {
        self.clock_authority_text = text.into();
        self.clock_authority_color = c;
    }

    pub fn set_link_state(
        &mut self,
        enabled: bool,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.link_enabled = enabled;
        self.link_dot_color = dot_color;
        self.link_status_text = status.into();
        self.link_status_color = status_color;
    }

    pub fn set_clk_state(
        &mut self,
        enabled: bool,
        device_text: &str,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.clk_enabled = enabled;
        self.clk_device_text = device_text.into();
        self.clk_dot_color = dot_color;
        self.clk_status_text = status.into();
        self.clk_status_color = status_color;
    }

    pub fn set_sync_state(
        &mut self,
        enabled: bool,
        dot_color: Color32,
        status: &str,
        status_color: Color32,
    ) {
        self.sync_enabled = enabled;
        self.sync_dot_color = dot_color;
        self.sync_status_text = status.into();
        self.sync_status_color = status_color;
    }

    pub fn set_play_state(&mut self, text: &str, c: Color32) {
        self.play_text = text.into();
        self.play_color = c;
    }

    pub fn set_record_state(&mut self, active: bool, enabled: bool) {
        self.rec_active = active;
        self.rec_enabled = enabled;
    }

    pub fn set_bpm_text(&mut self, text: &str) {
        self.bpm_text = text.into();
    }

    pub fn set_bpm_reset_active(&mut self, active: bool) {
        self.bpm_reset_active = active;
    }

    pub fn set_bpm_clear_active(&mut self, active: bool) {
        self.bpm_clear_active = active;
    }

    pub fn set_save_text(&mut self, text: &str) {
        self.save_text = text.into();
    }

    pub fn set_export_active(&mut self, active: bool) {
        self.export_active = active;
    }

    pub fn set_hdr_active(&mut self, active: bool) {
        self.hdr_active = active;
    }

    pub fn set_perc_active(&mut self, active: bool) {
        self.perc_active = active;
    }

    // ── View description ────────────────────────────────────────────

    fn btn(text: impl Into<String>, w: f32, style: UIStyle, action: PanelAction) -> View {
        View::button(text)
            .w(Sizing::Fixed(w))
            .fill_h()
            .style(style)
            .on_click(action)
    }

    fn dot(color: Color32) -> View {
        View::panel()
            .fixed(STATUS_DOT_SIZE, STATUS_DOT_SIZE)
            .style(dot_style(color))
    }

    fn status(text: &str, color: Color32) -> View {
        View::label(text)
            .w(Sizing::Fixed(STATUS_TEXT_W))
            .fill_h()
            .style(status_text_style(color))
    }

    /// A group separator: a fixed-width transparent cell with a 1px vertical
    /// divider centred in it (and inset from the bar top/bottom). Dropped into a
    /// section gap, it lands the divider at the gap's midpoint — matching the
    /// old post-layout divider placement.
    fn section_break(&self, cell_w: f32) -> View {
        let divider_h = (self.rect.height - DIVIDER_V_INSET * 2.0).max(1.0);
        View::panel()
            .w(Sizing::Fixed(cell_w))
            .fill_h()
            .main_align(Align::Center)
            .cross_align(Align::Center)
            .child(View::panel().fixed(DIVIDER_W, divider_h).bg(color::DIVIDER_COLOR))
    }

    fn left_group(&self) -> View {
        let link_bg = if self.link_enabled { color::LINK_ORANGE } else { color::BUTTON_INACTIVE_C32 };
        let clk_bg = if self.clk_enabled { color::MIDI_PURPLE } else { color::BUTTON_INACTIVE_C32 };
        let sync_bg = if self.sync_enabled { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };

        View::row(ITEM_SPACING)
            .fill()
            .main_align(Align::Start)
            .cross_align(Align::Center)
            // Clock authority is display-only now (auto-determined) — interactive
            // chrome with no action, so it is deliberately inert.
            .child(
                View::button(self.clock_authority_text.as_str())
                    .w(Sizing::Fixed(CLOCK_AUTHORITY_W))
                    .fill_h()
                    .style(button_style(self.clock_authority_color))
                    .inert(),
            )
            .child(self.section_break(SECTION_SPACER))
            .child(Self::btn("LINK", LINK_BUTTON_W, button_style(link_bg), PanelAction::ToggleLink))
            .child(Self::dot(self.link_dot_color))
            .child(Self::status(self.link_status_text.as_str(), self.link_status_color))
            .child(self.section_break(SECTION_SPACER))
            .child(Self::btn("CLK", CLK_BUTTON_W, button_style(clk_bg), PanelAction::ToggleMidiClock))
            .child(Self::btn(
                self.clk_device_text.as_str(),
                CLK_DEVICE_W,
                button_style(color::BUTTON_INACTIVE_C32),
                PanelAction::SelectClkDevice,
            ))
            .child(Self::dot(self.clk_dot_color))
            .child(Self::status(self.clk_status_text.as_str(), self.clk_status_color))
            .child(self.section_break(SECTION_SPACER))
            .child(Self::btn("SYNC", SYNC_BUTTON_W, button_style(sync_bg), PanelAction::ToggleSyncOutput))
            .child(Self::dot(self.sync_dot_color))
            .child(Self::status(self.sync_status_text.as_str(), self.sync_status_color))
    }

    fn center_group(&self) -> View {
        let rec_c = if self.rec_active { color::RECORD_ACTIVE } else { color::RECORD_RED };
        let reset_c = if self.bpm_reset_active { color::BPM_RESET_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let clear_c = if self.bpm_clear_active { color::BPM_CLEAR_ACTIVE } else { color::BUTTON_INACTIVE_C32 };

        let bpm_field_style = UIStyle {
            bg_color: color::SLIDER_TRACK_C32,
            hover_bg_color: BPM_FIELD_HOVER,
            pressed_bg_color: color::BUTTON_PRESSED,
            text_color: color::TEXT_WHITE_C32,
            font_size: BUTTON_FONT,
            corner_radius: color::SMALL_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        };

        View::row(ITEM_SPACING)
            .fill()
            .main_align(Align::Center)
            .cross_align(Align::Center)
            .child(Self::btn(self.play_text.as_str(), PLAY_BUTTON_W, button_style(self.play_color), PanelAction::PlayPause))
            .child(Self::btn("STOP", STOP_BUTTON_W, button_style(self.stop_color), PanelAction::Stop))
            .child(
                Self::btn("REC", REC_BUTTON_W, button_style(rec_c), PanelAction::Record)
                    .disabled(!self.rec_enabled),
            )
            .child(self.section_break(CENTER_SPACER))
            .child(
                View::label("BPM")
                    .w(Sizing::Fixed(BPM_LABEL_W))
                    .fill_h()
                    .font(STATUS_FONT)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Right),
            )
            .child(
                View::button(self.bpm_text.as_str())
                    .w(Sizing::Fixed(BPM_FIELD_W))
                    .fill_h()
                    .style(bpm_field_style)
                    .on_click(PanelAction::BpmFieldClicked)
                    .key(KEY_BPM_FIELD),
            )
            .child(
                Self::btn("R", BPM_RESET_W, button_style(reset_c), PanelAction::ResetBpm)
                    .disabled(!self.bpm_reset_active),
            )
            .child(
                Self::btn("CLR", BPM_CLEAR_W, button_style(clear_c), PanelAction::ClearBpm)
                    .disabled(!self.bpm_clear_active),
            )
    }

    fn right_group(&self) -> View {
        let open_recent_style = UIStyle {
            bg_color: color::BUTTON_INACTIVE_C32,
            hover_bg_color: BUTTON_HOVER_C,
            pressed_bg_color: color::BUTTON_PRESSED,
            text_color: color::TEXT_WHITE_C32,
            font_size: color::FONT_LABEL,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        };
        let save_bg = if self.save_text.contains('*') { SAVE_DIRTY_BG } else { color::BUTTON_INACTIVE_C32 };
        let export_bg = if self.export_active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let hdr_bg = if self.hdr_active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let perc_bg = if self.perc_active { color::SYNC_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let inactive = || button_style(color::BUTTON_INACTIVE_C32);

        View::row(RIGHT_SPACING)
            .fill()
            .main_align(Align::End)
            .cross_align(Align::Center)
            .child(Self::btn("NEW", NEW_BUTTON_W, inactive(), PanelAction::NewProject))
            .child(Self::btn("OPEN", OPEN_BUTTON_W, inactive(), PanelAction::OpenProject))
            .child(Self::btn("OPEN RECENT", OPEN_RECENT_W, open_recent_style, PanelAction::OpenRecent))
            .child(Self::btn(self.save_text.as_str(), SAVE_BUTTON_W, button_style(save_bg), PanelAction::SaveProject))
            .child(Self::btn("SAVE AS", SAVE_AS_W, inactive(), PanelAction::SaveProjectAs))
            // The file|render section gap is RIGHT_SPACING + SECTION_SPACER; the
            // row already contributes RIGHT_SPACING on each side of the cell.
            .child(self.section_break(SECTION_SPACER - RIGHT_SPACING))
            .child(Self::btn("EXPORT", EXPORT_BUTTON_W, button_style(export_bg), PanelAction::ExportVideo))
            .child(Self::btn("FRAME", FRAME_BUTTON_W, inactive(), PanelAction::ExportFrame))
            .child(Self::btn("HDR", HDR_BUTTON_W, button_style(hdr_bg), PanelAction::ToggleHdr))
            .child(Self::btn("PERC", PERC_BUTTON_W, button_style(perc_bg), PanelAction::TogglePercussion))
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

impl Default for TransportPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for TransportPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.rect = layout.transport_bar();
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
            "transport structure is invariant per frame — value/disabled changes update in place"
        );
    }

    /// Transport is fully intent-dispatched (see `register_intents`). Required no-op.
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
    use std::collections::HashMap;

    // Golden oracle: the original three-regime pixel layout. The Chrome `view()`
    // must land every interactive button at the same rect.
    #[derive(Default)]
    struct TransportGolden {
        rects: HashMap<&'static str, Rect>,
    }

    impl TransportGolden {
        fn compute(&mut self, bounds: Rect) {
            let eh = bounds.height - GROUP_Y_PAD * 2.0;
            let ey = bounds.y + GROUP_Y_PAD;
            let mut put = |k, x, w| {
                self.rects.insert(k, Rect::new(x, ey, w, eh));
            };

            // Left
            let mut x = bounds.x + INSET;
            put("SRC:INT", x, CLOCK_AUTHORITY_W);
            x += CLOCK_AUTHORITY_W + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;
            put("LINK", x, LINK_BUTTON_W);
            x += LINK_BUTTON_W + ITEM_SPACING + STATUS_DOT_SIZE + ITEM_SPACING + STATUS_TEXT_W
                + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;
            put("CLK", x, CLK_BUTTON_W);
            x += CLK_BUTTON_W + ITEM_SPACING;
            put("Select...", x, CLK_DEVICE_W);
            x += CLK_DEVICE_W + ITEM_SPACING + STATUS_DOT_SIZE + ITEM_SPACING + STATUS_TEXT_W
                + ITEM_SPACING + SECTION_SPACER + ITEM_SPACING;
            put("SYNC", x, SYNC_BUTTON_W);

            // Center
            let center_w = PLAY_BUTTON_W + ITEM_SPACING + STOP_BUTTON_W + ITEM_SPACING + REC_BUTTON_W
                + ITEM_SPACING + CENTER_SPACER + ITEM_SPACING + BPM_LABEL_W + ITEM_SPACING
                + BPM_FIELD_W + ITEM_SPACING + BPM_RESET_W + ITEM_SPACING + BPM_CLEAR_W;
            let mut cx = bounds.x + (bounds.width - center_w) * 0.5;
            put("PLAY", cx, PLAY_BUTTON_W);
            cx += PLAY_BUTTON_W + ITEM_SPACING;
            put("STOP", cx, STOP_BUTTON_W);
            cx += STOP_BUTTON_W + ITEM_SPACING;
            put("REC", cx, REC_BUTTON_W);
            cx += REC_BUTTON_W + ITEM_SPACING + CENTER_SPACER + ITEM_SPACING + BPM_LABEL_W + ITEM_SPACING;
            put("120.0", cx, BPM_FIELD_W);
            cx += BPM_FIELD_W + ITEM_SPACING;
            put("R", cx, BPM_RESET_W);
            cx += BPM_RESET_W + ITEM_SPACING;
            put("CLR", cx, BPM_CLEAR_W);

            // Right
            let right_w = NEW_BUTTON_W + RIGHT_SPACING + OPEN_BUTTON_W + RIGHT_SPACING + OPEN_RECENT_W
                + RIGHT_SPACING + SAVE_BUTTON_W + RIGHT_SPACING + SAVE_AS_W + RIGHT_SPACING
                + SECTION_SPACER + EXPORT_BUTTON_W + RIGHT_SPACING + FRAME_BUTTON_W + RIGHT_SPACING
                + HDR_BUTTON_W + RIGHT_SPACING + PERC_BUTTON_W;
            let mut rx = bounds.x_max() - INSET - right_w;
            put("NEW", rx, NEW_BUTTON_W);
            rx += NEW_BUTTON_W + RIGHT_SPACING;
            put("OPEN", rx, OPEN_BUTTON_W);
            rx += OPEN_BUTTON_W + RIGHT_SPACING;
            put("OPEN RECENT", rx, OPEN_RECENT_W);
            rx += OPEN_RECENT_W + RIGHT_SPACING;
            put("SAVE", rx, SAVE_BUTTON_W);
            rx += SAVE_BUTTON_W + RIGHT_SPACING;
            put("SAVE AS", rx, SAVE_AS_W);
            rx += SAVE_AS_W + RIGHT_SPACING + SECTION_SPACER;
            put("EXPORT", rx, EXPORT_BUTTON_W);
            rx += EXPORT_BUTTON_W + RIGHT_SPACING;
            put("FRAME", rx, FRAME_BUTTON_W);
            rx += FRAME_BUTTON_W + RIGHT_SPACING;
            put("HDR", rx, HDR_BUTTON_W);
            rx += HDR_BUTTON_W + RIGHT_SPACING;
            put("PERC", rx, PERC_BUTTON_W);
        }
    }

    fn buttons(tree: &UITree) -> Vec<(String, Rect)> {
        (0..tree.count())
            .map(|i| tree.get_node(tree.id_at(i)))
            .filter(|n| n.node_type == UINodeType::Button)
            .map(|n| (n.text.clone().unwrap_or_default(), n.bounds))
            .collect()
    }

    #[test]
    fn chrome_layout_matches_golden() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let mut g = TransportGolden::default();
        g.compute(layout.transport_bar());

        let got = buttons(&tree);
        assert_eq!(got.len(), 20, "20 transport buttons");
        for (text, rect) in &got {
            let want = g.rects.get(text.as_str()).unwrap_or_else(|| panic!("unexpected button {text:?}"));
            assert!(
                (rect.x - want.x).abs() < 0.01
                    && (rect.y - want.y).abs() < 0.01
                    && (rect.width - want.width).abs() < 0.01
                    && (rect.height - want.height).abs() < 0.01,
                "button '{text}' at {rect:?} != golden {want:?}"
            );
        }
    }

    #[test]
    fn intents_resolve_through_registry() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        let id_of = |t: &str| {
            (0..tree.count())
                .map(|i| tree.get_node(tree.id_at(i)))
                .find(|n| n.text.as_deref() == Some(t))
                .map(|n| n.id)
        };
        assert!(matches!(
            intents.resolve(&tree, id_of("PLAY"), Gesture::Click),
            Some(PanelAction::PlayPause)
        ));
        assert!(matches!(
            intents.resolve(&tree, id_of("EXPORT"), Gesture::Click),
            Some(PanelAction::ExportVideo)
        ));
        assert!(matches!(
            intents.resolve(&tree, panel.bpm_field_id(), Gesture::Click),
            Some(PanelAction::BpmFieldClicked)
        ));
        // Clock authority is display-only: interactive, but no resolved action.
        assert!(intents.resolve(&tree, id_of("SRC:INT"), Gesture::Click).is_none());
    }

    #[test]
    fn disabled_buttons_are_flagged() {
        // REC (enabled by default) is hittable; bpm reset/clear (inactive) are
        // disabled and excluded from hit-testing.
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);

        let node = |t: &str| {
            (0..tree.count())
                .map(|i| tree.get_node(tree.id_at(i)))
                .find(|n| n.text.as_deref() == Some(t))
                .unwrap()
                .id
        };
        assert!(!tree.has_flag(node("REC"), UIFlags::DISABLED));
        assert!(tree.has_flag(node("R"), UIFlags::DISABLED));
        assert!(tree.has_flag(node("CLR"), UIFlags::DISABLED));
    }

    #[test]
    fn disable_toggle_is_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        // Recorded-tempo lane appears → BPM reset enables. In-place, no rebuild.
        panel.set_bpm_reset_active(true);
        panel.update(&mut tree);

        let r = (0..tree.count())
            .map(|i| tree.get_node(tree.id_at(i)))
            .find(|n| n.text.as_deref() == Some("R"))
            .unwrap()
            .id;
        assert_eq!(tree.structure_version(), sv, "disable toggle must not rebuild");
        assert!(!tree.has_flag(r, UIFlags::DISABLED));
    }

    #[test]
    fn play_state_updates_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        panel.set_play_state("PAUSE", color::PAUSED_YELLOW);
        panel.update(&mut tree);

        let play = (0..tree.count())
            .map(|i| tree.get_node(tree.id_at(i)))
            .find(|n| n.text.as_deref() == Some("PAUSE"))
            .expect("PLAY became PAUSE in place");
        assert_eq!(tree.structure_version(), sv);
        assert_eq!(play.text.as_deref(), Some("PAUSE"));
    }
}
