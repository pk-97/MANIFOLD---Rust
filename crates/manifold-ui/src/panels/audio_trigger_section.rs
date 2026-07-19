//! AUDIO TRIGGERS — the inspector's layer-owned clip-trigger authoring
//! section (P3b, `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`).
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
//! Each row is one `Layer.clip_triggers` entry: an ON/OFF toggle (D4 — a
//! `LayerClipTrigger` starts disabled; "the user enables a row once they've
//! tuned it"), a label that expands/collapses the row's drawer, and a
//! remove button. The expanded drawer is the SAME `build_audio_mod_drawer`
//! every param/gate card uses (D5, "one drawer builder, three callers"),
//! parameterized via `AudioModDrawerTarget::ClipTrigger` so its reset
//! gestures emit the additive `PanelAction::AudioTrigger*` family instead of
//! `AudioMod*` (a `LayerClipTrigger` has no `GraphParamTarget`/`ParamId`).
//!
//! This module owns its OWN click/drag dispatch (Source/Feature/Band/Invert/
//! Length button clicks, Sensitivity/Attack/Release slider drags) — the
//! same division of labor `ParamCardPanel` already has with the shared
//! drawer builder (drawer = shared visuals + 3 reset actions; click/drag
//! resolution = caller-owned). It's a second CALLER of the shared builder,
//! not a fork of it.

use super::drawer::DrawerIds;
use super::param_card::ParamInfo;
use super::param_slider_shared::{
    AUDIO_ATTACK_MAX_MS, AUDIO_BAND_COUNT, AUDIO_KIND_COUNT, AUDIO_MOD_ACTIVE_C32,
    AUDIO_RELEASE_MAX_MS, AUDIO_SENS_MAX, AudioCardState, AudioModDrawerTarget, DRAWER_BOTTOM_GAP,
    FONT_SIZE, LENGTH_OPTIONS, ParamModState, ROW_HEIGHT, audio_band_from_index,
    audio_config_height, audio_kind_from_index, build_audio_mod_drawer, de_btn_style,
    toggle_btn_style,
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
/// sole source; this panel never reads `Project`). Mirrors the shape
/// `configure_layer_effects`/`ParamCardConfig` uses for the analogous
/// effect-card list, scaled down to what a `LayerClipTrigger` actually
/// carries (no envelopes/drivers/Ableton — those don't apply here).
#[derive(Debug, Clone, Default)]
pub struct AudioTriggerRowConfig {
    pub enabled: bool,
    /// Precomputed "{band} → {feature kind}" row label (state_sync's job —
    /// this panel never reaches into `manifold-core` for `.label()`).
    pub label: String,
    pub kind_idx: i32,
    pub band_idx: i32,
    pub invert: bool,
    pub rate_of_change: bool,
    pub sensitivity: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
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
    param_info: Vec<ParamInfo>,
    mod_state: ParamModState,
    /// Per-row: is this row's drawer open. UI-local (mirrors `is_collapsed`);
    /// resized on every `configure()`, preserving the existing prefix so a
    /// row that's still at the same index keeps its expand state across a
    /// value-only resync.
    expanded: Vec<bool>,
    audio_configs: Vec<Option<(DrawerIds, usize)>>,
    first_node: Option<NodeId>,
    node_count: usize,
    /// `(row_index, which)` of the shaping slider currently being dragged,
    /// mirroring `ParamCardPanel::DragState::dragging_audio_shape`. Lifecycle
    /// on `DragController` (P7, `docs/UI_WIDGET_UNIFICATION_DESIGN.md`) — the
    /// grab position isn't read back (each `handle_drag` call gets a fresh
    /// `pos_x`), only the active/payload/release shape is used.
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
            param_info: Vec::new(),
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
        self.expanded.resize(n, false);

        self.param_info = (0..n)
            .map(|i| ParamInfo {
                param_id: manifold_foundation::ParamId::from(format!("clip_trigger_{i}")),
                name: self.labels[i].clone(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                whole_numbers: false,
                is_angle: false,
                exposed: true,
                is_toggle: false,
                // Always `true` (D4): suppresses the drawer's Action/Wrap/
                // Mode rows via the same `show_action`/`show_mode` logic
                // `build_audio_mod_drawer` already derives from `info`.
                is_trigger: true,
                is_trigger_gate: false,
                value_labels: None,
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: false,
                section: None,
            })
            .collect();

        let audio = AudioCardState {
            active: config.rows.iter().map(|r| r.enabled).collect(),
            send_id: config.rows.iter().map(|r| r.send_id.clone()).collect(),
            kind_idx: config.rows.iter().map(|r| r.kind_idx).collect(),
            band_idx: config.rows.iter().map(|r| r.band_idx).collect(),
            range_min: vec![0.0; n],
            range_max: vec![1.0; n],
            invert: config.rows.iter().map(|r| r.invert).collect(),
            rate: config.rows.iter().map(|r| r.rate_of_change).collect(),
            sensitivity: config.rows.iter().map(|r| r.sensitivity).collect(),
            attack_ms: config.rows.iter().map(|r| r.attack_ms).collect(),
            release_ms: config.rows.iter().map(|r| r.release_ms).collect(),
            trigger_mode_idx: vec![0; n],
            action_idx: vec![0; n],
            step_amount: vec![1.0; n],
            wrap_idx: vec![0; n],
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
                h += audio_config_height(&self.param_info[i], &self.mod_state, i, true)
                    + DRAWER_BOTTOM_GAP
                    + ROW_SPACING;
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
                        let drawer_h =
                            audio_config_height(&self.param_info[i], &self.mod_state, i, true);
                        col = col.child(
                            View::panel()
                                .fill_w()
                                .h(Sizing::Fixed(drawer_h + DRAWER_BOTTOM_GAP))
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
                // ordinary z-order/clip inheritance, matching
                // `build_toggle_trigger_row`'s real-parent convention rather
                // than `macros_panel`'s `None`-parented static sub-widgets
                // (that panel's slots never need "which row is this" scoping
                // — its slot COUNT is fixed at `MACRO_COUNT`).
                let (dids, send_count) = build_audio_mod_drawer(
                    tree,
                    Some(slot_id),
                    slot.x,
                    slot.y,
                    slot.width,
                    &self.mod_state,
                    i,
                    color::FONT_CAPTION,
                    &self.param_info[i],
                    AudioModDrawerTarget::ClipTrigger(layer_id, i),
                    Some(one_shot),
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

    /// Combine the row's current send/kind/band selections with the one
    /// dimension a click changed — mirrors `ParamCardPanel::audio_set_source_action`.
    fn set_source_action(
        &self,
        layer_id: LayerId,
        i: usize,
        send_override: Option<usize>,
        kind_override: Option<usize>,
        band_override: Option<usize>,
    ) -> Vec<PanelAction> {
        let send_k = send_override
            .map(|k| k as i32)
            .unwrap_or_else(|| self.mod_state.audio_send_idx.get(i).copied().unwrap_or(-1));
        let Some(send_id) = (send_k >= 0)
            .then(|| self.mod_state.audio_send_ids.get(send_k as usize).cloned())
            .flatten()
        else {
            return vec![];
        };
        let kind_idx = kind_override
            .unwrap_or_else(|| self.mod_state.audio_kind_idx.get(i).copied().unwrap_or(0) as usize);
        let band_idx = band_override
            .unwrap_or_else(|| self.mod_state.audio_band_idx.get(i).copied().unwrap_or(0) as usize);
        let feature = AudioFeature::new(audio_kind_from_index(kind_idx), audio_band_from_index(band_idx));
        vec![PanelAction::AudioTriggerSetSource(layer_id, i, send_id, feature)]
    }

    pub fn handle_click(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        if self.host.node_id_for_key(KEY_CHEVRON) == Some(node_id) {
            return vec![PanelAction::AudioTriggerSectionToggle];
        }
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };

        if self.host.node_id_for_key(KEY_ADD_BTN) == Some(node_id) {
            return vec![PanelAction::AudioTriggerAdd(layer_id)];
        }
        for i in 0..self.labels.len() {
            if self.host.node_id_for_key(KEY_ROW_TOGGLE_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioTriggerEnabledToggle(layer_id, i)];
            }
            if self.host.node_id_for_key(KEY_ROW_LABEL_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioTriggerRowExpandToggle(layer_id, i)];
            }
            if self.host.node_id_for_key(KEY_ROW_REMOVE_BASE + i as u64) == Some(node_id) {
                return vec![PanelAction::AudioTriggerRemove(layer_id, i)];
            }
        }

        // Drawer button clicks — send/feature/band/invert/length. Flat
        // index order matches exactly what `build_audio_mod_drawer` builds
        // for a `ClipTrigger` target (is_trigger:true, is_trigger_gate:false,
        // length_beats:Some): Source, Feature, Band, [Invert], Length. Delta
        // removed from the drawer (§7.2 item 2, 2026-07-11), so the toggle
        // row now contributes exactly one flat index, not two.
        // Action/Wrap/Mode never appear (`show_action`/`show_mode` are false
        // whenever `info.is_trigger` is true), unlike the general-purpose
        // `match_param_row_click` this deliberately doesn't reuse.
        for (i, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, send_count)) = cfg else { continue };
            let Some(flat) = dids.resolve_button(node_id) else { continue };
            if flat < *send_count {
                return self.set_source_action(layer_id, i, Some(flat), None, None);
            }
            let f = flat - send_count;
            if f < AUDIO_KIND_COUNT {
                return self.set_source_action(layer_id, i, None, Some(f), None);
            }
            let f = f - AUDIO_KIND_COUNT;
            if f < AUDIO_BAND_COUNT {
                return self.set_source_action(layer_id, i, None, None, Some(f));
            }
            let f = f - AUDIO_BAND_COUNT;
            if f == 0 {
                return vec![PanelAction::AudioTriggerSetInvert(layer_id, i)];
            }
            let f = f - 1;
            if f < LENGTH_OPTIONS.len() {
                return vec![PanelAction::AudioTriggerSetLength(layer_id, i, LENGTH_OPTIONS[f])];
            }
        }
        Vec::new()
    }

    // ── Shaping-slider drag (Amount/Attack/Release) ─────────────────────
    // Mirrors `ParamCardPanel`'s "2b. Audio shaping sliders" press/drag/
    // release exactly (`param_card.rs`), addressed by row index instead of
    // `pi` and emitting `AudioTrigger*` instead of `AudioMod*`.

    pub fn handle_press(&mut self, node_id: NodeId, pos_x: f32) -> Vec<PanelAction> {
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        for (i, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = cfg else { continue };
            for (si, which) in [
                (0usize, AudioShapeParam::Sensitivity),
                (1, AudioShapeParam::Attack),
                (2, AudioShapeParam::Release),
            ] {
                if let Some(sl) = dids.sliders.get(si)
                    && node_id == sl.track
                {
                    let norm = BitmapSlider::x_to_normalized(sl.track_span, pos_x).clamp(0.0, 1.0);
                    let value = shape_value_from_norm(which, norm);
                    self.dragging_shape.start((i, which), Vec2::new(pos_x, 0.0));
                    return vec![
                        PanelAction::AudioTriggerShapeSnapshot(layer_id.clone(), i),
                        PanelAction::AudioTriggerShapeParamChanged(layer_id, i, which, value),
                    ];
                }
            }
        }
        Vec::new()
    }

    pub fn handle_drag(&mut self, pos_x: f32, tree: &mut UITree) -> Vec<PanelAction> {
        let Some(&(i, which)) = self.dragging_shape.payload() else { return Vec::new() };
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        let si = match which {
            AudioShapeParam::Sensitivity => 0,
            AudioShapeParam::Attack => 1,
            AudioShapeParam::Release => 2,
        };
        let rect = self
            .audio_configs
            .get(i)
            .and_then(|c| c.as_ref())
            .and_then(|(d, _)| d.sliders.get(si))
            .map(|sl| sl.track_span);
        let Some(rect) = rect else { return Vec::new() };
        let norm = BitmapSlider::x_to_normalized(rect, pos_x).clamp(0.0, 1.0);
        let value = shape_value_from_norm(which, norm);
        match which {
            AudioShapeParam::Sensitivity => {
                if let Some(v) = self.mod_state.audio_sensitivity.get_mut(i) {
                    *v = value;
                }
            }
            AudioShapeParam::Attack => {
                if let Some(v) = self.mod_state.audio_attack_ms.get_mut(i) {
                    *v = value;
                }
            }
            AudioShapeParam::Release => {
                if let Some(v) = self.mod_state.audio_release_ms.get_mut(i) {
                    *v = value;
                }
            }
        }
        let text = shape_value_text(which, value);
        if let Some((d, _)) = self.audio_configs.get(i).and_then(|c| c.as_ref())
            && let Some(sl) = d.sliders.get(si)
        {
            BitmapSlider::update_value(tree, sl, norm, &text);
        }
        vec![PanelAction::AudioTriggerShapeParamChanged(layer_id, i, which, value)]
    }

    pub fn handle_release(&mut self) -> Vec<PanelAction> {
        let Some((i, _)) = self.dragging_shape.release() else { return Vec::new() };
        let Some(layer_id) = self.layer_id.clone() else { return Vec::new() };
        vec![PanelAction::AudioTriggerShapeCommit(layer_id, i)]
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging_shape.is_active()
    }

    /// D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    /// P3c, BUG-082's fix): push this tick's live shaped-signal level onto
    /// every open row's Amount meter — in place, no rebuild. Every clip
    /// trigger is a fire-mode config (D6: "clip triggers alike" — no
    /// `is_trigger_gate` gate needed here, unlike `ParamCardPanel`). Keyed
    /// on `(layer_id, row index)` via `manifold_foundation::
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
    /// row index of the currently-OPEN clip-trigger drawer, if any. Every
    /// clip trigger is fire-mode by construction (D6: no `is_trigger_gate`
    /// gate needed here, unlike `ParamCardPanel`) — `audio_configs[i].is_some()`
    /// already means "this row is expanded AND the section isn't collapsed"
    /// (see `configure_and_build`'s gate). First match wins.
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

    /// Right-click resets on the shaping sliders — the drawer's own
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

fn shape_value_from_norm(which: AudioShapeParam, norm: f32) -> f32 {
    let n = norm.clamp(0.0, 1.0);
    match which {
        AudioShapeParam::Sensitivity => n * AUDIO_SENS_MAX,
        AudioShapeParam::Attack => n * AUDIO_ATTACK_MAX_MS,
        AudioShapeParam::Release => n * AUDIO_RELEASE_MAX_MS,
    }
}

fn shape_value_text(which: AudioShapeParam, value: f32) -> String {
    match which {
        AudioShapeParam::Sensitivity => format!("{value:.2}"),
        AudioShapeParam::Attack | AudioShapeParam::Release => format!("{value:.0} ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // Simulate `handle_press` having grabbed row 2's Attack slider.
        section
            .dragging_shape
            .start((2, AudioShapeParam::Attack), Vec2::ZERO);
        assert!(section.is_dragging());

        let actions = section.handle_release();
        assert!(!section.is_dragging(), "release must clear the drag");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AudioTriggerShapeCommit(layer_id, row) => {
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
}
