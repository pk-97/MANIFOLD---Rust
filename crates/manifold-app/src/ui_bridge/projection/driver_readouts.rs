//! Driver period readouts: rewrites each frame-aligned driver's shown period
//! in place when the content clock's bpm moves (UI_INVALIDATION D20), with no
//! projection and no rebuild.

use crate::ui_root::UIRoot;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;

/// The frame-aligned driver period readout `build_card_modulation` projects,
/// for one param at the given content timing.
fn driver_frame_rate(
    inst: &PresetInstance,
    param_id: &str,
    timing: (manifold_core::Bpm, f32),
) -> Option<(u32, f32)> {
    inst.drivers
        .iter()
        .flatten()
        .rfind(|d| d.enabled && d.frame_aligned && d.param_id.as_ref() == param_id)
        .and_then(|d| d.effective_period_frames(timing.0, timing.1))
        .map(|frames| (frames, timing.1 / frames as f32))
}

/// Reaches the same cards `sync_card_values` does.
pub fn sync_driver_readouts(ui: &mut UIRoot, project: &Project, active_layer: Option<usize>) {
    let timing = (
        ui.driver_bpm.unwrap_or(project.settings.bpm),
        project.settings.frame_rate,
    );
    let tree = &mut ui.tree;
    for (i, effect) in project.settings.master_effects.iter().enumerate() {
        if let Some(card) = ui.inspector.master_effect_mut(i) {
            card.sync_driver_frame_rates(tree, &mut |id| driver_frame_rate(effect, id, timing));
        }
    }
    let layer = active_layer.and_then(|idx| project.timeline.layers.get(idx));
    if let Some(effects) = layer.and_then(|l| l.effects.as_ref()) {
        for (i, effect) in effects.iter().enumerate() {
            if let Some(card) = ui.inspector.layer_effect_mut(i) {
                card.sync_driver_frame_rates(tree, &mut |id| driver_frame_rate(effect, id, timing));
            }
        }
    }
    if let Some(gp) = layer.and_then(|l| l.gen_params())
        && let Some(card) = ui.inspector.gen_params_mut()
    {
        card.sync_driver_frame_rates(tree, &mut |id| driver_frame_rate(gp, id, timing));
    }
    if let Some((_, scene_layer)) = ui
        .scene_setup_panel
        .live_layer_id()
        .and_then(|id| project.timeline.find_layer_by_id(id))
        && let Some(gp) = scene_layer.gen_params()
    {
        ui.scene_setup_panel
            .sync_driver_frame_rates(tree, &mut |id| driver_frame_rate(gp, id, timing));
    }
}
