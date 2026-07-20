//! Macros panel: 8 horizontal sliders spanning the full inspector width, on the
//! Chrome API.
//!
//! The host owns the declarative chrome — the section card (border + inner bg),
//! the header + collapse chevron, the 8 `slider_row` slots and the conditional
//! Ableton-config-drawer slots. The sliders are materialised by the host (typed
//! building block); the per-slot Ableton trim handles (built into each slider
//! track) and the config drawers (built into their keyed slots) stay imperative
//! sub-widgets — the next blocks to typify. Public interface unchanged, so the
//! inspector composite is untouched.

use super::PanelAction;
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::{
    ABL_CONFIG_HEIGHT, AbletonConfigClick, AbletonConfigIds, AbletonMappingDisplay, TrimHandleIds,
    build_ableton_config, build_trim_handles_explicit, reposition_trim_bars,
    check_ableton_config_click,
};
use crate::chrome::{Align, ChromeHost, Pad, Sizing, SliderSpec, View};
use crate::color;
use crate::drag::DragController;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderDragState, TrackSpan};
use crate::tree::UITree;
use crate::types::MACRO_COUNT;

// ── Layout constants ───────────────────────────────────────────────

// Match the inspector param rows so the macros bank reads as one instrument with
// the cards below it (same row height + gap).
const ROW_HEIGHT: f32 = 24.0;
const ROW_SPACING: f32 = 6.0;
const HEADER_ROW_H: f32 = 22.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 4.0;
const PAD_H: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const CHEVRON_H: f32 = 16.0;
const GAP: f32 = 4.0;
const LABEL_WIDTH: f32 = crate::slider::DEFAULT_LABEL_WIDTH;
const FONT_SIZE: u16 = color::FONT_BODY;

const SECTION_BORDER: Color32 = Color32::new(50, 50, 54, 255);
const SECTION_BG: Color32 = Color32::new(22, 22, 23, 255);

const KEY_CHEVRON: u64 = 1;
const KEY_SLIDER_BASE: u64 = 10;
const KEY_CONFIG_BASE: u64 = 20;

fn fmt_macro(v: f32) -> String {
    format!("{:.2}", v)
}

// what an Ableton-range
// trim-bar drag targets — folds the old `i32` slot-index sentinel (−1 idle)
// plus its parallel min/max bool onto `DragController`, the same
// sentinel-pair disease P7.1's `ParamDragTarget` was built to cure (D8).
#[derive(Debug, Clone, Copy, PartialEq)]
struct AbletonTrimDrag {
    /// Which macro slot's trim bar is being dragged.
    index: usize,
    /// `true` if the min-edge handle is being dragged, `false` for max.
    is_min: bool,
}

// ── MacrosPanel ────────────────────────────────────────────────────

pub struct MacrosPanel {
    host: ChromeHost,
    sliders: [SliderDragState; MACRO_COUNT],
    first_node: Option<NodeId>,
    node_count: usize,
    copied_flash: CopyToClipboardLabelState,
    /// Ableton trim handle node IDs per macro slot.
    ableton_trim_ids: [Option<TrimHandleIds>; MACRO_COUNT],
    /// Ableton config drawer node IDs per macro slot (status dot + name + INV).
    ableton_config_ids: [Option<AbletonConfigIds>; MACRO_COUNT],
    /// Cached Ableton display data per slot (for build).
    ableton_displays: [Option<AbletonMappingDisplay>; MACRO_COUNT],
    /// Cached Ableton range per slot (for drag updates + build).
    ableton_ranges: [Option<(f32, f32)>; MACRO_COUNT],
    /// Which macro slot's Ableton trim bar is being dragged, if any —
    /// replaces the old `i32` slot-index sentinel (−1 idle) plus
    /// its parallel min/max bool field.
    ableton_trim_drag: DragController<AbletonTrimDrag>,
    is_collapsed: bool,
}

impl Default for MacrosPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl MacrosPanel {
    fn display_label(labels: &[String], index: usize) -> String {
        labels
            .get(index)
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("M{}", index + 1))
    }

    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            sliders: std::array::from_fn(|_| SliderDragState::with_range(0.0, 1.0, false)),
            first_node: None,
            node_count: 0,
            copied_flash: CopyToClipboardLabelState::default(),
            ableton_trim_ids: std::array::from_fn(|_| None),
            ableton_config_ids: std::array::from_fn(|_| None),
            ableton_displays: std::array::from_fn(|_| None),
            ableton_ranges: [None; MACRO_COUNT],
            ableton_trim_drag: DragController::new(),
            // Default closed; project load overrides via set_collapsed().
            is_collapsed: true,
        }
    }

    /// Total height of the macros panel (for inspector column Y offset).
    pub fn height(&self) -> f32 {
        if self.is_collapsed {
            return PAD_TOP + HEADER_ROW_H + PAD_BOTTOM;
        }
        let mut h = PAD_TOP + HEADER_ROW_H + PAD_BOTTOM;
        for i in 0..MACRO_COUNT {
            h += ROW_HEIGHT;
            if self.ableton_displays[i].is_some() {
                h += ABL_CONFIG_HEIGHT;
            }
            if i + 1 < MACRO_COUNT {
                h += ROW_SPACING;
            }
        }
        h
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

    pub fn first_node(&self) -> usize {
        self.first_node.map_or(usize::MAX, |id| id.index())
    }

    /// Set Ableton display data per macro slot (call before build).
    pub fn set_ableton_displays(&mut self, displays: &[Option<AbletonMappingDisplay>]) {
        for (i, d) in displays.iter().enumerate().take(MACRO_COUNT) {
            self.ableton_displays[i] = d.clone();
        }
    }

    /// Set Ableton trim ranges per macro slot (call before build or sync).
    pub fn set_ableton_ranges(&mut self, ranges: &[Option<(f32, f32)>]) {
        for (i, r) in ranges.iter().enumerate().take(MACRO_COUNT) {
            self.ableton_ranges[i] = *r;
        }
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    pub fn is_dragging(&self) -> bool {
        self.sliders.iter().any(|s| s.is_dragging())
    }

    /// Sync cached macro values from project state.
    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32], labels: &[String]) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| {
                self.sliders
                    .iter()
                    .enumerate()
                    .find_map(|(i, s)| {
                        s.ids()
                            .filter(|ids| ids.label == Some(label_id))
                            .map(|_| Self::display_label(labels, i))
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(ids) = s.ids()
                && let Some(label) = ids.label
                && Some(label) != self.copied_flash.label_id()
            {
                tree.set_text(label, &Self::display_label(labels, i));
            }
            if let Some(&v) = values.get(i) {
                s.sync(tree, v, &fmt_macro);
            }
        }
    }

    // ── View description (chrome + slider/config slots) ──────────────

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
                View::label("Macros")
                    .fill_w()
                    .fill_h()
                    .font(color::FONT_HEADING)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(chevron);

        // Inner card surface (bg), inset 1px inside the border, content padded so
        // the first row lands at the same place the old constant layout used.
        let mut inner = View::column(0.0)
            .fill()
            .bg(SECTION_BG)
            .radius(color::BUTTON_RADIUS)
            .pad(Pad {
                l: PAD_H - 1.0,
                t: PAD_TOP - 1.0,
                r: PAD_H - 1.0,
                b: PAD_BOTTOM - 1.0,
            })
            .child(header);

        if !self.is_collapsed {
            for i in 0..MACRO_COUNT {
                let v = self.sliders[i].cached_value();
                let v = if v.is_nan() { 0.0 } else { v };
                let spec = SliderSpec {
                    label: Some(format!("M{}", i + 1)),
                    value: v,
                    default: 0.0,
                    value_text: fmt_macro(v),
                    colors: SliderColors::default_slider(),
                    font_size: FONT_SIZE,
                    label_width: LABEL_WIDTH,
                    reset: PanelAction::slider_reset(
                        PanelAction::MacroSnapshot(i),
                        PanelAction::MacroChanged(i, 0.0),
                        PanelAction::MacroCommit(i),
                    ),
                };
                inner = inner.child(
                    View::slider_row(spec)
                        .fill_w()
                        .h(Sizing::Fixed(ROW_HEIGHT))
                        .key(KEY_SLIDER_BASE + i as u64),
                );
                if self.ableton_displays[i].is_some() {
                    inner = inner.child(
                        View::panel()
                            .fill_w()
                            .h(Sizing::Fixed(ABL_CONFIG_HEIGHT))
                            .key(KEY_CONFIG_BASE + i as u64),
                    );
                }
                if i + 1 < MACRO_COUNT {
                    inner = inner.child(View::panel().fill_w().h(Sizing::Fixed(ROW_SPACING)));
                }
            }
        }

        View::panel()
            .fill()
            .bg(SECTION_BORDER)
            .radius(color::CARD_RADIUS)
            .pad(Pad::all(1.0))
            .child(inner)
    }

    /// Build the macros panel into the tree at the given rect.
    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        let view = self.chrome_view();
        self.host.build(tree, &view, rect);
        self.first_node = self.host.node_id(0);

        // Wire each materialised slider, then build its Ableton trim handles +
        // config drawer (imperative sub-widgets) into the host-laid slots.
        for i in 0..MACRO_COUNT {
            match self.host.slider_ids(KEY_SLIDER_BASE + i as u64) {
                Some(ids) => {
                    self.sliders[i].set_ids(ids);
                    if let Some((amin, amax)) = self.ableton_ranges[i] {
                        self.ableton_trim_ids[i] = Some(build_trim_handles_explicit(
                            tree,
                            ids.track,
                            tree.get_bounds(ids.track),
                            amin,
                            amax,
                            color::ABL_TRIM_BAR_C32,
                            color::ABL_TRIM_BAR_HOVER_C32,
                            color::ABL_TRIM_FILL_C32,
                        ));
                    } else {
                        self.ableton_trim_ids[i] = None;
                    }
                }
                None => {
                    self.sliders[i].clear();
                    self.ableton_trim_ids[i] = None;
                }
            }

            if let Some(display) = self.ableton_displays[i].clone() {
                if let Some(slot) = self
                    .host
                    .node_id_for_key(KEY_CONFIG_BASE + i as u64)
                    .map(|id| tree.get_bounds(id))
                {
                    self.ableton_config_ids[i] =
                        Some(build_ableton_config(tree, None, slot.x, slot.y, slot.width, &display, None));
                } else {
                    self.ableton_config_ids[i] = None;
                }
            } else {
                self.ableton_config_ids[i] = None;
            }
        }

        let first = self.first_node.map_or(0, |id| id.index());
        self.node_count = tree.count() - first;
    }

    /// Handle press on a slider track or trim bar.
    pub fn handle_press(&mut self, node_id: NodeId, pos_x: f32) -> Vec<PanelAction> {
        // Check Ableton trim bars first (higher z-order)
        for (i, trim) in self.ableton_trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id == t.min_bar_id {
                    self.ableton_trim_drag
                        .start(AbletonTrimDrag { index: i, is_min: true }, Vec2::new(pos_x, 0.0));
                    return vec![PanelAction::AbletonMacroTrimSnapshot(i)];
                }
                if node_id == t.max_bar_id {
                    self.ableton_trim_drag
                        .start(AbletonTrimDrag { index: i, is_min: false }, Vec2::new(pos_x, 0.0));
                    return vec![PanelAction::AbletonMacroTrimSnapshot(i)];
                }
            }
        }
        // Slider track drag
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(val) = s.try_start_drag(node_id, pos_x) {
                return vec![
                    PanelAction::MacroSnapshot(i),
                    PanelAction::MacroChanged(i, val),
                ];
            }
        }
        Vec::new()
    }

    /// Handle drag (pointer move while pressed).
    pub fn handle_drag(&mut self, pos_x: f32, tree: &mut UITree) -> Vec<PanelAction> {
        // Ableton trim drag
        if let Some(&AbletonTrimDrag { index: i, is_min }) = self.ableton_trim_drag.payload()
            && let Some((cur_min, cur_max)) = self.ableton_ranges[i]
            && let Some(ids) = self.sliders[i].ids()
        {
            // Live bounds, not the cached span: in-place scroll shifts the
            // tree nodes without refreshing panel caches (BUG-257/259).
            let track_rect = tree.get_bounds(ids.track);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos_x);
            let (new_min, new_max) = if is_min {
                (norm.clamp(0.0, cur_max), cur_max)
            } else {
                (cur_min, norm.clamp(cur_min, 1.0))
            };
            self.ableton_ranges[i] = Some((new_min, new_max));

            if let Some(t) = &self.ableton_trim_ids[i] {
                reposition_trim_bars(tree, track_rect, t, new_min, new_max);
            }

            return vec![PanelAction::AbletonMacroTrimChanged(i, new_min, new_max)];
        }
        // Slider drag
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(val) = s.apply_drag(pos_x, tree, &fmt_macro) {
                return vec![PanelAction::MacroChanged(i, val)];
            }
        }
        Vec::new()
    }

    /// Handle pointer up — commit the drag.
    pub fn handle_release(&mut self) -> Vec<PanelAction> {
        if let Some(AbletonTrimDrag { index: i, .. }) = self.ableton_trim_drag.release() {
            return vec![PanelAction::AbletonMacroTrimCommit(i)];
        }
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if s.end_drag() {
                return vec![PanelAction::MacroCommit(i)];
            }
        }
        Vec::new()
    }

    /// Handle click — chevron toggles collapse, label click copies OSC address,
    /// INV button toggles invert.
    pub fn handle_click(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        if self.host.node_id_for_key(KEY_CHEVRON) == Some(node_id) {
            return vec![PanelAction::MacrosCollapseToggle];
        }

        if let Some((slot_idx, AbletonConfigClick::Invert)) =
            check_ableton_config_click(node_id, &self.ableton_config_ids)
        {
            return vec![PanelAction::AbletonMacroInvertToggle(slot_idx)];
        }

        for (i, s) in self.sliders.iter().enumerate() {
            if let Some(ids) = s.ids()
                && ids.label == Some(node_id)
            {
                self.copied_flash.trigger(node_id);
                let addr = format!("/macro/{}", i + 1);
                return vec![PanelAction::CopyOscAddress(addr)];
            }
        }
        Vec::new()
    }

    /// Node-intent dispatch for the macro sliders: track → reset (via the
    /// host's shared replay), label → open mappings dropdown (P3/D14: through
    /// the contract's `register_label_mapping`, not a hand `intents.on`).
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        self.host.register_slider_resets(intents);
        for (i, s) in self.sliders.iter().enumerate() {
            if let Some(ids) = s.ids() {
                BitmapSlider::register_label_mapping(ids, &PanelAction::MacroLabelRightClick(i), intents);
            }
        }
    }

    /// Check if a node belongs to this panel.
    pub fn owns_node(&self, node_id: NodeId) -> bool {
        let Some(first) = self.first_node else {
            return false;
        };
        let id = node_id.index();
        id >= first.index() && id < first.index() + self.node_count
    }

    pub fn label_rect(&self, tree: &UITree, index: usize) -> Option<Rect> {
        self.sliders
            .get(index)
            .and_then(|slider| slider.ids())
            .and_then(|ids| ids.label)
            .map(|label| tree.get_bounds(label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 300.0, 220.0)
    }

    #[test]
    fn collapsed_builds_only_header() {
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new(); // default collapsed
        panel.build(&mut tree, rect());
        assert!(panel.host.node_id_for_key(KEY_CHEVRON).is_some());
        assert!(panel.host.slider_ids(KEY_SLIDER_BASE).is_none());
        assert!(panel.sliders.iter().all(|s| s.ids().is_none()));
    }

    #[test]
    fn expanded_materialises_eight_sliders() {
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        panel.build(&mut tree, rect());
        for i in 0..MACRO_COUNT {
            assert!(
                panel.sliders[i].ids().is_some(),
                "slider {i} materialised + wired"
            );
        }
    }

    #[test]
    fn slider_slots_match_golden_layout() {
        // Each slider slot lands at the old constant cy-tracked rect.
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        let r = rect();
        panel.build(&mut tree, r);

        let inner_x = r.x + PAD_H;
        let inner_w = r.width - PAD_H * 2.0;
        let mut cy = r.y + PAD_TOP + HEADER_ROW_H;
        for i in 0..MACRO_COUNT {
            let slot = tree.get_bounds(panel.host.node_id_for_key(KEY_SLIDER_BASE + i as u64).unwrap());
            let want = Rect::new(inner_x, cy, inner_w, ROW_HEIGHT);
            assert!(
                (slot.x - want.x).abs() < 0.01
                    && (slot.y - want.y).abs() < 0.01
                    && (slot.width - want.width).abs() < 0.01
                    && (slot.height - want.height).abs() < 0.01,
                "macro {i} slot {slot:?} != {want:?}"
            );
            cy += ROW_HEIGHT;
            if i + 1 < MACRO_COUNT {
                cy += ROW_SPACING;
            }
        }
    }

    #[test]
    fn chevron_click_toggles_collapse() {
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.build(&mut tree, rect());
        let chev = panel.host.node_id_for_key(KEY_CHEVRON).unwrap();
        assert!(matches!(
            panel.handle_click(chev).as_slice(),
            [PanelAction::MacrosCollapseToggle]
        ));
    }

    #[test]
    fn config_drawer_grows_height() {
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        let base = panel.height();
        let mut displays: Vec<Option<AbletonMappingDisplay>> = vec![None; MACRO_COUNT];
        displays[0] = Some(AbletonMappingDisplay {
            macro_name: "Macro 1".into(),
            track_name: "Track".into(),
            device_name: "Dev".into(),
            status: crate::panels::param_slider_shared::AbletonMappingStatus::Active,
            inverted: false,
        });
        panel.set_ableton_displays(&displays);
        assert!(panel.height() > base);
    }

    #[test]
    fn ableton_trim_drag_lifecycle_matches_press_drag_release_contract() {
        // exercises the real handle_press/handle_drag/
        // handle_release path for the Ableton range trim-bar drag.
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        let mut ranges: Vec<Option<(f32, f32)>> = vec![None; MACRO_COUNT];
        ranges[0] = Some((0.2, 0.8));
        panel.set_ableton_ranges(&ranges);
        panel.build(&mut tree, rect());

        let trim = panel.ableton_trim_ids[0].expect("macro 0 has an ableton range, trim handles built");

        // Press the MIN bar on macro 0.
        let press = panel.handle_press(trim.min_bar_id, 0.3);
        assert!(
            matches!(press.as_slice(), [PanelAction::AbletonMacroTrimSnapshot(0)]),
            "press min bar: {press:?}"
        );

        // Drag toward a new min. Expected value computed via the same
        // x_to_normalized helper the production path uses, clamped to cur_max.
        let track_rect = tree.get_bounds(panel.sliders[0].track_id().unwrap());
        let pos_x = track_rect.x + track_rect.width * 0.35;
        let expected_norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos_x).clamp(0.0, 0.8);
        let drag = panel.handle_drag(pos_x, &mut tree);
        match drag.as_slice() {
            [PanelAction::AbletonMacroTrimChanged(0, new_min, new_max)] => {
                assert!((new_min - expected_norm).abs() < 1e-6, "new_min={new_min} expected={expected_norm}");
                assert!((new_max - 0.8).abs() < 1e-6, "new_max={new_max} should hold at cur_max");
            }
            other => panic!("expected AbletonMacroTrimChanged(0, ..), got {other:?}"),
        }

        // Release commits macro 0.
        let release = panel.handle_release();
        assert!(
            matches!(release.as_slice(), [PanelAction::AbletonMacroTrimCommit(0)]),
            "release: {release:?}"
        );

        // A second release (nothing in flight) is a no-op — proves the drag
        // truly ended, not just that the first release fired once.
        assert!(panel.handle_release().is_empty());

        // Press the MAX bar on macro 0 to exercise the other arm.
        let press_max = panel.handle_press(trim.max_bar_id, 0.9);
        assert!(
            matches!(press_max.as_slice(), [PanelAction::AbletonMacroTrimSnapshot(0)]),
            "press max bar: {press_max:?}"
        );
        let pos_x_max = track_rect.x + track_rect.width * 0.95;
        // cur_min is whatever the previous drag committed it to (expected_norm),
        // since ableton_ranges was mutated in place during the first drag.
        let expected_max = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos_x_max).clamp(expected_norm, 1.0);
        let drag_max = panel.handle_drag(pos_x_max, &mut tree);
        match drag_max.as_slice() {
            [PanelAction::AbletonMacroTrimChanged(0, new_min, new_max)] => {
                assert!((new_min - expected_norm).abs() < 1e-6);
                assert!((new_max - expected_max).abs() < 1e-6);
            }
            other => panic!("expected AbletonMacroTrimChanged(0, ..), got {other:?}"),
        }
        let release_max = panel.handle_release();
        assert!(matches!(release_max.as_slice(), [PanelAction::AbletonMacroTrimCommit(0)]));
    }

    /// after an in-place scroll (every
    /// content node's y shifted, no rebuild), an Ableton trim drag must place
    /// the bars at the track's LIVE y.
    #[test]
    fn ableton_trim_bars_follow_the_track_after_scroll() {
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        let mut ranges: Vec<Option<(f32, f32)>> = vec![None; MACRO_COUNT];
        ranges[0] = Some((0.2, 0.8));
        panel.set_ableton_ranges(&ranges);
        panel.build(&mut tree, rect());

        let trim = panel.ableton_trim_ids[0].expect("trim built");
        let track = panel.sliders[0].track_id().unwrap();

        // What ScrollContainer::offset_content does on a wheel scroll.
        for i in 0..tree.count() {
            let id = tree.id_at(i);
            let mut b = tree.get_bounds(id);
            b.y += 137.0;
            tree.set_bounds(id, b);
        }

        panel.handle_press(trim.min_bar_id, 0.3);
        let live = tree.get_bounds(track);
        let drag = panel.handle_drag(live.x + live.width * 0.5, &mut tree);
        assert!(
            matches!(drag.as_slice(), [PanelAction::AbletonMacroTrimChanged(0, ..)]),
            "trim drag still routes after scroll: {drag:?}"
        );

        for (name, id) in [("min_bar", trim.min_bar_id), ("max_bar", trim.max_bar_id), ("fill", trim.fill_id)] {
            let y = tree.get_bounds(id).y;
            assert!(
                (y - live.y).abs() <= 4.0,
                "{name} y={y} should track the live track y={} (stale cache would put it ~137px up)",
                live.y
            );
        }
    }

    #[test]
    fn right_click_on_macro_track_resolves_to_slider_reset_with_zero_default() {
        // the per-macro reset now rides the generic SliderReset trio,
        // carrying the macro's own default of 0.0.
        let mut tree = UITree::new();
        let mut panel = MacrosPanel::new();
        panel.set_collapsed(false);
        panel.build(&mut tree, rect());

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let track = panel.sliders[0].track_id().unwrap();
        match reg.resolve(&tree, Some(track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::SliderReset { changed, .. }) => {
                assert!(matches!(*changed, PanelAction::MacroChanged(0, v) if v.abs() < f32::EPSILON));
            }
            other => panic!("expected SliderReset, got {other:?}"),
        }
    }
}
