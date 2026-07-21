//! AUDIO TRIGGERS — the inspector's layer-owned clip-trigger authoring
//! section (P3b, `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`;
//! drawer redesigned 2026-07-19).
//!
//! A single collapsible section pinned at the top of the selected layer's
//! inspector content, default-collapsed (Peter, 2026-07-10: "a single
//! section that sits at the top of the inspector for the layer and is
//! default collapsed"). Collapse mechanics mirror `macros_panel.rs`
//! (`is_collapsed`, default `true`, chevron toggle) and `layer_chrome.rs`
//! (the same pattern, used for a section that lives INSIDE the layer's own
//! card rather than as its own bordered strip — this section follows THAT
//! precedent since it sits inside the layer column like `layer_chrome`
//! does, not as an independent top-level strip like `MacrosPanel`).
//!
//! Each row is one `Layer.clip_triggers` entry: an ON/OFF toggle, a label
//! that expands/collapses the row's drawer, and a remove button. The
//! expanded drawer is [`build_clip_trigger_drawer`] — a purpose-built
//! surface, NOT the shared param-mod drawer: a clip trigger fires on the raw
//! sensitivity-scaled signal against a fixed edge, so that drawer's
//! Attack/Release/Invert rows would be knobs that do nothing here (the
//! shaped envelope only conditions continuous modulation and the meter), and
//! its 8×4 Feature×Band matrix is the wrong vocabulary for an onset. The
//! drawer is four rows: Source (send), Listen (curated trigger-source
//! chips — see `TRIGGER_SOURCE_CHIPS`), Sensitivity (with the live fire
//! meter), Length.
//!
//! This module owns its OWN click/drag dispatch (Source/chip/Length button
//! clicks, the Sensitivity slider drag) — the same division of labor
//! `ParamCardPanel` has with its drawer builder (builder = visuals + the
//! slider's reset action; click/drag resolution = caller-owned).

use crate::{AudioSetupAction};
use super::drawer::DrawerIds;
use super::param_slider_shared::{
    AUDIO_ATTACK_DEFAULT_MS, AUDIO_MOD_ACTIVE_C32, AUDIO_RELEASE_DEFAULT_MS, AUDIO_SENS_MAX,
    AudioCardState, AudioRowState, DRAWER_BOTTOM_GAP, FONT_SIZE, LENGTH_OPTIONS, ParamModState,
    ROW_HEIGHT,
    audio_band_from_index, audio_kind_from_index, build_clip_trigger_drawer,
    clip_trigger_drawer_height, de_btn_style, toggle_btn_style, trigger_source_chips,
};
use super::{AudioShapeParam, PanelAction};
use crate::chrome::{Align, ChromeHost, Sizing, View};
use crate::color;
use crate::drag::DragController;
use crate::node::*;
use crate::slider::BitmapSlider;
use crate::tree::UITree;
use crate::types::AudioFeature;
use manifold_foundation::{AudioSendId, LayerId};

const HEADER_ROW_H: f32 = 22.0;
const ADD_ROW_H: f32 = 22.0;
const ROW_SPACING: f32 = 4.0;
const ENABLE_BTN_W: f32 = 40.0;
const REMOVE_BTN_W: f32 = 22.0;
const GAP: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const CHEVRON_H: f32 = 16.0;

const KEY_CHEVRON: u64 = 1;
const KEY_ADD_BTN: u64 = 2;
const KEY_EMPTY_LABEL: u64 = 3;
// Per-row keys space out by 1000 so a section can hold hundreds of rows
// without the row-line/drawer-slot key ranges colliding.
const KEY_ROW_TOGGLE_BASE: u64 = 1_000;
const KEY_ROW_LABEL_BASE: u64 = 2_000;
const KEY_ROW_REMOVE_BASE: u64 = 3_000;
const KEY_ROW_DRAWER_BASE: u64 = 4_000;

/// Structural config for the section, assembled in `state_sync` from a
/// layer's `clip_triggers` — the panel data boundary (state_sync stays the
/// sole source; this panel never reads `Project`). Only what the drawer
/// actually shows: the source cell (send + feature), the fire-edge tuning
/// (sensitivity), and the one-shot length. Attack/release/invert are
/// continuous-envelope shaping — they don't gate a fire edge, so this
/// surface neither displays nor edits them (the model fields keep their
/// defaults).
#[derive(Debug, Clone, Default)]
pub struct AudioTriggerRowConfig {
    pub enabled: bool,
    /// Precomputed "{band} → {feature kind}" row label (state_sync's job —
    /// this panel never reaches into `manifold-core` for `.label()`).
    pub label: String,
    pub kind_idx: i32,
    pub band_idx: i32,
    pub sensitivity: f32,
    pub send_id: Option<AudioSendId>,
    pub one_shot_beats: f32,
}

#[derive(Debug, Clone, Default)]
pub struct AudioTriggerSectionConfig {
    pub rows: Vec<AudioTriggerRowConfig>,
    /// Card-level: every project audio send, for the drawer's Source row.
    pub send_labels: Vec<String>,
    pub send_ids: Vec<AudioSendId>,
}

pub struct AudioTriggerSection {
    host: ChromeHost,
    is_collapsed: bool,
    layer_id: Option<LayerId>,
    labels: Vec<String>,
    enabled: Vec<bool>,
    one_shot_beats: Vec<f32>,
    mod_state: ParamModState,
    /// Per-row: is this row's drawer open. UI-local (mirrors `is_collapsed`);
    /// resized on every `configure()`, preserving the existing prefix so a
    /// row that's still at the same index keeps its expand state across a
    /// value-only resync.
    expanded: Vec<bool>,
    audio_configs: Vec<Option<(DrawerIds, usize)>>,
    first_node: Option<NodeId>,
    node_count: usize,
    /// `(row_index, which)` of the shaping slider currently being dragged.
    /// Only `AudioShapeParam::Sensitivity` is grabbable (it's the drawer's
    /// only slider); the payload keeps the enum so the action family stays
    /// uniformly typed. Lifecycle on `DragController` (P7,
    /// `docs/UI_WIDGET_UNIFICATION_DESIGN.md`).
    dragging_shape: DragController<(usize, AudioShapeParam)>,
}

impl Default for AudioTriggerSection {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioTriggerSection {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            is_collapsed: true,
            layer_id: None,
            labels: Vec::new(),
            enabled: Vec::new(),
            one_shot_beats: Vec::new(),
            mod_state: ParamModState::allocate(0),
            expanded: Vec::new(),
            audio_configs: Vec::new(),
            first_node: None,
            node_count: 0,
            dragging_shape: DragController::new(),
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn toggle_row_expanded(&mut self, index: usize) {
        if let Some(e) = self.expanded.get_mut(index) {
            *e = !*e;
        }
    }

    /// Open one row's drawer without toggling — the Add flow uses this so a
    /// freshly-created trigger's tuning is immediately visible. Grows the
    /// vector if the row hasn't been configured into existence yet (Add's
    /// structural resync lands after the action dispatch).
    pub fn expand_row(&mut self, index: usize) {
        if index >= self.expanded.len() {
            self.expanded.resize(index + 1, false);
        }
        self.expanded[index] = true;
    }

    pub fn layer_id(&self) -> Option<&LayerId> {
        self.layer_id.as_ref()
    }

    pub fn row_count(&self) -> usize {
        self.labels.len()
    }

    /// Structural configure — rebuilds the row list from the layer's current
    /// `clip_triggers`. Called from `sync_inspector_data` (structural pass),
    /// mirroring `configure_layer_effects`.
    /// Range-truthfulness reset (`inspector.rs`'s `build_in_rect` invariant):
    /// called up front, before the scope-gated build pass, so a frame where
    /// this section doesn't build (Master/Clip tab active, or the layer
    /// chrome collapsed) honestly reports an empty node range instead of
    /// aliasing last frame's indices.
    pub fn clear_nodes(&mut self) {
        self.first_node = None;
        self.node_count = 0;
    }

    pub fn configure(&mut self, layer_id: Option<LayerId>, config: &AudioTriggerSectionConfig) {
        self.layer_id = layer_id;
        let n = config.rows.len();
        self.labels = config.rows.iter().map(|r| r.label.clone()).collect();
        self.enabled = config.rows.iter().map(|r| r.enabled).collect();
        self.one_shot_beats = config.rows.iter().map(|r| r.one_shot_beats).collect();
        // Truncate-then-grow: `resize` alone never shrinks, so a removed row
        // used to leave a stale tail and shift every later row's flag.
        self.expanded.truncate(n);
        self.expanded.resize(n, false);

        // The drawer reads send/kind/band/sensitivity out of `ParamModState`
        // (the shared audio-mod display state). The envelope fields a clip
        // trigger never shows are filled with their defaults — inert here:
        // no row displays them and no gesture writes them.
        let audio = AudioCardState {
            rows: config
                .rows
                .iter()
                .map(|r| AudioRowState {
                    active: r.enabled,
                    send_id: r.send_id.clone(),
                    kind_idx: r.kind_idx,
                    band_idx: r.band_idx,
                    sensitivity: r.sensitivity,
                    attack_ms: AUDIO_ATTACK_DEFAULT_MS,
                    release_ms: AUDIO_RELEASE_DEFAULT_MS,
                    ..Default::default()
                })
                .collect(),
            send_labels: config.send_labels.clone(),
            send_ids: config.send_ids.clone(),
        };
        self.mod_state = ParamModState::allocate(n);
        self.mod_state.sync_audio(n, &audio);
    }

    /// Total height this section occupies at the top of the layer column.
    pub fn height(&self) -> f32 {
        if self.is_collapsed {
            return HEADER_ROW_H;
        }
        let mut h = HEADER_ROW_H + ROW_SPACING;
        if self.labels.is_empty() {
            return h + ROW_HEIGHT + ROW_SPACING + ADD_ROW_H;
        }
        for i in 0..self.labels.len() {
            h += ROW_HEIGHT + ROW_SPACING;
            if self.expanded.get(i).copied().unwrap_or(false) {
                h += clip_trigger_drawer_height() + DRAWER_BOTTOM_GAP + ROW_SPACING;
            }
        }
        h + ADD_ROW_H
    }

    fn row_label_style(expanded: bool) -> UIStyle {
        UIStyle {
            bg_color: if expanded {
                Color32::new(44, 44, 50, 255)
            } else {
                Color32::new(32, 32, 35, 255)
            },
            hover_bg_color: color::HOVER_OVERLAY,
            pressed_bg_color: color::PRESS_OVERLAY,
            text_color: color::TEXT_PRIMARY_C32,
            font_size: FONT_SIZE,
            text_align: TextAlign::Left,
            corner_radius: color::SMALL_RADIUS,
            ..UIStyle::default()
        }
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
                View::label("AUDIO TRIGGERS")
                    .fill_w()
                    .fill_h()
                    .font(color::FONT_HEADING)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(chevron);

        let mut col = View::column(ROW_SPACING).fill_w().child(header);

        if !self.is_collapsed {
            if self.labels.is_empty() {
                col = col.child(
                    View::label("No triggers on this layer")
                        .fill_w()
                        .h(Sizing::Fixed(ROW_HEIGHT))
                        .font(FONT_SIZE)
                        .text_color(color::TEXT_DIMMED_C32)
                        .align_text(TextAlign::Left)
                        .inert()
                        .key(KEY_EMPTY_LABEL),
                );
            } else {
                for i in 0..self.labels.len() {
                    let expanded = self.expanded.get(i).copied().unwrap_or(false);
                    let glyph = if expanded { "\u{25BC} " } else { "\u{25B8} " };
                    let toggle = View::button(if self.enabled[i] { "ON" } else { "OFF" })
                        .fixed(ENABLE_BTN_W, ROW_HEIGHT)
                        .style(toggle_btn_style(self.enabled[i]))
                        .inert()
                        .key(KEY_ROW_TOGGLE_BASE + i as u64);
                    let label = View::button(format!("{glyph}{}", self.labels[i]))
                        .fill_w()
                        .h(Sizing::Fixed(ROW_HEIGHT))
                        .style(Self::row_label_style(expanded))
                        .inert()
                        .key(KEY_ROW_LABEL_BASE + i as u64);
                    let remove = View::button("\u{00D7}")
                        .fixed(REMOVE_BTN_W, ROW_HEIGHT)
                        .style(de_btn_style(false, color::AUDIO_TRIM_BAR_C32))
                        .inert()
                        .key(KEY_ROW_REMOVE_BASE + i as u64);
                    let row_line = View::row(GAP)
                        .fill_w()
                        .h(Sizing::Fixed(ROW_HEIGHT))
                        .cross_align(Align::Center)
                        .child(toggle)
                        .child(label)
                        .child(remove);
                    col = col.child(row_line);

                    if expanded {
                        col = col.child(
                            View::panel()
                                .fill_w()
                                .h(Sizing::Fixed(clip_trigger_drawer_height() + DRAWER_BOTTOM_GAP))
                                .key(KEY_ROW_DRAWER_BASE + i as u64),
                        );
                    }
                }
            }
            let add_style = UIStyle {
                bg_color: Color32::new(30, 30, 33, 255),
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            };
            col = col.child(
                View::button("+ Add Trigger")
                    .fill_w()
                    .h(Sizing::Fixed(ADD_ROW_H))
                    .style(add_style)
                    .inert()
                    .key(KEY_ADD_BTN),
            );
        }
        col
    }

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        let view = self.chrome_view();
        self.host.build(tree, &view, rect);
        self.first_node = self.host.node_id(0);

        self.audio_configs = (0..self.labels.len()).map(|_| None).collect();
        if !self.is_collapsed {
            for i in 0..self.labels.len() {
                if !self.expanded.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let Some(slot_id) = self.host.node_id_for_key(KEY_ROW_DRAWER_BASE + i as u64)
                else {
                    continue;
                };
                let slot = tree.get_bounds(slot_id);
                let Some(layer_id) = self.layer_id.clone() else { continue };
                let one_shot = self.one_shot_beats.get(i).copied().unwrap_or(1.0);
                // Parented under the slot node (a real descendant of this
                // section's own chrome subtree), not `None` — so the drawer
                // shares an ancestor with its row's label/toggle for
                // selector tooling (`under_text` in ui-flow scripts) and
                // ordinary z-order/clip inheritance.
                let (dids, send_count) = build_clip_trigger_drawer(
                    tree,
                    Some(slot_id),
                    slot.x,
                    slot.y,
                    slot.width,
                    &self.mod_state,
                    i,
                    color::FONT_CAPTION,
                    &layer_id,
                    i,
                    one_shot,
                );
                self.audio_configs[i] = Some((dids, send_count));
            }
        }

        let first = self.first_node.map_or(0, |id| id.index());
        self.node_count = tree.count() - first;
    }

    pub fn owns_node(&self, node_id: NodeId) -> bool {
        let Some(first) = self.first_node else {
            return false;
        };
        let id = node_id.index();
        id >= first.index() && id < first.index() + self.node_count
    }

    // ── Click resolution ─────────────────────────────────────────────

    /// Row `i`'s current source cell as the drawer displays it — the basis
    /// for clicks that change one axis and keep the other.
    fn current_feature(&self, i: usize) -> AudioFeature {
        AudioFeature::new(
            audio_kind_from_index(self.mod_state.audio_kind_idx.get(i).copied().unwrap_or(0) as usize),
            audio_band_from_index(self.mod_state.audio_band_idx.get(i).copied().unwrap_or(0) as usize),
        )
    }

    fn current_send_id(&self, i: usize) -> Option<AudioSendId> {
        let idx = self.mod_state.audio_send_idx.get(i).copied().unwrap_or(-1);
        if idx < 0 {
            return None;
        }
        self.mod_state.audio_send_ids.get(idx as usize).cloned()
    }

    pub fn handle_click(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        if self.host.node_id_for_key(KEY_CHEVRON) == Some(node_id) {
            return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerSectionToggle)];
        }
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };

        if self.host.node_id_for_key(KEY_ADD_BTN) == Some(node_id) {
            return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerAdd(layer_id))];
        }
        for i in 0..self.labels.len() {
            if self.host.node_id_for_key(KEY_ROW_TOGGLE_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerEnabledToggle(layer_id, i))];
            }
            if self.host.node_id_for_key(KEY_ROW_LABEL_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerRowExpandToggle(layer_id, i))];
            }
            if self.host.node_id_for_key(KEY_ROW_REMOVE_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerRemove(layer_id, i))];
            }
        }

        // Drawer button clicks. Flat index order matches exactly what
        // `build_clip_trigger_drawer` builds: send buttons, then the chips
        // `trigger_source_chips` returns for the row's current cell (five,
        // or six with a truthful fallback chip), then Length.
        for (i, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, send_count)) = cfg else { continue };
            let Some(flat) = dids.resolve_button(node_id) else { continue };
            let feature = self.current_feature(i);
            if flat < *send_count {
                // Source click: keep the row's feature, point it at this send.
                let Some(send_id) = self.mod_state.audio_send_ids.get(flat).cloned() else {
                    return vec![];
                };
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerSetSource(layer_id, i, send_id, feature))];
            }
            let f = flat - send_count;
            let chips = trigger_source_chips(feature);
            if f < chips.len() {
                // Chip click: keep the row's send, listen to the chip's cell.
                let Some(send_id) = self.current_send_id(i) else { return vec![] };
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerSetSource(
                    layer_id,
                    i,
                    send_id,
                    chips[f].feature,
                ))];
            }
            let f = f - chips.len();
            if f < LENGTH_OPTIONS.len() {
                return vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerSetLength(layer_id, i, LENGTH_OPTIONS[f]))];
            }
        }
        Vec::new()
    }

    // ── Sensitivity slider drag ──────────────────────────────────────
    // The drawer's only slider (`DrawerIds.sliders[0]`). Mirrors
    // `ParamCardPanel`'s audio-shaping press/drag/release, addressed by row
    // index and emitting `AudioTrigger*` instead of `AudioMod*`.

    pub fn handle_press(&mut self, node_id: NodeId, pos_x: f32) -> Vec<PanelAction> {
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        for (i, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = cfg else { continue };
            if let Some(sl) = dids.sliders.first()
                && node_id == sl.track
            {
                let norm = BitmapSlider::x_to_normalized(sl.track_span, pos_x).clamp(0.0, 1.0);
                let value = norm * AUDIO_SENS_MAX;
                self.dragging_shape
                    .start((i, AudioShapeParam::Sensitivity), Vec2::new(pos_x, 0.0));
                return vec![
                    PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeSnapshot(layer_id.clone(), i)),
                    PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeParamChanged(
                        layer_id,
                        i,
                        AudioShapeParam::Sensitivity,
                        value,
                    )),
                ];
            }
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos_x: f32, tree: &mut UITree) -> Vec<PanelAction> {
        let Some(&(i, _)) = self.dragging_shape.payload() else { return Vec::new() };
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        let rect = self
            .audio_configs
            .get(i)
            .and_then(|c| c.as_ref())
            .and_then(|(d, _)| d.sliders.first())
            .map(|sl| sl.track_span);
        let Some(rect) = rect else { return Vec::new() };
        let norm = BitmapSlider::x_to_normalized(rect, pos_x).clamp(0.0, 1.0);
        let value = norm * AUDIO_SENS_MAX;
        if let Some(v) = self.mod_state.audio_sensitivity.get_mut(i) {
            *v = value;
        }
        let text = format!("{value:.2}");
        if let Some((d, _)) = self.audio_configs.get(i).and_then(|c| c.as_ref())
            && let Some(sl) = d.sliders.first()
        {
            BitmapSlider::update_value(tree, sl, norm, &text);
        }
        vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeParamChanged(
            layer_id,
            i,
            AudioShapeParam::Sensitivity,
            value,
        ))]
    }

    pub fn handle_release(&mut self) -> Vec<PanelAction> {
        let Some((i, _)) = self.dragging_shape.release() else { return Vec::new() };
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        vec![PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeCommit(layer_id, i))]
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging_shape.is_active()
    }

    /// D6 fire meter: push this tick's live shaped-signal level onto
    /// every open row's Sensitivity meter — in place, no rebuild. Keyed on
    /// `(layer_id, row index)` via `manifold_foundation::
    /// fire_meter_key_for_clip_trigger` — the SAME constructor the
    /// content-thread capture uses.
    pub fn update_fire_meters(
        &self,
        tree: &mut UITree,
        fire_level: &dyn Fn(u64) -> Option<f32>,
        dt: f32,
    ) {
        let Some(layer_id) = self.layer_id.as_ref() else { return };
        for (i, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = cfg else { continue };
            let Some(Some(meter)) = dids.meters.first() else { continue };
            let key = manifold_foundation::fire_meter_key_for_clip_trigger(
                layer_id.as_str(),
                i as u64,
            );
            let level = fire_level(key).unwrap_or(0.0);
            meter.update(tree, level, AUDIO_MOD_ACTIVE_C32, dt);
        }
    }

    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 5):
    /// row index of the currently-OPEN clip-trigger drawer, if any.
    /// `audio_configs[i].is_some()` already means "this row is expanded AND
    /// the section isn't collapsed" (see `configure_and_build`'s gate).
    /// First match wins.
    fn open_fire_mode_drawer_row(&self) -> Option<usize> {
        self.audio_configs.iter().position(Option::is_some)
    }

    /// The send the currently-open clip-trigger drawer is reading, if any.
    pub fn open_fire_mode_drawer_send(&self) -> Option<manifold_foundation::AudioSendId> {
        let i = self.open_fire_mode_drawer_row()?;
        let idx = self.mod_state.audio_send_idx.get(i).copied().unwrap_or(-1);
        if idx < 0 {
            return None;
        }
        self.mod_state.audio_send_ids.get(idx as usize).cloned()
    }

    /// The band the currently-open clip-trigger drawer is reading, if any.
    pub fn open_fire_mode_drawer_band(&self) -> Option<crate::types::AudioBand> {
        let i = self.open_fire_mode_drawer_row()?;
        let idx = self.mod_state.audio_band_idx.get(i).copied().unwrap_or(0);
        crate::types::AudioBand::ALL.get(idx as usize).copied()
    }

    /// Right-click reset on the Sensitivity slider — the drawer's own
    /// `slider_resets`, registered exactly as `ParamCardPanel` does. Walks
    /// the contract via [`BitmapSlider::register_track_reset`]
    /// (UI_WIDGET_UNIFICATION_DESIGN.md P1).
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        for cfg in self.audio_configs.iter().flatten() {
            let (dids, _) = cfg;
            for (sl, reset) in dids.sliders.iter().zip(dids.slider_resets.iter()) {
                BitmapSlider::register_track_reset(sl, reset, intents);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AudioFeatureKind;

    // P7 (`docs/UI_WIDGET_UNIFICATION_DESIGN.md`) behavior-pinning test for the
    // `dragging_shape` grab/release lifecycle, written BEFORE migrating the
    // field from `Option<(usize, AudioShapeParam)>` onto `DragController<T>`.
    // Drives only the public `is_dragging`/`handle_release` surface (plus the
    // module-private field directly, legitimate white-box access in the same
    // module) so it stays valid regardless of the field's internal shape.

    #[test]
    fn not_dragging_by_default() {
        let section = AudioTriggerSection::new();
        assert!(!section.is_dragging());
    }

    #[test]
    fn grab_marks_dragging_and_release_commits_once() {
        let mut section = AudioTriggerSection::new();
        section.layer_id = Some(LayerId::new("layer-1"));

        // Simulate `handle_press` having grabbed row 2's Sensitivity slider.
        section
            .dragging_shape
            .start((2, AudioShapeParam::Sensitivity), Vec2::ZERO);
        assert!(section.is_dragging());

        let actions = section.handle_release();
        assert!(!section.is_dragging(), "release must clear the drag");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AudioSetup(AudioSetupAction::AudioTriggerShapeCommit(layer_id, row)) => {
                assert_eq!(layer_id, &LayerId::new("layer-1"));
                assert_eq!(*row, 2);
            }
            other => panic!("expected AudioTriggerShapeCommit, got {other:?}"),
        }

        // Releasing again with nothing in flight signals nothing — take-once.
        let actions = section.handle_release();
        assert!(actions.is_empty());
        assert!(!section.is_dragging());
    }

    #[test]
    fn release_without_layer_id_emits_nothing_but_still_clears() {
        let mut section = AudioTriggerSection::new();
        section
            .dragging_shape
            .start((0, AudioShapeParam::Sensitivity), Vec2::ZERO);

        let actions = section.handle_release();
        assert!(actions.is_empty());
        assert!(!section.is_dragging());
    }

    #[test]
    fn handle_drag_is_noop_when_not_dragging() {
        let mut tree = UITree::new();
        let mut section = AudioTriggerSection::new();
        assert!(section.handle_drag(0.5, &mut tree).is_empty());
    }

    #[test]
    fn curated_chips_cover_the_onset_cells_and_fall_back_truthfully() {
        use crate::types::AudioBand;
        use super::super::param_slider_shared::TRIGGER_SOURCE_CHIPS;
        // Every curated cell highlights exactly its own chip.
        for &(label, kind, band) in TRIGGER_SOURCE_CHIPS.iter() {
            let chips = trigger_source_chips(AudioFeature::new(kind, band));
            assert_eq!(chips.len(), 5, "curated cell needs no fallback chip");
            let active: Vec<_> = chips.iter().filter(|c| c.active).collect();
            assert_eq!(active.len(), 1, "{label} must highlight exactly one chip");
            assert_eq!(active[0].label, label);
        }
        // The Kick detector ignores band — a Kick cell on any band still
        // highlights the Kick chip (never a spurious fallback).
        let chips = trigger_source_chips(AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Full));
        assert_eq!(chips.len(), 5);
        assert!(chips.iter().any(|c| c.active && c.label == "Kick"));
        // A non-curated cell (an older project's Flux×Mid) keeps a truthful
        // trailing chip naming what it actually listens to.
        let chips = trigger_source_chips(AudioFeature::new(AudioFeatureKind::Flux, AudioBand::Mid));
        assert_eq!(chips.len(), 6);
        assert_eq!(chips[5].label, "Flux\u{00B7}Mid");
        assert!(chips[5].active);
        assert_eq!(chips.iter().filter(|c| c.active).count(), 1);
    }

    #[test]
    fn expand_row_grows_to_fit_then_survives_resync() {
        let mut section = AudioTriggerSection::new();
        // Add dispatches before the structural resync: the row doesn't exist
        // in `expanded` yet, so expand_row must grow to fit.
        section.expand_row(1);
        assert_eq!(section.expanded, vec![false, true]);
        // configure()'s truncate+resize preserves the prefix — the new row
        // stays expanded.
        section.expanded.truncate(2);
        section.expanded.resize(2, false);
        assert_eq!(section.expanded, vec![false, true]);
    }
}
