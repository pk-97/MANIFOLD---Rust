//! The live push's Shapes and MappingRanges group (UI_INVALIDATION D22):
//! every display of a user-set live scalar, written in place with no
//! projection and no rebuild. Drawers and handles on every row card, the
//! layer-header and Audio Setup gains, clip-trigger sensitivity, and the
//! Ableton range bands on cards and macros. Reaches the same
//! cards `sync_card_values` does. Allocation-free when nothing moved.

use manifold_core::GraphTarget;
use manifold_core::audio_mod::TriggerAction;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_ui::panels::param_slider_shared::{AudioShape, EnvelopeShape, RowShapes};

use crate::ui_bridge::scrub::ResolvedScrub;
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

/// One card row's shapes: the project's, with the in-flight gesture laid over
/// them when it is on this card's param. `owner` names the card's graph and is
/// only built while a gesture is live.
fn card_row_shapes(
    inst: &PresetInstance,
    owner: impl Fn() -> GraphTarget,
    drag: Option<&ResolvedScrub>,
    param_id: &str,
) -> RowShapes {
    let mut shapes = row_shapes(inst, param_id);
    if let Some(drag) = drag {
        drag.overlay_shapes(&owner(), param_id, &mut shapes);
    }
    shapes
}

/// Push every Shapes and MappingRanges display of `project` in place. A
/// gesture's live value draws over the project's, which keeps the pre-drag
/// value until the commit lands.
pub fn sync_live_shapes(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    drag: Option<&ResolvedScrub>,
) {
    let tree = &mut ui.tree;
    for (i, effect) in project.settings.master_effects.iter().enumerate() {
        if let Some(card) = ui.inspector.master_effect_mut(i) {
            card.sync_live_shapes(tree, &mut |id| {
                card_row_shapes(effect, || GraphTarget::Effect(effect.id.clone()), drag, id)
            });
        }
    }
    let layer = active_layer.and_then(|idx| project.timeline.layers.get(idx));
    if let Some(effects) = layer.and_then(|l| l.effects.as_ref()) {
        for (i, effect) in effects.iter().enumerate() {
            if let Some(card) = ui.inspector.layer_effect_mut(i) {
                card.sync_live_shapes(tree, &mut |id| {
                    card_row_shapes(effect, || GraphTarget::Effect(effect.id.clone()), drag, id)
                });
            }
        }
    }
    if let Some(layer) = layer
        && let Some(gp) = layer.gen_params()
        && let Some(card) = ui.inspector.gen_params_mut()
    {
        card.sync_live_shapes(tree, &mut |id| {
            card_row_shapes(
                gp,
                || GraphTarget::Generator(layer.layer_id.clone()),
                drag,
                id,
            )
        });
    }
    if let Some(layer) = layer {
        let mut sensitivities =
            layer
                .clip_triggers
                .iter()
                .enumerate()
                .map(|(index, t)| match drag {
                    Some(ResolvedScrub::AudioTriggerShape {
                        layer_id,
                        index: dragged,
                        live,
                        ..
                    }) if *layer_id == layer.layer_id && *dragged == index => live.sensitivity,
                    _ => t.shape.sensitivity,
                });
        ui.inspector.audio_trigger_section_mut().sync_sensitivities(
            tree,
            &layer.layer_id,
            &mut sensitivities,
        );
    }
    if let Some((_, scene_layer)) = ui
        .scene_setup_panel
        .live_layer_id()
        .and_then(|id| project.timeline.find_layer_by_id(id))
        && let Some(gp) = scene_layer.gen_params()
    {
        ui.scene_setup_panel.sync_live_shapes(tree, &mut |id| {
            card_row_shapes(
                gp,
                || GraphTarget::Generator(scene_layer.layer_id.clone()),
                drag,
                id,
            )
        });
    }

    let mut gains = project.timeline.layers.iter().map(|l| {
        let db = match drag {
            Some(ResolvedScrub::LayerAudioGain { layer_id, live, .. })
                if *layer_id == l.layer_id =>
            {
                *live
            }
            _ => l.audio_gain_db,
        };
        (l.layer_id.as_str(), db)
    });
    ui.layer_headers.sync_audio_gains(tree, &mut gains);
    let mut send_gains = project.audio_setup.sends.iter().map(|s| {
        let db = match drag {
            Some(ResolvedScrub::AudioSendGain { send_id, live, .. }) if *send_id == s.id => *live,
            _ => s.gain_db,
        };
        (&s.id, db)
    });
    ui.audio_setup_panel.sync_send_gains(tree, &mut send_gains);

    let mut ranges = macro_ableton_ranges(project);
    if let Some(ResolvedScrub::AbletonMacroTrim { slot_idx, live, .. }) = drag
        && let Some(range) = ranges.get_mut(*slot_idx)
        && range.is_some()
    {
        *range = Some(*live);
    }
    ui.inspector
        .macros_panel_mut()
        .sync_ableton_ranges(tree, &ranges);
}

/// Each macro slot's Ableton range. Which slots have one is membership: the
/// inspector projection hands it to the macros panel before its build.
pub(crate) fn macro_ableton_ranges(
    project: &Project,
) -> [Option<(f32, f32)>; manifold_core::MACRO_COUNT] {
    let mut ranges = [None; manifold_core::MACRO_COUNT];
    for (range, slot) in ranges.iter_mut().zip(&project.settings.macro_bank.slots) {
        *range = slot
            .ableton_mapping
            .as_ref()
            .map(|m| (m.range_min, m.range_max));
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{ParamId, ParameterDriver};
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use manifold_ui::panels::TrimKind;

    #[test]
    fn a_driver_trim_drag_shows_its_live_range_while_the_project_holds_the_baseline() {
        let mut fx = PresetInstance::new(PresetTypeId::new("Test"));
        let owner = GraphTarget::Effect(fx.id.clone());
        fx.drivers = Some(vec![ParameterDriver {
            param_id: ParamId::Borrowed("amount"),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            frame_aligned: false,
            legacy_param_index: None,
            is_paused_by_user: false,
        }]);
        let drag = ResolvedScrub::Trim {
            kind: TrimKind::Driver,
            target: owner.clone(),
            ableton_target: None,
            param_id: ParamId::Borrowed("amount"),
            baseline: (0.0, 1.0),
            live: (0.3, 0.9),
        };

        let dragged = card_row_shapes(&fx, || owner.clone(), Some(&drag), "amount");
        assert_eq!(dragged.driver_trim, Some((0.3, 0.9)));
        let at_rest = card_row_shapes(&fx, || owner.clone(), None, "amount");
        assert_eq!(at_rest.driver_trim, Some((0.0, 1.0)));
        let other_card = GraphTarget::Effect(manifold_core::EffectId::new("other"));
        let elsewhere = card_row_shapes(&fx, || other_card.clone(), Some(&drag), "amount");
        assert_eq!(elsewhere.driver_trim, Some((0.0, 1.0)));
    }
}
