//! Transport bar on the declarative Chrome API.
//!
//! Three positioning regimes (sources on the left, transport+tempo centred,
//! file+render right) as a `Stack` of three `Fill` rows, plus the thin group
//! dividers folded into the section gaps as cross-centred fixed cells. See the
//! footer/header for the integration pattern and `docs/CHROME_API_DESIGN.md`.

use crate::{TransportAction};
use super::{Panel, PanelAction};
use crate::chrome::{Align, ChromeHost, Pad, Reconcile, Sizing, View, components};
use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants (from TransportLayout.cs) ─────────────────────

const INSET: f32 = color::SPACE_M;
const GROUP_Y_PAD: f32 = color::SPACE_S;
const ITEM_SPACING: f32 = color::SPACE_S; // §14.4: 5 → 4
const SECTION_SPACER: f32 = color::SPACE_M;
const CENTER_SPACER: f32 = color::SPACE_L;

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

// Automation globals (P4, docs/AUTOMATION_LANES_DESIGN.md §7) — right-aligned
// group, mirroring the removed file-ops group's old slot.
const AUTO_ARM_BUTTON_W: f32 = 48.0;
const AUTO_BACK_BUTTON_W: f32 = 92.0;
const AUTO_LANES_BUTTON_W: f32 = 62.0;

// ── Panel-specific colors ──────────────────────────────────────────

const BUTTON_FONT: u16 = color::FONT_SUBHEADING;
const STATUS_FONT: u16 = color::FONT_BODY;

/// Stable key for the BPM field — the app anchors a numeric text-input session
/// to this node ([`TransportPanel::bpm_field_id`]).
const KEY_BPM_FIELD: u64 = 1;

// ── Style helpers ──────────────────────────────────────────────────

// Every transport button is the same state-button mechanic: filled with its
// semantic `bg` when active, a neutral chip when not. `BUTTON_INACTIVE_C32` is
// the "no state colour" sentinel that selects the off chip. Delegates to the kit
// so transport and the layer-card mixer share one look (just a larger font here).
fn button_style(bg: Color32) -> UIStyle {
    let active = bg != color::BUTTON_INACTIVE_C32;
    UIStyle {
        font_size: BUTTON_FONT,
        ..components::state_button_style(bg, active)
    }
}

fn dot_style(c: Color32) -> UIStyle {
    UIStyle {
        bg_color: c,
        corner_radius: 4.0, // design-token-exempt: circular 8px status dot (radius = half size)
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

    // Automation globals (P4).
    automation_armed: bool,
    automation_overridden: bool,
    /// View-only: lanes currently shown across the timeline (`PanelAction::
    /// ToggleAutomationMode`) — lit exactly when visible, no runtime/project
    /// state behind it.
    automation_mode_visible: bool,
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
            automation_armed: false,
            automation_overridden: false,
            automation_mode_visible: false,
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

    /// Automation globals (P4): `armed` mirrors `PlaybackEngine::automation_armed()`
    /// (the ARM button's lit state); `overridden` is whether any lane latch is
    /// active (`!automation_latched_params.is_empty()`) — the BACK button lights
    /// red exactly when this is true, matching Live's Back to Arrangement.
    pub fn set_automation_state(&mut self, armed: bool, overridden: bool) {
        self.automation_armed = armed;
        self.automation_overridden = overridden;
    }

    /// View-only lane visibility (P4 `ToggleAutomationMode`) — lights the
    /// LANES button exactly when lane strips are currently shown.
    pub fn set_automation_mode_visible(&mut self, visible: bool) {
        self.automation_mode_visible = visible;
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
            .child(Self::btn("LINK", LINK_BUTTON_W, button_style(link_bg), PanelAction::Transport(TransportAction::ToggleLink)))
            .child(Self::dot(self.link_dot_color))
            .child(Self::status(self.link_status_text.as_str(), self.link_status_color))
            .child(self.section_break(SECTION_SPACER))
            .child(Self::btn("CLK", CLK_BUTTON_W, button_style(clk_bg), PanelAction::Transport(TransportAction::ToggleMidiClock)))
            .child(Self::btn(
                self.clk_device_text.as_str(),
                CLK_DEVICE_W,
                button_style(color::BUTTON_INACTIVE_C32),
                PanelAction::Transport(TransportAction::SelectClkDevice),
            ))
            .child(Self::dot(self.clk_dot_color))
            .child(Self::status(self.clk_status_text.as_str(), self.clk_status_color))
            .child(self.section_break(SECTION_SPACER))
            .child(Self::btn("SYNC", SYNC_BUTTON_W, button_style(sync_bg), PanelAction::Transport(TransportAction::ToggleSyncOutput)))
            .child(Self::dot(self.sync_dot_color))
            .child(Self::status(self.sync_status_text.as_str(), self.sync_status_color))
    }

    fn center_group(&self) -> View {
        let rec_c = if self.rec_active { color::RECORD_ACTIVE } else { color::RECORD_RED };
        let reset_c = if self.bpm_reset_active { color::BPM_RESET_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let clear_c = if self.bpm_clear_active { color::BPM_CLEAR_ACTIVE } else { color::BUTTON_INACTIVE_C32 };

        // The BPM type-in field rides the same neutral chip surface as the buttons
        // beside it — one control look across the transport bar.
        let bpm_field_style = UIStyle {
            bg_color: color::BG_3,
            hover_bg_color: color::BG_3_HOVER,
            pressed_bg_color: color::BG_3_PRESSED,
            text_color: color::TEXT_NORMAL,
            font_size: BUTTON_FONT,
            corner_radius: color::CHIP_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        };

        View::row(ITEM_SPACING)
            .fill()
            .main_align(Align::Center)
            .cross_align(Align::Center)
            .child(
                Self::btn(self.play_text.as_str(), PLAY_BUTTON_W, button_style(self.play_color), PanelAction::Transport(TransportAction::PlayPause))
                    .name("transport.play"),
            )
            .child(Self::btn("STOP", STOP_BUTTON_W, button_style(self.stop_color), PanelAction::Transport(TransportAction::Stop)).name("transport.stop"))
            .child(
                Self::btn("REC", REC_BUTTON_W, button_style(rec_c), PanelAction::Transport(TransportAction::Record))
                    .disabled(!self.rec_enabled)
                    .name("transport.record"),
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
                    .on_click(PanelAction::Transport(TransportAction::BpmFieldClicked))
                    .key(KEY_BPM_FIELD),
            )
            .child(
                Self::btn("R", BPM_RESET_W, button_style(reset_c), PanelAction::Transport(TransportAction::ResetBpm))
                    .disabled(!self.bpm_reset_active),
            )
            .child(
                Self::btn("CLR", BPM_CLEAR_W, button_style(clear_c), PanelAction::Transport(TransportAction::ClearBpm))
                    .disabled(!self.bpm_clear_active),
            )
    }

    /// Right-aligned automation globals (P4): LANES (view-only, lit while lane
    /// strips are shown — Live's `A`) + BACK (Back to Arrangement, lit red
    /// exactly when a lane override latch is active) + ARM.
    ///
    /// ARM used to mirror REC's active/idle red pair (lit red while armed),
    /// but ARM and REC mean different things: REC's two states are just
    /// "recording or not"; ARM's decide what touching a param DOES (override
    /// the lane vs punch automation into the arrangement) — a misread at
    /// stage distance silently writes automation into the show. So ARM gets
    /// its own idle/armed pair, distinct from every red in this row: idle
    /// matches its neutral siblings (BACK/LANES), armed is amber, never red.
    /// See BUG-048, `docs/TIMELINE_UX_AUDIT_2026-07-07.md` item 2.5.
    fn automation_group(&self) -> View {
        let arm_bg = if self.automation_armed { color::STATUS_WARNING } else { color::BUTTON_INACTIVE_C32 };
        let back_bg = if self.automation_overridden { color::RECORD_ACTIVE } else { color::BUTTON_INACTIVE_C32 };
        let lanes_bg = if self.automation_mode_visible { color::AUTOMATION_LINE_COLOR } else { color::BUTTON_INACTIVE_C32 };

        View::row(ITEM_SPACING)
            .fill()
            .main_align(Align::End)
            .cross_align(Align::Center)
            .child(Self::btn("LANES", AUTO_LANES_BUTTON_W, button_style(lanes_bg), PanelAction::Transport(TransportAction::ToggleAutomationMode)))
            .child(Self::btn("BACK", AUTO_BACK_BUTTON_W, button_style(back_bg), PanelAction::Transport(TransportAction::AutomationBackToArrangement)))
            .child(Self::btn("ARM", AUTO_ARM_BUTTON_W, button_style(arm_bg), PanelAction::Transport(TransportAction::ToggleAutomationArm)))
    }

    fn view(&self) -> View {
        // File ops moved to the native File menu; HDR/Percussion to the Settings
        // popup — so the old right group is gone. Left (sync) + centred transport
        // + right (automation globals), each stacked layer keeping its own
        // `main_align`.
        View::stack()
            .fill()
            .bg(color::PANEL_BG_DARK)
            .border(color::BORDER, 1.0)
            .pad(Pad { l: INSET, t: GROUP_Y_PAD, r: INSET, b: GROUP_Y_PAD })
            .child(self.left_group())
            .child(self.center_group())
            .child(self.automation_group())
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

            // Right group removed: file ops → File menu, HDR/PERC → Settings popup.
            // Automation globals (P4) now occupy that right-aligned slot: a Row
            // with `main_align(Align::End)` offsets its whole sequence by `slack`
            // (see `chrome::layout::align_offset`), so LANES/BACK/ARM land flush
            // against the padded right edge in child order — LANES first (left),
            // ARM last (flush right), same math as `left_group`'s Start-aligned x
            // but mirrored.
            let auto_w = AUTO_LANES_BUTTON_W + ITEM_SPACING + AUTO_BACK_BUTTON_W + ITEM_SPACING + AUTO_ARM_BUTTON_W;
            let mut ax = bounds.x_max() - INSET - auto_w;
            put("LANES", ax, AUTO_LANES_BUTTON_W);
            ax += AUTO_LANES_BUTTON_W + ITEM_SPACING;
            put("BACK", ax, AUTO_BACK_BUTTON_W);
            ax += AUTO_BACK_BUTTON_W + ITEM_SPACING;
            put("ARM", ax, AUTO_ARM_BUTTON_W);
        }
    }

    fn buttons(tree: &UITree) -> Vec<(String, Rect)> {
        (0..tree.count())
            .filter_map(|i| tree.get_node(tree.id_at(i)))
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
        assert_eq!(got.len(), 14, "14 transport buttons (sync left + transport centre + automation right)");
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
                .filter_map(|i| tree.get_node(tree.id_at(i)))
                .find(|n| n.text.as_deref() == Some(t))
                .map(|n| n.id)
        };
        assert!(matches!(
            intents.resolve(&tree, id_of("PLAY"), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::PlayPause))
        ));
        assert!(matches!(
            intents.resolve(&tree, id_of("SYNC"), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::ToggleSyncOutput))
        ));
        assert!(matches!(
            intents.resolve(&tree, panel.bpm_field_id(), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::BpmFieldClicked))
        ));
        // Clock authority is display-only: interactive, but no resolved action.
        assert!(intents.resolve(&tree, id_of("SRC:INT"), Gesture::Click).is_none());
        assert!(matches!(
            intents.resolve(&tree, id_of("ARM"), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::ToggleAutomationArm))
        ));
        assert!(matches!(
            intents.resolve(&tree, id_of("BACK"), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::AutomationBackToArrangement))
        ));
        assert!(matches!(
            intents.resolve(&tree, id_of("LANES"), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::ToggleAutomationMode))
        ));
    }

    #[test]
    fn automation_state_updates_in_place() {
        let mut tree = UITree::new();
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let mut panel = TransportPanel::new();
        panel.build(&mut tree, &layout);
        let sv = tree.structure_version();

        fn node<'a>(tree: &'a UITree, t: &str) -> &'a UINode {
            (0..tree.count())
                .filter_map(|i| tree.get_node(tree.id_at(i)))
                .find(|n| n.text.as_deref() == Some(t))
                .unwrap()
        }

        panel.set_automation_state(true, true);
        panel.set_automation_mode_visible(true);
        panel.update(&mut tree);
        assert_eq!(tree.structure_version(), sv, "automation state toggle must not rebuild");
        assert_eq!(node(&tree, "ARM").style.bg_color, button_style(color::STATUS_WARNING).bg_color);
        assert_eq!(node(&tree, "BACK").style.bg_color, button_style(color::RECORD_ACTIVE).bg_color);
        assert_eq!(node(&tree, "LANES").style.bg_color, button_style(color::AUTOMATION_LINE_COLOR).bg_color);

        panel.set_automation_state(false, false);
        panel.set_automation_mode_visible(false);
        panel.update(&mut tree);
        assert_eq!(
            node(&tree, "ARM").style.bg_color,
            button_style(color::BUTTON_INACTIVE_C32).bg_color
        );
        assert_eq!(
            node(&tree, "BACK").style.bg_color,
            button_style(color::BUTTON_INACTIVE_C32).bg_color
        );
        assert_eq!(
            node(&tree, "LANES").style.bg_color,
            button_style(color::BUTTON_INACTIVE_C32).bg_color
        );
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
                .filter_map(|i| tree.get_node(tree.id_at(i)))
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
            .filter_map(|i| tree.get_node(tree.id_at(i)))
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
            .filter_map(|i| tree.get_node(tree.id_at(i)))
            .find(|n| n.text.as_deref() == Some("PAUSE"))
            .expect("PLAY became PAUSE in place");
        assert_eq!(tree.structure_version(), sv);
        assert_eq!(play.text.as_deref(), Some("PAUSE"));
    }
}
