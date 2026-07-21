//! Master-FX inspector card on the Chrome API (hybrid).
//!
//! The card's declarative chrome — header + collapse chevron, dividers, the LED
//! row's label / exit-path button / ON-OFF toggle, and two `Fill` *slider slots*
//! — is described once in [`MasterChromePanel::chrome_view`] and reconciled by a
//! [`ChromeHost`]. The two sliders stay [`BitmapSlider`] widgets (a 5-node
//! interactive control whose fill/thumb geometry is post-layout and whose drag is
//! imperative — not expressible as a `View` node); `build` recovers each slot's
//! laid rect by key and builds the slider into it, byte-identical to before. The
//! host never owns the slider nodes, so the per-frame chrome reconcile and the
//! slider drag never fight. The public interface is unchanged, so the inspector
//! composite that drives this card is untouched. See `docs/CHROME_API_DESIGN.md`.

use crate::{ParamsAction, RootAction};
use super::PanelAction;
use crate::chrome::{Align, ChromeHost, Pad, Sizing, SliderSpec, View, components};
use crate::color;
use crate::node::*;
use crate::slider::{SliderColors, SliderDragState};
use crate::tree::UITree;

// ── Layout constants (from MasterChromeBitmapPanel.cs) ───────────

const HEADER_ROW_H: f32 = color::HEADER_ROW_HEIGHT; // §14.2 rule 5: one header height (was 27.5)
const EXIT_PATH_ROW_H: f32 = 27.5;
const SLIDER_ROW_H: f32 = 22.5;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = color::SECTION_CONTENT_INSET; // §14.5 C: align with card param-label column
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const CHEVRON_H: f32 = 16.0;
const TOGGLE_H: f32 = 18.0;
const LED_LABEL_W: f32 = 28.0;
const LED_TOGGLE_W: f32 = 28.0;
const LED_SLIDER_W: f32 = 80.0;
const OPACITY_LABEL_W: f32 = 50.0;
const FONT_SIZE: u16 = color::FONT_BODY;


// Stable keys: chrome elements the panel resolves (clicks / overlay anchor) and
// the slots the sliders drop into.
const KEY_CHEVRON: u64 = 1;
const KEY_EXIT_PATH: u64 = 2;
const KEY_LED_TOGGLE: u64 = 3;
const KEY_BRIGHTNESS_SLOT: u64 = 4;
const KEY_OPACITY_SLOT: u64 = 5;

fn fmt_opacity(v: f32) -> String {
    format!("{:.2}", v)
}

// ── MasterChromePanel ────────────────────────────────────────────

pub struct MasterChromePanel {
    host: ChromeHost,
    chrome_rect: Rect,

    // Sliders — single source of truth for drag state + cache.
    opacity: SliderDragState,
    led_brightness: SliderDragState,

    // State (the source the chrome_view reads).
    is_collapsed: bool,
    cached_exit_path: String,
    cached_led_enabled: bool,

    // Node range for ownership checking (host chrome + slider nodes).
    first_node: usize,
    node_count: usize,
}

impl MasterChromePanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            chrome_rect: Rect::ZERO,
            opacity: SliderDragState::default(),
            led_brightness: SliderDragState::default(),
            is_collapsed: false,
            cached_exit_path: "Default".into(),
            cached_led_enabled: false,
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn compute_height(&self) -> f32 {
        if self.is_collapsed {
            PAD_V + HEADER_ROW_H + PAD_V
        } else {
            // §6d: opacity is now inline on the header row; only the LED row
            // stays below it.
            PAD_V + HEADER_ROW_H + DIVIDER_H + EXIT_PATH_ROW_H + DIVIDER_H + PAD_V
        }
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    /// Reset to "not built": empties the stored node range so consumers that gate
    /// on `node_count() > 0` skip this section when the inspector doesn't build it
    /// this frame (an inactive scope). Keeps `(first_node, node_count)` honest
    /// about the current tree — no stale range aliasing the active scope.
    pub fn clear_nodes(&mut self) {
        self.first_node = usize::MAX;
        self.node_count = 0;
    }
    pub fn is_dragging(&self) -> bool {
        self.opacity.is_dragging() || self.led_brightness.is_dragging()
    }
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub fn set_collapsed(&mut self, v: bool) {
        self.is_collapsed = v;
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    // ── View description (chrome only — sliders dropped into slots) ──

    fn led_toggle_style(&self) -> UIStyle {
        // The kit state button — fills green when the LED output is live, recesses
        // to the neutral chip when off.
        UIStyle {
            font_size: FONT_SIZE,
            ..components::state_button_style(color::PLAY_GREEN, self.cached_led_enabled)
        }
    }

    fn divider() -> View {
        View::panel()
            .fill_w()
            .h(Sizing::Fixed(DIVIDER_H))
            .bg(color::DIVIDER_C32)
    }

    /// A 0–1 slider spec at the drag state's current cached value (defaulting a
    /// never-synced NaN to 1.0, as the old build did). `reset` is the caller's
    /// own trio (opacity vs LED brightness carry different `PanelAction`s even
    /// though both default to 1.0) — this shared helper just bundles it in.
    fn slider_spec(
        slider: &SliderDragState,
        label: Option<&str>,
        label_width: f32,
        reset: PanelAction,
    ) -> SliderSpec {
        let v = slider.cached_value();
        let v = if v.is_nan() { 1.0 } else { v };
        SliderSpec {
            label: label.map(str::to_string),
            value: v,
            // Both the opacity and LED-brightness sliders this helper builds
            // default to full-scale (1.0) — matches `reset`'s carried value.
            default: 1.0,
            value_text: fmt_opacity(v),
            colors: SliderColors::default_slider(),
            font_size: FONT_SIZE,
            label_width,
            reset,
        }
    }

    fn header_row(&self) -> View {
        let chevron = View::button(if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" })
            .fixed(CHEVRON_W, CHEVRON_H)
            .style(UIStyle {
                font_size: FONT_SIZE,
                ..components::icon_button_style()
            })
            .inert() // click handled via handle_click (inspector routing kept)
            .key(KEY_CHEVRON);

        // §6d — title + chevron, then the opacity slider inline (was a separate
        // stacked row). The LED row stays below in `chrome_view`.
        let mut row = View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(HEADER_ROW_H))
            .cross_align(Align::Center)
            .child(
                View::label("Master FX")
                    .font(color::FONT_HEADING)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(chevron);
        if !self.is_collapsed {
            row = row.child(
                View::slider_row(Self::slider_spec(
                    &self.opacity,
                    Some("Opacity"),
                    OPACITY_LABEL_W,
                    PanelAction::slider_reset(
                        PanelAction::Params(ParamsAction::MasterOpacitySnapshot),
                        PanelAction::Params(ParamsAction::MasterOpacityChanged(1.0)),
                        PanelAction::Params(ParamsAction::MasterOpacityCommit),
                    ),
                ))
                .fill_w()
                .h(Sizing::Fixed(HEADER_ROW_H))
                .key(KEY_OPACITY_SLOT),
            );
        }
        row
    }

    fn led_row(&self) -> View {
        let exit_path = View::button(self.cached_exit_path.as_str())
            .fill_w()
            .h(Sizing::Fixed(TOGGLE_H))
            .style(UIStyle {
                font_size: FONT_SIZE,
                ..components::button_secondary_style()
            })
            .inert()
            .key(KEY_EXIT_PATH);

        let toggle = View::button(if self.cached_led_enabled { "ON" } else { "OFF" })
            .w(Sizing::Fixed(LED_TOGGLE_W))
            .h(Sizing::Fixed(TOGGLE_H))
            .style(self.led_toggle_style())
            .inert()
            .key(KEY_LED_TOGGLE);

        // Brightness slider (no label) — the host materialises it into this slot.
        let brightness_slot = View::slider_row(Self::slider_spec(
            &self.led_brightness,
            None,
            0.0,
            PanelAction::slider_reset(
                PanelAction::Params(ParamsAction::LedBrightnessSnapshot),
                PanelAction::Params(ParamsAction::LedBrightnessChanged(1.0)),
                PanelAction::Params(ParamsAction::LedBrightnessCommit),
            ),
        ))
        .fixed(LED_SLIDER_W, SLIDER_ROW_H)
        .key(KEY_BRIGHTNESS_SLOT);

        View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(EXIT_PATH_ROW_H))
            .cross_align(Align::Center)
            .child(
                View::label("LED")
                    .w(Sizing::Fixed(LED_LABEL_W))
                    .fill_h()
                    .font(FONT_SIZE)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Left),
            )
            .child(exit_path)
            .child(toggle)
            .child(brightness_slot)
    }

    fn chrome_view(&self) -> View {
        let root = View::column(0.0)
            .fill()
            .pad(Pad { l: PAD_H, t: PAD_V, r: PAD_H, b: PAD_V })
            .child(self.header_row());
        if self.is_collapsed {
            return root;
        }
        root.child(Self::divider())
            .child(self.led_row())
            .child(Self::divider())
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.chrome_rect = rect;
        let view = self.chrome_view();
        self.host.build(tree, &view, rect);
        self.first_node = self.host.first_node();

        // The host materialised the slider slots; wire their ids to the drag
        // state (or clear when collapsed, where the slots aren't emitted).
        match self.host.slider_ids(KEY_BRIGHTNESS_SLOT) {
            Some(ids) => self.led_brightness.set_ids(ids),
            None => self.led_brightness.clear(),
        }
        match self.host.slider_ids(KEY_OPACITY_SLOT) {
            Some(ids) => self.opacity.set_ids(ids),
            None => self.opacity.clear(),
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// Reconcile the declarative chrome in place (value/style changes only —
    /// structural changes go through the inspector's rebuild on collapse).
    fn reconcile_chrome(&mut self, tree: &mut UITree) {
        if !self.host.is_built() {
            return;
        }
        let view = self.chrome_view();
        let _ = self.host.update(tree, &view, self.chrome_rect);
    }

    // ── Sync methods (called by state_sync; preserved signatures) ──

    pub fn sync_opacity(&mut self, tree: &mut UITree, value: f32) {
        self.opacity.sync(tree, value, &fmt_opacity);
    }

    pub fn sync_led_brightness(&mut self, tree: &mut UITree, value: f32) {
        self.led_brightness.sync(tree, value, &fmt_opacity);
    }

    pub fn sync_led_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        if enabled == self.cached_led_enabled {
            return;
        }
        self.cached_led_enabled = enabled;
        self.reconcile_chrome(tree);
    }

    pub fn sync_exit_path(&mut self, tree: &mut UITree, path: &str) {
        if self.cached_exit_path == path {
            return;
        }
        self.cached_exit_path = path.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_collapsed(&mut self, _tree: &mut UITree, collapsed: bool) {
        // Structural change — the inspector rebuilds on collapse toggle; here we
        // only record the flag so the next build emits the right shape.
        self.is_collapsed = collapsed;
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        if self.host.node_id_for_key(KEY_CHEVRON) == Some(node_id) {
            return vec![PanelAction::Params(ParamsAction::MasterCollapseToggle)];
        }
        if self.host.node_id_for_key(KEY_EXIT_PATH) == Some(node_id) {
            return vec![PanelAction::Params(ParamsAction::MasterExitPathClicked)];
        }
        if self.host.node_id_for_key(KEY_LED_TOGGLE) == Some(node_id) {
            return vec![PanelAction::Params(ParamsAction::LedEnabledToggle)];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2) -> Vec<PanelAction> {
        if let Some(val) = self.opacity.try_start_drag(node_id, pos.x) {
            return vec![
                PanelAction::Params(ParamsAction::MasterOpacitySnapshot),
                PanelAction::Params(ParamsAction::MasterOpacityChanged(val)),
            ];
        }
        if let Some(val) = self.led_brightness.try_start_drag(node_id, pos.x) {
            return vec![
                PanelAction::Params(ParamsAction::LedBrightnessSnapshot),
                PanelAction::Params(ParamsAction::LedBrightnessChanged(val)),
            ];
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if let Some(val) = self.opacity.apply_drag(pos.x, tree, &fmt_opacity) {
            return vec![PanelAction::Params(ParamsAction::MasterOpacityChanged(val))];
        }
        if let Some(val) = self.led_brightness.apply_drag(pos.x, tree, &fmt_opacity) {
            return vec![PanelAction::Params(ParamsAction::LedBrightnessChanged(val))];
        }
        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.opacity.end_drag() {
            return vec![PanelAction::Params(ParamsAction::MasterOpacityCommit)];
        }
        if self.led_brightness.end_drag() {
            return vec![PanelAction::Params(ParamsAction::LedBrightnessCommit)];
        }
        Vec::new()
    }

    /// Node-intent dispatch for the master chrome sliders' right-click resets,
    /// via the host's shared replay (BUG-061 follow-through).
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        self.host.register_slider_resets(intents);
    }

    pub fn exit_path_button_rect(&self, tree: &UITree) -> Rect {
        self.host
            .node_id_for_key(KEY_EXIT_PATH)
            .map(|id| tree.get_bounds(id))
            .unwrap_or(Rect::ZERO)
    }
}

impl Default for MasterChromePanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    // Golden oracle for the LED brightness slot (unchanged by §6d — still on the
    // LED row, the second row). The opacity slot moved inline onto the header row
    // and its X depends on the measured title width, so it's checked structurally.
    fn golden_brightness(rect: Rect) -> Rect {
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let cy = rect.y + PAD_V + HEADER_ROW_H + DIVIDER_H; // LED row top
        let btn_x = cx + LED_LABEL_W + GAP;
        let btn_w =
            (content_w - LED_LABEL_W - GAP - GAP - LED_TOGGLE_W - GAP - LED_SLIDER_W).max(20.0);
        let toggle_x = btn_x + btn_w + GAP;
        let slider_x = toggle_x + LED_TOGGLE_W + GAP;
        Rect::new(
            slider_x,
            cy + (EXIT_PATH_ROW_H - SLIDER_ROW_H) * 0.5,
            LED_SLIDER_W,
            SLIDER_ROW_H,
        )
    }

    #[test]
    fn slot_rects_match_golden() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        let rect = Rect::new(0.0, 0.0, 280.0, 200.0);
        panel.build(&mut tree, rect);

        let bright = golden_brightness(rect);
        let got_bright = tree.get_bounds(panel.host.node_id_for_key(KEY_BRIGHTNESS_SLOT).unwrap());
        let got_opacity = tree.get_bounds(panel.host.node_id_for_key(KEY_OPACITY_SLOT).unwrap());

        let close = |a: Rect, b: Rect| {
            (a.x - b.x).abs() < 0.01
                && (a.y - b.y).abs() < 0.01
                && (a.width - b.width).abs() < 0.01
                && (a.height - b.height).abs() < 0.01
        };
        assert!(close(got_bright, bright), "brightness slot {got_bright:?} != {bright:?}");
        // §6d: opacity is inline on the header row now.
        assert!(
            (got_opacity.y - (rect.y + PAD_V)).abs() < 0.01 && got_opacity.width > 0.0,
            "opacity slot not inline on header row: {got_opacity:?}"
        );
    }

    #[test]
    fn build_makes_chrome_and_sliders() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.host.is_built());
        assert!(panel.opacity.ids().is_some());
        assert!(panel.led_brightness.ids().is_some());
        assert!(panel.node_count > panel.host.node_count(), "slider nodes appended after chrome");
        assert!(panel.host.node_id_for_key(KEY_CHEVRON).is_some());
    }

    #[test]
    fn collapsed_drops_sliders_and_shrinks() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        let expanded_h = panel.compute_height();

        panel.set_collapsed(true);
        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.compute_height() < expanded_h);
        assert!(panel.opacity.ids().is_none(), "no opacity slider when collapsed");
        assert!(panel.host.node_id_for_key(KEY_OPACITY_SLOT).is_none());
    }

    #[test]
    fn handle_click_chevron_and_exit_path() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let chev = panel.host.node_id_for_key(KEY_CHEVRON).unwrap();
        assert!(matches!(
            panel.handle_click(chev).as_slice(),
            [PanelAction::Params(ParamsAction::MasterCollapseToggle)]
        ));
        let exit = panel.host.node_id_for_key(KEY_EXIT_PATH).unwrap();
        assert!(matches!(
            panel.handle_click(exit).as_slice(),
            [PanelAction::Params(ParamsAction::MasterExitPathClicked)]
        ));
    }

    #[test]
    fn sync_exit_path_updates_in_place() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        let sv = tree.structure_version();

        panel.sync_exit_path(&mut tree, "Additive");
        let exit = panel.host.node_id_for_key(KEY_EXIT_PATH).unwrap();
        assert_eq!(tree.get_node(exit).unwrap().text.as_deref(), Some("Additive"));
        assert_eq!(tree.structure_version(), sv, "value change must not rebuild");
    }

    #[test]
    fn drag_lifecycle() {
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let track_id = panel.opacity.track_id().unwrap();
        let track_rect = tree.get_bounds(panel.opacity.track_id().unwrap());
        let mid_x = track_rect.x + track_rect.width * 0.5;

        let actions = panel.handle_pointer_down(track_id, Vec2::new(mid_x, 10.0));
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::MasterOpacitySnapshot)));
        assert!(panel.is_dragging());

        let actions = panel.handle_drag(Vec2::new(mid_x + 10.0, 10.0), &mut tree);
        assert_eq!(actions.len(), 1);

        let actions = panel.handle_drag_end(&mut tree);
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::MasterOpacityCommit)));
        assert!(!panel.is_dragging());
    }

    #[test]
    fn right_click_on_either_slider_track_resolves_to_slider_reset_with_declared_default() {
        // both master-chrome sliders' reset now rides the generic
        // SliderReset trio (not a bespoke *RightClick), and each carries its
        // own declared default (1.0 for both opacity and LED brightness).
        let mut tree = UITree::new();
        let mut panel = MasterChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let opacity_track = panel.opacity.track_id().unwrap();
        match reg.resolve(&tree, Some(opacity_track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) => {
                assert!(matches!(*changed, PanelAction::Params(ParamsAction::MasterOpacityChanged(v)) if (v - 1.0).abs() < f32::EPSILON));
            }
            other => panic!("expected SliderReset, got {other:?}"),
        }

        let brightness_track = panel.led_brightness.track_id().unwrap();
        match reg.resolve(&tree, Some(brightness_track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) => {
                assert!(matches!(*changed, PanelAction::Params(ParamsAction::LedBrightnessChanged(v)) if (v - 1.0).abs() < f32::EPSILON));
            }
            other => panic!("expected SliderReset, got {other:?}"),
        }
    }
}
