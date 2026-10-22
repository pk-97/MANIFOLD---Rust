//! The live push's Shapes and MappingRanges group (UI_INVALIDATION D22):
//! every display of a user-set live scalar, written in place with no
//! projection and no rebuild. Drawers and handles on every row card, the
//! layer-header and Audio Setup gains, clip-trigger sensitivity, and the
//! Ableton range bands on cards and macros. Reaches the same
//! cards `sync_card_values` does. Allocation-free when nothing moved.

use manifold_core::audio_mod::TriggerAction;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_ui::panels::param_slider_shared::{AudioShape, EnvelopeShape, RowShapes};

use crate::ui_root::UIRoot;

fn step_amount(action: TriggerAction) -> Option<f32> {
    match action {
        TriggerAction::Step { amount, .. } => Some(amount),
        _ => None,
    }
}

/// One row's live scalars, resolved the way the card projection resolves
/// them: the last enabled driver, envelope and audio mod on the param, and
/// the first Ableton mapping.
pub(crate) fn row_shapes(inst: &PresetInstance, param_id: &str) -> RowShapes {
    RowShapes {
        driver_trim: inst
            .drivers
            .iter()
            .flatten()
            .rfind(|d| d.enabled && d.param_id.as_ref() == param_id)
            .map(|d| (d.trim_min, d.trim_max)),
        envelope: inst
            .envelopes
            .iter()
            .flatten()
            .rfind(|e| e.enabled && e.param_id.as_ref() == param_id)
            .map(|e| EnvelopeShape {
                target_norm: e.target_normalized,
                decay: e.decay_beats,
                step_amount: step_amount(e.action),
            }),
        audio: inst
            .audio_mods
            .iter()
            .flatten()
            .rfind(|m| m.enabled && m.param_id.as_ref() == param_id)
            .map(|m| AudioShape {
                range_min: m.shape.range_min,
                range_max: m.shape.range_max,
                sensitivity: m.shape.sensitivity,
                attack_ms: m.shape.attack_ms,
                release_ms: m.shape.release_ms,
                invert: m.shape.invert,
                step_amount: step_amount(m.action),
            }),
        ableton_range: inst
            .ableton_mappings
            .iter()
            .flatten()
            .find(|m| !param_id.is_empty() && m.param_id.as_ref() == param_id)
            .map(|m| (m.range_min, m.range_max)),
    }
}

/// Push every Shapes and MappingRanges display of `project` in place.
pub fn sync_live_shapes(ui: &mut UIRoot, project: &Project, active_layer: Option<usize>) {
    let tree = &mut ui.tree;
    for (i, effect) in project.settings.master_effects.iter().enumerate() {
        if let Some(card) = ui.inspector.master_effect_mut(i) {
            card.sync_live_shapes(tree, &mut |id| row_shapes(effect, id));
        }
    }
    let layer = active_layer.and_then(|idx| project.timeline.layers.get(idx));
    if let Some(effects) = layer.and_then(|l| l.effects.as_ref()) {
        for (i, effect) in effects.iter().enumerate() {
            if let Some(card) = ui.inspector.layer_effect_mut(i) {
                card.sync_live_shapes(tree, &mut |id| row_shapes(effect, id));
            }
        }
    }
    if let Some(gp) = layer.and_then(|l| l.gen_params())
        && let Some(card) = ui.inspector.gen_params_mut()
    {
        card.sync_live_shapes(tree, &mut |id| row_shapes(gp, id));
    }
    if let Some(layer) = layer {
        let mut sensitivities = layer.clip_triggers.iter().map(|t| t.shape.sensitivity);
        ui.inspector
            .audio_trigger_section_mut()
            .sync_sensitivities(tree, &layer.layer_id, &mut sensitivities);
    }
    if let Some((_, scene_layer)) = ui.scene_setup_panel.live_layer_id()
        .and_then(|id| project.timeline.find_layer_by_id(id))
        && let Some(gp) = scene_layer.gen_params()
    {
        ui.scene_setup_panel.sync_live_shapes(tree, &mut |id| row_shapes(gp, id));
    }

    let mut gains = project.timeline.layers.iter().map(|l| (l.layer_id.as_str(), l.audio_gain_db));
    ui.layer_headers.sync_audio_gains(tree, &mut gains);
    let mut send_gains = project.audio_setup.sends.iter().map(|s| (&s.id, s.gain_db));
    ui.audio_setup_panel.sync_send_gains(tree, &mut send_gains);

    ui.inspector.macros_panel_mut().sync_ableton_ranges(tree, &macro_ableton_ranges(project));
}

/// Each macro slot's Ableton range. Which slots have one is membership: the
/// inspector projection hands it to the macros panel before its build.
pub(crate) fn macro_ableton_ranges(project: &Project) -> [Option<(f32, f32)>; manifold_core::MACRO_COUNT] {
    let mut ranges = [None; manifold_core::MACRO_COUNT];
    for (range, slot) in ranges.iter_mut().zip(&project.settings.macro_bank.slots) {
        *range = slot.ableton_mapping.as_ref().map(|m| (m.range_min, m.range_max));
    }
    ranges
}
