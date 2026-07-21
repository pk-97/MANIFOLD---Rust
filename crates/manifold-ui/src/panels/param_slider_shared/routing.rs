//! Click-resolution helpers for parameter slider row drawers.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;


// ── Shared helper functions ─────────────────────────────────────

/// BUG-250: the click-to-change action set for an enum (`value_labels`)
/// row's value cell — the behavior SCENE_OBJECT_AND_PANEL_V2 D9 committed
/// to, restored in the shared card core after C-P1c/d deleted the bespoke
/// producers. A 2-label row cycles to the next value through the
/// `ParamSnapshot`/`ParamChanged`/`ParamCommit` trio (one undo unit; the
/// scene id_map interception comes free); a 3+-label row opens the shared
/// dropdown via [`PanelAction::ParamEnumDropdown`]. `current_value` is the
/// row's base value, `min` the param's range minimum (enum index = value −
/// min, same encoding as [`format_param_value`]).
pub(crate) fn enum_value_cell_actions(
    target: crate::panels::GraphParamTarget,
    param_id: manifold_foundation::ParamId,
    labels: &[String],
    current_value: f32,
    min: f32,
    cell_node_id: NodeId,
) -> Vec<crate::panels::PanelAction> {
    use crate::panels::PanelAction;
    let count = labels.len();
    if count == 0 {
        return Vec::new();
    }
    let current_index =
        ((current_value - min).round() as i32).clamp(0, count as i32 - 1) as usize;
    if count <= 2 {
        let next = (current_index + 1) % count;
        let new_value = min + next as f32;
        vec![
            PanelAction::Scrub(ValueRef::Param(target.clone(), param_id.clone()), ScrubPhase::Begin),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), param_id.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(new_value)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, param_id), ScrubPhase::Commit),
        ]
    } else {
        vec![PanelAction::Root(RootAction::ParamEnumDropdown {
            target,
            param_id,
            labels: labels.to_vec(),
            current_index: current_index as u32,
            cell_node_id,
        })]
    }
}


// ── Shared event helpers ────────────────────────────────────────

impl DriverConfigIds {
    /// Resolve a clicked node against THIS drawer's own buttons (the
    /// widget-contract split, D5 `docs/WIDGET_TREE_DESIGN.md` — the bundle
    /// knows its own nodes; `row_action` supplies the row via `RowIndex`).
    /// The Free field is *not* here — it opens a type-in (handled via
    /// [`driver_free_field_index`] on the tree-aware click path), not a
    /// config command.
    pub(crate) fn resolve(&self, node_id: NodeId) -> Option<DriverConfigAction> {
        for (j, &bid) in self.beat_div_btn_ids.iter().enumerate() {
            if node_id == bid {
                return Some(DriverConfigAction::BeatDiv(j));
            }
        }
        if node_id == self.straight_btn_id {
            return Some(DriverConfigAction::Straight);
        }
        if node_id == self.dotted_btn_id {
            return Some(DriverConfigAction::Dotted);
        }
        if node_id == self.triplet_btn_id {
            return Some(DriverConfigAction::Triplet);
        }
        if node_id == self.invert_btn_id {
            return Some(DriverConfigAction::Invert);
        }
        for (j, &wid) in self.wave_btn_ids.iter().enumerate() {
            if node_id == wid {
                return Some(DriverConfigAction::Wave(j));
            }
        }
        None
    }
}


/// If `node_id` is a driver drawer's Free-period field, return its param index.
/// The Free field opens a beats type-in (free mode) rather than issuing a config
/// command, so it's matched separately from [`DriverConfigIds::resolve`].
pub(crate) fn driver_free_field_index(
    node_id: NodeId,
    driver_config_ids: &[Option<DriverConfigIds>],
) -> Option<usize> {
    driver_config_ids.iter().enumerate().find_map(|(pi, cfg)| {
        cfg.as_ref()
            .filter(|c| c.free_btn_id == node_id)
            .map(|_| pi)
    })
}


impl AbletonConfigIds {
    /// Resolve a clicked node against THIS drawer's own Invert button (the
    /// widget-contract split, D5) — the `ParamCardPanel` row-model twin of
    /// [`check_ableton_config_click`] below, which stays for `macros_panel`
    /// (a different, non-row-model panel; not this design's scope).
    pub(crate) fn resolve(&self, node_id: NodeId) -> bool {
        node_id == self.invert_btn_id
    }
}


/// Check if a click hit an Ableton config button. Returns param index if matched.
/// Used by `macros_panel` (its own bespoke, non-`RowIndex` dispatch) — kept
/// array-scanning; `ParamCardPanel` uses [`AbletonConfigIds::resolve`] instead.
pub(crate) fn check_ableton_config_click(
    node_id: NodeId,
    ableton_config_ids: &[Option<AbletonConfigIds>],
) -> Option<(usize, AbletonConfigClick)> {
    for (pi, ids) in ableton_config_ids.iter().enumerate() {
        if let Some(c) = ids
            && node_id == c.invert_btn_id
        {
            return Some((pi, AbletonConfigClick::Invert));
        }
    }
    None
}


pub(crate) enum AbletonConfigClick {
    Invert,
}


// ── Shared per-parameter click dispatch ─────────────────────────────
//
// The old array-scanning row-click gauntlet DIED in P2
// (`docs/WIDGET_TREE_DESIGN.md` D5) — `ParamCardPanel::row_action` routes
// through `RowIndex` instead. `AudioConfigClick`/`resolve_audio_config_click`
// below is the one surviving per-role resolver: the audio drawer's flat
// button index can't be split into typed sub-fields the way driver/ableton
// config can (`DriverConfigIds::resolve`/`AbletonConfigIds::resolve`), so it
// stays a function — but scoped to the ONE row `row_action` already
// resolved via `RowIndex`, never scanning every row's drawer.

/// A click inside ONE row's audio-mod drawer, resolved from its flat button
/// index (`DrawerIds::resolve_button`). Mirrors the variant shapes
/// `PanelAction`'s `AudioMod*` family expects; `row_action` supplies `pi`.
pub(crate) enum AudioConfigClick {
    SelectSend(usize),
    SelectChip(usize),
    ToggleMatrix,
    SelectKind(usize),
    SelectBand(usize),
    ToggleInvert,
    SelectAction(usize),
    SelectWrap(usize),
    SelectTriggerMode(usize),
}


/// Resolve a clicked node against ONE row's audio-mod drawer. Flat index
/// layout: sends, the Listen chips (`trigger_source_chips(current)` + the
/// trailing "Custom" cell), then — only while the matrix is open — the
/// Feature and Band rows, then — only where shaping is offered (every target
/// EXCEPT `is_trigger_gate`, which fires on the raw BUG-242 edge) the Invert
/// toggle, then (D8, non-toggle/non-trigger rows only) the Action row, then
/// — while armed to Step — the Wrap row, then the trailing Mode row (§9
/// U2/D3). Must stay in lockstep with the row order `build_audio_mod_drawer`
/// actually builds.
pub(crate) fn resolve_audio_config_click(
    dids: &crate::panels::drawer::DrawerIds,
    send_count: usize,
    mod_state: &ParamModState,
    row: &ParamRow,
    pi: usize,
    node_id: NodeId,
) -> Option<AudioConfigClick> {
    let flat = dids.resolve_button(node_id)?;
    if flat < send_count {
        return Some(AudioConfigClick::SelectSend(flat));
    }
    let mut f = flat - send_count;
    let current = crate::types::AudioFeature::new(
        audio_kind_from_index(mod_state.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize),
        audio_band_from_index(mod_state.audio_band_idx.get(pi).copied().unwrap_or(0) as usize),
    );
    let chip_count = trigger_source_chips(current).len();
    if f < chip_count {
        return Some(AudioConfigClick::SelectChip(f));
    }
    f -= chip_count;
    if f == 0 {
        return Some(AudioConfigClick::ToggleMatrix);
    }
    f -= 1;
    if mod_state.audio_matrix_open.get(pi).copied().unwrap_or(false) {
        if f < AUDIO_KIND_COUNT {
            return Some(AudioConfigClick::SelectKind(f));
        }
        f -= AUDIO_KIND_COUNT;
        if f < AUDIO_BAND_COUNT {
            return Some(AudioConfigClick::SelectBand(f));
        }
        f -= AUDIO_BAND_COUNT;
    }
    let is_gate = row.spec.is_trigger_gate;
    if !is_gate {
        if f == 0 {
            return Some(AudioConfigClick::ToggleInvert);
        }
        f -= 1;
    }
    let show_action = !row.spec.is_toggle && !row.spec.is_trigger;
    if show_action {
        if f < AUDIO_ACTION_COUNT {
            return Some(AudioConfigClick::SelectAction(f));
        }
        f -= AUDIO_ACTION_COUNT;
        let action_idx = mod_state.audio_action_idx.get(pi).copied().unwrap_or(0);
        if action_idx == 1 {
            if f < AUDIO_WRAP_COUNT {
                return Some(AudioConfigClick::SelectWrap(f));
            }
            return Some(AudioConfigClick::SelectTriggerMode(f - AUDIO_WRAP_COUNT));
        }
        return Some(AudioConfigClick::SelectTriggerMode(f));
    }
    Some(AudioConfigClick::SelectTriggerMode(f))
}

