//! `RowHost` — the shared per-row id-bundle machinery + row-index + row-action
//! routing lifted out of `ParamCardPanel` (P-S2, UI funnel decomposition).
//!
//! A parameter card (the effect/generator inspector card, and — after P-S3 —
//! the scene properties card) renders a column of parameter rows and must map
//! a clicked/pressed `NodeId` back to the row + role that owns it. That map,
//! the per-row node-id bundles it's built from, and the routing that turns a
//! resolved `(row, role)` into a `PanelAction` are identical across every
//! card kind. `RowHost` is the single home for that machinery so a scene card
//! can BE one instead of hand-copying it (the `SceneCardState` twin P-S3
//! deletes).
//!
//! `RowHost` owns ONLY the id/routing machinery. The per-row *data* a card
//! renders and edits — the `ParamRow`s, the `ParamModState`, the value /
//! osc / tab caches — stays on the owning panel and is passed to the routing
//! methods by reference. That keeps the seam tight: `RowHost` is the widget-id
//! bookkeeping and the click→action logic; the panel keeps the model and the
//! animation/drag runtime.

use super::*;
use crate::panels::copy_to_clipboard_label::CopyToClipboardLabelState;
use crate::param_surface::{RowIndex, RowRole, RowSpec};
use crate::{MappingAction, ModulationAction, ParamsAction};

/// Release-mode once-per-id loud signal for the id-join miss invariant (INV-6):
/// a built row whose id has NO live manifest entry this frame. `debug_assert!`
/// already panics in dev; this keeps the release path from freezing a row
/// silently. Reachable only if a manifest mutation skipped the structural
/// reconfigure that rebuilds the rows — an upstream bug, surfaced here rather
/// than swallowed.
pub(crate) fn warn_join_gap_once(id: &str) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(id.to_string()) {
        eprintln!(
            "param value sync: built row id {id:?} has no live manifest entry — a manifest \
             mutation skipped its structural reconfigure; row frozen this frame (INV-6)"
        );
    }
}

/// Per-row widget-id bundles + the reverse `WidgetId → (row, role)` index for
/// one parameter card. Every field is row-parallel (indexed by param row),
/// except `row_index` (the flat reverse map) and `section_header_ids` (rebuilt
/// each build pass). Populated by the card's row builders as each row's
/// controls land; consumed by [`RowHost::reindex_row`] /
/// [`RowHost::register_intents`] / [`RowHost::row_action`].
pub(crate) struct RowHost {
    pub(crate) slider_ids: Vec<Option<SliderNodeIds>>,
    /// Per-param right-click reset for `slider_ids[pi]`'s track — a parallel
    /// array (rather than folding into `slider_ids`) so the many existing
    /// `slider_ids[pi].track`/etc. access sites are untouched. `Some` exactly
    /// when `slider_ids[pi]` is `Some` (BUG-070 follow-through).
    pub(crate) slider_resets: Vec<Option<PanelAction>>,
    /// Per-param transparent full-row hit catcher behind the slider widgets.
    pub(crate) row_catcher_ids: Vec<Option<NodeId>>,
    pub(crate) driver_btn_ids: Vec<Option<NodeId>>,
    pub(crate) envelope_btn_ids: Vec<Option<NodeId>>,
    pub(crate) driver_config_ids: Vec<Option<DriverConfigIds>>,
    /// Per-param "A" audio-mod button node id.
    pub(crate) audio_btn_ids: Vec<Option<NodeId>>,
    /// Per-param audio drawer ids + send count (for click resolution). An
    /// `is_trigger_gate` row's "A" button + drawer live here too (§9).
    pub(crate) audio_configs: Vec<Option<(crate::panels::drawer::DrawerIds, usize)>>,
    /// Per-param collapsed-row mode-indicator label (§9, `is_trigger_gate`
    /// rows only).
    pub(crate) audio_trigger_mode_badge_ids: Vec<Option<NodeId>>,
    /// Per-param orange envelope target handle on the slider track (when armed).
    pub(crate) target_ids: Vec<Option<EnvelopeTargetIds>>,
    /// Per-param envelope drawer — the single "Decay" slider (when armed).
    pub(crate) envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    pub(crate) trim_ids: Vec<Option<TrimHandleIds>>,
    pub(crate) ableton_trim_ids: Vec<Option<TrimHandleIds>>,
    /// Per-param green audio-mod trim handles (when an audio mod is armed).
    pub(crate) audio_trim_ids: Vec<Option<TrimHandleIds>>,
    pub(crate) ableton_config_ids: Vec<Option<AbletonConfigIds>>,
    /// Per-param modulation-config tab strip node ids paired with their
    /// `ModTab`, for routing tab clicks. Empty for rows with fewer than two
    /// active configs. Rebuilt each frame.
    pub(crate) mod_tab_ids: Vec<Vec<(NodeId, ModTab)>>,
    pub(crate) toggle_ids: Vec<Option<ToggleParamIds>>,
    /// Per-param sideways-mapping-drawer chevron (Author context, mappable rows
    /// only). `None` for rows without one.
    pub(crate) mapping_chevron_ids: Vec<Option<NodeId>>,
    /// Rebuilt every build pass: `(header_node_id, section_name)` for every
    /// section-header row drawn this frame, so a header click resolves back to
    /// its section without a second id → name map.
    pub(crate) section_header_ids: Vec<(NodeId, String)>,
    /// WidgetId → (row, role) reverse map, rebuilt every `build()` from the
    /// same rows being rendered (D5, `docs/WIDGET_TREE_DESIGN.md` P2) — the
    /// ONLY sanctioned way `handle_click`/`handle_pointer_down`/`handle_drag`
    /// identify a row element. Cleared at the top of `build()`, repopulated by
    /// `reindex_row` as each row's controls land.
    pub(crate) row_index: RowIndex,
}

impl RowHost {
    pub(crate) fn new() -> Self {
        Self {
            slider_ids: Vec::new(),
            slider_resets: Vec::new(),
            row_catcher_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            audio_btn_ids: Vec::new(),
            audio_configs: Vec::new(),
            audio_trigger_mode_badge_ids: Vec::new(),
            target_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            ableton_trim_ids: Vec::new(),
            audio_trim_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            mod_tab_ids: Vec::new(),
            toggle_ids: Vec::new(),
            mapping_chevron_ids: Vec::new(),
            section_header_ids: Vec::new(),
            row_index: RowIndex::default(),
        }
    }

    /// Populate `self.row_index` for row `i` from the per-row node-id fields
    /// that were just built — the SAME fields the row renders (D5: routing
    /// agrees with rendering by construction). Called once per row from both
    /// the toggle/trigger and slider row builders, right after their fields
    /// land. Bundles register EVERY interactive node they own under one role
    /// (the widget-contract split — `row_action`'s bundle `resolve` methods
    /// name the sub-element).
    pub(crate) fn reindex_row(&mut self, tree: &UITree, i: usize) {
        if let Some(s) = &self.slider_ids[i] {
            self.row_index.insert(tree.widget_of(s.track), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(s.value_text), i, RowRole::Slider);
            if let Some(l) = s.label {
                self.row_index.insert(tree.widget_of(l), i, RowRole::Slider);
            }
        }
        // Trim/target overlay handles nest under the slider track (they
        // inherit its stability through the parent chain — D4) and belong to
        // the slider bundle functionally; indexed under the same role so
        // `handle_pointer_down` resolves them by row instead of scanning.
        if let Some(t) = &self.trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.ableton_trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.audio_trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.target_ids[i] {
            self.row_index.insert(tree.widget_of(t.target_bar_id), i, RowRole::Slider);
        }
        if let Some(rc) = self.row_catcher_ids[i] {
            self.row_index.insert(tree.widget_of(rc), i, RowRole::RowCatcher);
        }
        if let Some(b) = self.driver_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::DriverBtn);
        }
        if let Some(b) = self.envelope_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::EnvelopeBtn);
        }
        if let Some(b) = self.audio_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::AudioBtn);
        }
        if let Some(t) = &self.toggle_ids[i] {
            self.row_index.insert(tree.widget_of(t.button_id), i, RowRole::ToggleBtn);
            if let Some(l) = t.label_id {
                self.row_index.insert(tree.widget_of(l), i, RowRole::Label);
            }
        }
        if let Some(c) = &self.driver_config_ids[i] {
            for &b in c.beat_div_btn_ids.iter() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::DriverConfig);
            }
            for b in [c.straight_btn_id, c.dotted_btn_id, c.triplet_btn_id, c.free_btn_id, c.invert_btn_id] {
                self.row_index.insert(tree.widget_of(b), i, RowRole::DriverConfig);
            }
            for &b in c.wave_btn_ids.iter() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::DriverConfig);
            }
        }
        if let Some(c) = &self.envelope_config_ids[i] {
            self.row_index.insert(tree.widget_of(c.decay_slider.track), i, RowRole::EnvelopeConfig);
            self.row_index.insert(tree.widget_of(c.decay_slider.value_text), i, RowRole::EnvelopeConfig);
            if let Some(l) = c.decay_slider.label {
                self.row_index.insert(tree.widget_of(l), i, RowRole::EnvelopeConfig);
            }
        }
        if let Some(c) = &self.ableton_config_ids[i] {
            self.row_index.insert(tree.widget_of(c.invert_btn_id), i, RowRole::AbletonConfig);
        }
        if let Some((dids, _)) = &self.audio_configs[i] {
            for &b in dids.button_ids() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::AudioConfig);
            }
            for s in &dids.sliders {
                self.row_index.insert(tree.widget_of(s.track), i, RowRole::AudioConfig);
                self.row_index.insert(tree.widget_of(s.value_text), i, RowRole::AudioConfig);
                if let Some(l) = s.label {
                    self.row_index.insert(tree.widget_of(l), i, RowRole::AudioConfig);
                }
            }
        }
        if let Some(c) = self.mapping_chevron_ids[i] {
            self.row_index.insert(tree.widget_of(c), i, RowRole::MappingChevron);
        }
        for &(node, _tab) in &self.mod_tab_ids[i] {
            self.row_index.insert(tree.widget_of(node), i, RowRole::ModTab);
        }
    }

    /// §5.6 shared per-row value push: normalize → format → update row `i`'s
    /// slider fill + readout. The ONE place both parameter cards write a
    /// slider value (`ParamCardPanel::sync_param_value` and
    /// `ScenePanel::sync_properties_values`), so the fill/readout math can't
    /// drift apart. `display_norm_override` draws the FILL at an in-flight
    /// snapback position while the readout still shows the true `value`; `None`
    /// draws `value`'s own normalized position. No-op for a row with no slider
    /// bundle (a toggle/trigger row, or an out-of-range index).
    pub(crate) fn push_slider_value(
        &self,
        tree: &mut UITree,
        i: usize,
        value: f32,
        spec: &RowSpec,
        display_norm_override: Option<f32>,
    ) {
        let Some(ids) = self.slider_ids.get(i).and_then(|s| s.as_ref()) else {
            return;
        };
        let norm = crate::slider::BitmapSlider::value_to_normalized(value, spec.min, spec.max);
        let text = format_param_value(
            value,
            spec.min,
            spec.whole_numbers,
            spec.is_angle,
            spec.value_labels.as_deref(),
        );
        crate::slider::BitmapSlider::update_value(tree, ids, display_norm_override.unwrap_or(norm), &text);
    }

    /// Replay every materialised slider's `Track + RightClick → reset` intent —
    /// main rows AND every drawer slider (audio-shape Amount/Attack/Release,
    /// envelope Decay). This is the ROW half of the card's intent
    /// registration; the owning panel's `register_intents` calls it and then
    /// adds the card-chrome intents (border claim, relight resets, per-param
    /// mapping menus) it keeps. Mirrors `SceneCardState::register_intents`.
    pub(crate) fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::slider::BitmapSlider;
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let (Some(ids), Some(reset)) =
                (slider, self.slider_resets.get(pi).and_then(|r| r.as_ref()))
            {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }
        for cfg in self.envelope_config_ids.iter().flatten() {
            BitmapSlider::register_track_reset(&cfg.decay_slider, &cfg.decay_reset, intents);
        }
        for cfg in self.audio_configs.iter().flatten() {
            let (dids, _) = cfg;
            for (sl, reset) in dids.sliders.iter().zip(dids.slider_resets.iter()) {
                BitmapSlider::register_track_reset(sl, reset, intents);
            }
        }
    }

    // ── Row-action routing ────────────────────────────────────────
    //
    // `row_action` and its helpers turn a resolved `(row, role)` hit into a
    // `PanelAction`. `RowHost` owns the id bundles the routing reads
    // (`slider_ids`/`driver_config_ids`/`audio_configs`/…); the per-row *model*
    // it needs — the `ParamRow`s, the `ParamModState`, the value/osc caches and
    // the drawer-tab / section-fold UI state — belongs to the owning panel and
    // is passed in by reference. That keeps `RowHost` model-free while a single
    // routing body serves every card kind (the `SceneCardState` twin P-S3
    // folds in).

    /// Point param `pi`'s config drawer at `tab` — used when arming a modulator
    /// so its config comes forward. No-op if `pi` is out of range. Writes the
    /// panel-owned `mod_active_tab` (RowHost holds ids/routing, not the tab
    /// choice the render pass reads).
    fn focus_mod_tab(&self, mod_active_tab: &mut [ModTab], pi: usize, tab: ModTab) {
        if let Some(slot) = mod_active_tab.get_mut(pi) {
            *slot = tab;
        }
    }

    /// BUG-250: map a `RowRole::Slider` value-cell hit (an enum row) to the
    /// shared cycle-or-dropdown action set (`enum_value_cell_actions`). The
    /// cell node id comes from the row's own slider ids (the dropdown anchors
    /// under it); the current value is the synced base value, matching what
    /// the cell displays.
    fn enum_value_cell_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        clicked: NodeId,
        rows: &[ParamRow],
        base_values: &[f32],
    ) -> Vec<PanelAction> {
        let info = &rows[pi];
        let labels = info.spec.value_labels.clone().unwrap_or_default();
        let cell = self
            .slider_ids
            .get(pi)
            .and_then(|s| s.as_ref())
            .map(|s| s.value_text)
            .unwrap_or(clicked);
        let value = base_values.get(pi).copied().unwrap_or(info.spec.default);
        enum_value_cell_actions(target, rows[pi].id.clone(), &labels, value, info.spec.min, cell)
    }

    /// The "A" audio-mod button action — always opens (arms) or closes
    /// (disarms) this param's audio drawer, never the Audio Setup modal. With
    /// no sends defined yet, arming auto-creates the project's first send and
    /// points this param at it in one undo step, so the drawer opens populated
    /// and ready (the user routes/renames sends in Audio Setup afterward). The
    /// drawer's own "+" adds further sends.
    fn audio_toggle_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        rows: &[ParamRow],
        mod_state: &ParamModState,
    ) -> Vec<PanelAction> {
        let ms = mod_state;
        if ms.audio_active.get(pi).copied().unwrap_or(false) {
            // Already armed → disarm (closes the drawer), regardless of sends.
            vec![PanelAction::Modulation(ModulationAction::AudioModToggle(target, rows[pi].id.clone()))]
        } else if ms.audio_send_ids.is_empty() {
            // Not armed, no send to point at → open Audio Setup so the user can
            // create one. Sends are defined there, never from the drawer.
            vec![PanelAction::Root(RootAction::OpenAudioSetup)]
        } else {
            // Not armed, sends exist → arm at the project's first send.
            vec![PanelAction::Modulation(ModulationAction::AudioModToggle(target, rows[pi].id.clone()))]
        }
    }

    /// Build an `AudioModSetSource` from the param's current selections, with
    /// one axis optionally overridden (the clicked send / feature-kind / band).
    /// Empty when no send resolves (nothing to point at).
    fn audio_set_source_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        send_override: Option<usize>,
        kind_override: Option<usize>,
        band_override: Option<usize>,
        rows: &[ParamRow],
        mod_state: &ParamModState,
    ) -> Vec<PanelAction> {
        let ms = mod_state;
        let send_k = send_override
            .map(|k| k as i32)
            .unwrap_or_else(|| ms.audio_send_idx.get(pi).copied().unwrap_or(-1));
        let Some(send_id) = (send_k >= 0)
            .then(|| ms.audio_send_ids.get(send_k as usize).cloned())
            .flatten()
        else {
            return vec![];
        };
        let kind_idx =
            kind_override.unwrap_or_else(|| ms.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize);
        let band_idx =
            band_override.unwrap_or_else(|| ms.audio_band_idx.get(pi).copied().unwrap_or(0) as usize);
        let feature = crate::types::AudioFeature::new(
            audio_kind_from_index(kind_idx),
            audio_band_from_index(band_idx),
        );
        vec![PanelAction::Modulation(ModulationAction::AudioModSetSource(target, rows[pi].id.clone(), send_id, feature))]
    }

    /// A click on a Listen-row chip — resolves the chip's `AudioFeature` to
    /// (kind, band) indices and reuses the same set-source action a matrix
    /// click would issue, one command carrying both axes.
    fn audio_select_chip_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        chip: usize,
        rows: &[ParamRow],
        mod_state: &ParamModState,
    ) -> Vec<PanelAction> {
        let ms = mod_state;
        let current = crate::types::AudioFeature::new(
            audio_kind_from_index(ms.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize),
            audio_band_from_index(ms.audio_band_idx.get(pi).copied().unwrap_or(0) as usize),
        );
        let chips = trigger_source_chips(current);
        let Some(chip) = chips.get(chip) else {
            return vec![];
        };
        let kind_idx = crate::types::AudioFeatureKind::ALL
            .iter()
            .position(|&k| k == chip.feature.kind)
            .unwrap_or(0);
        let band_idx = crate::types::AudioBand::ALL
            .iter()
            .position(|&b| b == chip.feature.band)
            .unwrap_or(0);
        self.audio_set_source_action(target, pi, None, Some(kind_idx), Some(band_idx), rows, mod_state)
    }

    /// A click on an `is_trigger_gate` row's Mode row (§9 U3) — converts the
    /// clicked button index to a `TriggerFireMode` at this dispatch boundary
    /// and issues one `AudioModSetTriggerMode`, the same command family every
    /// other audio-mod drawer edit uses.
    fn audio_set_trigger_mode_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        mode_idx: usize,
        rows: &[ParamRow],
    ) -> Vec<PanelAction> {
        vec![PanelAction::Modulation(ModulationAction::AudioModSetTriggerMode(target, rows[pi].id.clone(), mode_idx))]
    }

    /// Route a resolved `(row, role)` hit to the `PanelAction` the old per-kind
    /// gauntlets emitted for that element — ONE match, both card kinds (D5).
    /// Identity comes off `rows[row].id`; the wire target is the caller's
    /// `param_target()`, passed in. Bundle roles (`DriverConfig`/`AbletonConfig`/
    /// `AudioConfig`) delegate to the bundle's own `resolve` (the
    /// widget-contract split: the bundle knows its own nodes, this function only
    /// knows which row it belongs to). Reads only `RowHost`'s id bundles; every
    /// mutation lands on the panel-owned model passed by `&mut`.
    ///
    /// The wide parameter list threads in the card model (the `rows`,
    /// `mod_state`, and the value/osc/tab/fold state) that stays on the panel
    /// this phase; it collapses when P-S3 folds `rows`/`mod_state` into
    /// `RowHost` (the `SceneCardState` unification). Well under `clippy.toml`'s
    /// `too-many-arguments-threshold` (20).
    pub(crate) fn row_action(
        &self,
        target: GraphParamTarget,
        row: usize,
        role: RowRole,
        node: NodeId,
        rows: &[ParamRow],
        base_values: &[f32],
        osc_addresses: &[Option<String>],
        mod_state: &mut ParamModState,
        mod_active_tab: &mut [ModTab],
        copied_flash: &mut CopyToClipboardLabelState,
        section_folded: &mut ahash::AHashMap<String, bool>,
    ) -> Vec<PanelAction> {
        match role {
            RowRole::Slider => {
                let Some(ids) = self.slider_ids[row] else {
                    return Vec::new();
                };
                if ids.label == Some(node) {
                    if osc_addresses.get(row).and_then(|a| a.as_ref()).is_none() {
                        return Vec::new();
                    }
                    if let Some(label) = ids.label {
                        copied_flash.trigger(label);
                    }
                    let addr = osc_addresses[row].clone().unwrap_or_default();
                    return vec![PanelAction::Root(RootAction::CopyOscAddress(addr))];
                }
                if ids.value_text == node && rows[row].spec.value_labels.is_some() {
                    return self.enum_value_cell_action(target, row, node, rows, base_values);
                }
                // The track itself (drag start) and a plain numeric value cell
                // (double-click type-in, a different dispatch path) emit no
                // click action — matches the old gauntlet's fall-through.
                Vec::new()
            }
            RowRole::RowCatcher => Vec::new(),
            RowRole::Label => {
                // Toggle/trigger row label → copy OSC address (mirrors the
                // slider label path; toggle rows carry no slider bundle).
                if let Some(addr) = osc_addresses.get(row).and_then(|a| a.clone()) {
                    copied_flash.trigger(node);
                    return vec![PanelAction::Root(RootAction::CopyOscAddress(addr))];
                }
                Vec::new()
            }
            RowRole::DriverBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Driver);
                vec![PanelAction::Modulation(ModulationAction::DriverToggle(target, rows[row].id.clone()))]
            }
            RowRole::EnvelopeBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Envelope);
                vec![PanelAction::Modulation(ModulationAction::EnvelopeToggle(target, rows[row].id.clone()))]
            }
            RowRole::AudioBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Audio);
                self.audio_toggle_action(target, row, rows, mod_state)
            }
            RowRole::ToggleBtn => {
                let is_trigger = rows.get(row).map(|i| i.spec.is_trigger).unwrap_or(false);
                let pid = rows[row].id.clone();
                if is_trigger {
                    vec![PanelAction::Params(ParamsAction::ParamFire(target, pid))]
                } else {
                    vec![PanelAction::Params(ParamsAction::ParamToggle(target, pid))]
                }
            }
            RowRole::DriverConfig => {
                let Some(cfg) = &self.driver_config_ids[row] else {
                    return Vec::new();
                };
                match cfg.resolve(node) {
                    Some(action) => vec![PanelAction::Modulation(ModulationAction::DriverConfig(target, rows[row].id.clone(), action))],
                    None => Vec::new(),
                }
            }
            // The Decay slider's own click (drag start / value-cell type-in)
            // carries no left-click action — matches the old gauntlet, which
            // never checked envelope-config nodes in `handle_click`.
            RowRole::EnvelopeConfig => Vec::new(),
            RowRole::AudioConfig => {
                let Some((dids, send_count)) = self.audio_configs[row].as_ref() else {
                    return Vec::new();
                };
                let Some(click) =
                    resolve_audio_config_click(dids, *send_count, mod_state, &rows[row], row, node)
                else {
                    return Vec::new();
                };
                match click {
                    AudioConfigClick::SelectSend(k) => {
                        self.audio_set_source_action(target, row, Some(k), None, None, rows, mod_state)
                    }
                    AudioConfigClick::SelectChip(c) => self.audio_select_chip_action(target, row, c, rows, mod_state),
                    AudioConfigClick::ToggleMatrix => {
                        if let Some(open) = mod_state.audio_matrix_open.get_mut(row) {
                            *open = !*open;
                        }
                        Vec::new()
                    }
                    AudioConfigClick::SelectKind(k) => {
                        self.audio_set_source_action(target, row, None, Some(k), None, rows, mod_state)
                    }
                    AudioConfigClick::SelectBand(b) => {
                        self.audio_set_source_action(target, row, None, None, Some(b), rows, mod_state)
                    }
                    AudioConfigClick::ToggleInvert => {
                        vec![PanelAction::Modulation(ModulationAction::AudioModSetInvert(target, rows[row].id.clone()))]
                    }
                    AudioConfigClick::SelectTriggerMode(m) => self.audio_set_trigger_mode_action(target, row, m, rows),
                    AudioConfigClick::SelectAction(k) => {
                        vec![PanelAction::Modulation(ModulationAction::AudioModSetActionKind(target, rows[row].id.clone(), k))]
                    }
                    AudioConfigClick::SelectWrap(w) => {
                        vec![PanelAction::Modulation(ModulationAction::AudioModSetWrap(target, rows[row].id.clone(), w))]
                    }
                }
            }
            RowRole::AbletonConfig => {
                let Some(cfg) = &self.ableton_config_ids[row] else {
                    return Vec::new();
                };
                if cfg.resolve(node) {
                    vec![PanelAction::Mapping(MappingAction::AbletonInvertToggle(target, rows[row].id.clone()))]
                } else {
                    Vec::new()
                }
            }
            RowRole::ModTab => {
                let Some(&(_, tab)) = self.mod_tab_ids[row].iter().find(|(n, _)| *n == node) else {
                    return Vec::new();
                };
                if let Some(slot) = mod_active_tab.get_mut(row) {
                    *slot = tab;
                }
                vec![PanelAction::Params(ParamsAction::ModConfigTabChanged)]
            }
            RowRole::MappingChevron => vec![PanelAction::Root(RootAction::OpenCardMapping(rows[row].id.clone()))],
            RowRole::SectionHeader => {
                let Some(name) = self
                    .section_header_ids
                    .iter()
                    .find(|(hid, _)| *hid == node)
                    .map(|(_, n)| n.clone())
                else {
                    return Vec::new();
                };
                let folded = section_folded.entry(name).or_insert(false);
                *folded = !*folded;
                vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]
            }
            RowRole::RelightToggle | RowRole::RelightHeightBtn | RowRole::RelightSlider => {
                // Never inserted into `row_index` (relight has no `rows` slot) —
                // the top-of-`handle_click` checks own these. Kept here only for
                // match exhaustiveness.
                Vec::new()
            }
        }
    }
}
