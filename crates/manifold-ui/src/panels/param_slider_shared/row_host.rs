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
use crate::param_surface::{RowIndex, RowRole};

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
}
