//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

use manifold_core::{Beats, LayerId, Seconds};
use manifold_core::effects::{EffectInstance, ParameterDriver, ParamEnvelope};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLedBrightnessCommand,
    ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
    PasteGeneratorCommand, ChangeMacroCommand,
};
use manifold_editing::commands::effects::{
    ToggleEffectCommand, ChangeEffectParamCommand, RemoveEffectCommand, ReorderEffectCommand,
    ReorderEffectGroupCommand,
};
use manifold_editing::commands::envelopes::{
    ChangeLayerEnvelopeADSRCommand,
    ChangeLayerEnvelopeTargetCommand,
};
use manifold_editing::commands::effect_target::{EffectTarget, DriverTarget};
use manifold_editing::commands::drivers::{
    AddDriverCommand, ToggleDriverEnabledCommand,
    ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand,
    ToggleDriverReversedCommand, ChangeTrimCommand,
};
use manifold_editing::commands::clip::{
    SlipClipCommand, ChangeClipLoopCommand,
};
use manifold_ui::{PanelAction, InspectorTab, DriverConfigAction};

use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use super::DispatchResult;
use super::{resolve_effects_read, resolve_effects_ref, resolve_effects_mut};

pub(super) fn dispatch_inspector(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    _content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    drag_snapshot: &mut Option<f32>,
    trim_snapshot: &mut Option<(f32, f32)>,
    adsr_snapshot: &mut Option<(f32, f32, f32, f32)>,
    target_snapshot: &mut Option<f32>,
    active_inspector_drag: &mut Option<crate::app::ActiveInspectorDrag>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Macro sliders ─────────────────────────────────────────
        PanelAction::MacroSnapshot(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                *drag_snapshot = Some(project.settings.macro_bank.slots[idx].value);
            }
            DispatchResult::handled()
        }
        PanelAction::MacroChanged(idx, val) => {
            let idx = *idx;
            let val = *val;
            manifold_core::macro_bank::MacroBank::apply_macro(project, idx, val);
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                manifold_core::macro_bank::MacroBank::apply_macro(p, idx, val);
            })));
            DispatchResult::handled()
        }
        PanelAction::MacroCommit(idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let idx = *idx;
                if idx < manifold_core::macro_bank::MACRO_COUNT {
                    let new_val = project.settings.macro_bank.slots[idx].value;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMacroCommand::new(
                            idx, old_val, new_val,
                        );
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::MacroRightClick(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let old = project.settings.macro_bank.slots[idx].value;
                if old.abs() > f32::EPSILON {
                    manifold_core::macro_bank::MacroBank::apply_macro(project, idx, 0.0);
                    let cmd = ChangeMacroCommand::new(idx, old, 0.0);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::MacroReset(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let old = project.settings.macro_bank.slots[idx].value;
                if old.abs() > f32::EPSILON {
                    manifold_core::macro_bank::MacroBank::apply_macro(project, idx, 0.0);
                    let cmd = ChangeMacroCommand::new(idx, old, 0.0);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── Master chrome ──────────────────────────────────────────
        PanelAction::MasterOpacitySnapshot => {
            *drag_snapshot = Some(project.settings.master_opacity);
            *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::MasterOpacity(project.settings.master_opacity));
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            project.settings.master_opacity = *val;
            if let Some(crate::app::ActiveInspectorDrag::MasterOpacity(v)) = active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                p.settings.master_opacity = v;
            })));
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                let new_val = project.settings.master_opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::MasterCollapseToggle => {
            ui.inspector.master_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::MasterExitPathClicked => {
            // Handled by try_open_dropdown in ui_root.rs — opens exit path dropdown.
            DispatchResult::handled()
        }
        PanelAction::SetLedExitIndex(idx) => {
            let idx = *idx;
            project.settings.led_exit_index = idx;
            // Push to content thread so the LED pipeline picks it up
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_exit_index = idx;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityRightClick => {
            let old = project.settings.master_opacity;
            if (old - 1.0).abs() > f32::EPSILON {
                project.settings.master_opacity = 1.0;
                let cmd = ChangeMasterOpacityCommand::new(old, 1.0);
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── LED brightness ───────────────────────────────────────
        PanelAction::LedBrightnessSnapshot => {
            *drag_snapshot = Some(project.settings.led_brightness);
            *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LedBrightness(
                project.settings.led_brightness,
            ));
            DispatchResult::handled()
        }
        PanelAction::LedBrightnessChanged(val) => {
            project.settings.led_brightness = *val;
            if let Some(crate::app::ActiveInspectorDrag::LedBrightness(v)) = active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                p.settings.led_brightness = v;
            })));
            DispatchResult::handled()
        }
        PanelAction::LedBrightnessCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                let new_val = project.settings.led_brightness;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeLedBrightnessCommand::new(old_val, new_val);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::LedBrightnessRightClick => {
            let old = project.settings.led_brightness;
            if (old - 1.0).abs() > f32::EPSILON {
                project.settings.led_brightness = 1.0;
                let cmd = ChangeLedBrightnessCommand::new(old, 1.0);
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Layer chrome ───────────────────────────────────────────
        PanelAction::LayerOpacitySnapshot => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(idx) {
                    *drag_snapshot = Some(layer.opacity);
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LayerOpacity {
                        layer_id: layer.layer_id.clone(),
                        value: layer.opacity,
                    });
                }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityChanged(val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(idx) {
                    layer.opacity = *val;
                }
                if let Some(crate::app::ActiveInspectorDrag::LayerOpacity { value, .. }) = active_inspector_drag {
                    *value = *val;
                }
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                        layer.opacity = v;
                    }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityCommit => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(old_val) = drag_snapshot.take()
                && let Some(idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(idx) {
                        let layer_id = layer.layer_id.clone();
                        let new_val = layer.opacity;
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let cmd = ChangeLayerOpacityCommand::new(layer_id, old_val, new_val);
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::LayerChromeCollapseToggle => {
            ui.inspector.layer_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::LayerOpacityRightClick => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(idx) {
                    let layer_id = layer.layer_id.clone();
                    let old = layer.opacity;
                    if (old - 1.0).abs() > f32::EPSILON {
                        layer.opacity = 1.0;
                        let cmd = ChangeLayerOpacityCommand::new(layer_id, old, 1.0);
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Clip chrome ────────────────────────────────────────────
        PanelAction::ClipChromeCollapseToggle => {
            ui.inspector.clip_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::ClipBpmClicked => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let old_loop = clip.is_looping;
                    let old_dur = clip.loop_duration_beats;
                    let cmd = ChangeClipLoopCommand::new(
                        clip_id, old_loop, !old_loop, old_dur, old_dur,
                    );
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipSlipSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    *drag_snapshot = Some(clip.in_point.as_f32());
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::ClipSlip {
                        clip_id: clip_id.clone(),
                        value: clip.in_point.as_f32(),
                    });
                }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.in_point = Seconds::from_f32(val.max(0.0));
                }
                if let Some(crate::app::ActiveInspectorDrag::ClipSlip { value, .. }) = active_inspector_drag {
                    *value = val.max(0.0);
                }
                let v = Seconds::from_f32(val.max(0.0));
                let cid = clip_id.clone();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(clip) = p.timeline.find_clip_by_id_mut(&cid) {
                        clip.in_point = v;
                    }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipCommit => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(clip_id) = &selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                        let new_val = clip.in_point;
                        if (old_val - new_val.as_f32()).abs() > f32::EPSILON {
                            let cmd = SlipClipCommand::new(clip_id, Seconds::from_f32(old_val), new_val);
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    *drag_snapshot = Some(clip.loop_duration_beats.as_f32());
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::ClipLoop {
                        clip_id: clip_id.clone(),
                        value: clip.loop_duration_beats.as_f32(),
                    });
                }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.loop_duration_beats = Beats::from_f32(val.max(0.0));
                }
                if let Some(crate::app::ActiveInspectorDrag::ClipLoop { value, .. }) = active_inspector_drag {
                    *value = val.max(0.0);
                }
                let v = Beats::from_f32(val.max(0.0));
                let cid = clip_id.clone();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(clip) = p.timeline.find_clip_by_id_mut(&cid) {
                        clip.loop_duration_beats = v;
                    }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopCommit => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(clip_id) = &selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                        let new_val = clip.loop_duration_beats;
                        let is_looping = clip.is_looping;
                        if (old_val - new_val.as_f32()).abs() > f32::EPSILON {
                            let cmd = ChangeClipLoopCommand::new(
                                clip_id, is_looping, is_looping, Beats::from_f32(old_val), new_val,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        PanelAction::ClipSlipRightClick => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                    let old = clip.in_point;
                    if old.as_f32().abs() > f32::EPSILON {
                        clip.in_point = Seconds(0.0);
                        let cmd = SlipClipCommand::new(clip_id, old, Seconds(0.0));
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::ClipLoopRightClick => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                    let old_dur = clip.loop_duration_beats;
                    let full_dur = clip.duration_beats;
                    let is_looping = clip.is_looping;
                    if (old_dur - full_dur).abs().as_f32() > f32::EPSILON {
                        clip.loop_duration_beats = full_dur;
                        let cmd = ChangeClipLoopCommand::new(
                            clip_id, is_looping, is_looping, old_dur, full_dur,
                        );
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect operations ──────────────────────────────────────
        PanelAction::EffectToggle(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            // Read current state to determine target + build commands
            let target = super::resolve_effect_target(tab, active_layer, project);
            let new_enabled = {
                let (effects_ref, _) = resolve_effects_read(tab, project, active_layer, selection);
                effects_ref.and_then(|e| e.get(*fx_idx)).map(|fx| !fx.enabled).unwrap_or(true)
            };
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            {
                let (effects_ref, _) = resolve_effects_read(tab, project, active_layer, selection);
                if let Some(effects) = effects_ref {
                    for &idx in &indices {
                        if let Some(fx) = effects.get(idx)
                            && fx.enabled != new_enabled
                        {
                            commands.push(Box::new(ToggleEffectCommand::new(
                                target.clone(), idx, fx.enabled, new_enabled,
                            )));
                        }
                    }
                }
            }
            // Apply locally for immediate visual feedback
            {
                let (effects_mut, _) = resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut {
                    for &idx in &indices {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.enabled = new_enabled;
                        }
                    }
                }
            }
            if !commands.is_empty() {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::ExecuteBatch(commands, "Toggle effects".into()),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            let new_collapsed;
            {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut {
                    new_collapsed = effects.get(*fx_idx).map(|fx| !fx.collapsed).unwrap_or(true);
                    for &idx in &indices {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.collapsed = new_collapsed;
                        }
                    }
                } else {
                    new_collapsed = true;
                }
            }
            // Send to content thread so snapshot sync doesn't overwrite
            let target = super::resolve_effect_target(tab, active_layer, project);
            let indices_owned = indices;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                let effects = match &target {
                    EffectTarget::Master => Some(&mut p.settings.master_effects),
                    EffectTarget::Layer { layer_id } => {
                        p.timeline.find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut())
                    }
                };
                if let Some(effects) = effects {
                    for &idx in &indices_owned {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.collapsed = new_collapsed;
                        }
                    }
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::EffectCardClicked(_) => {
            // Deselect generator card when an effect card is clicked
            if let Some(gp) = ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ui.tree, false);
            }
            let tree = &mut ui.tree;
            let inspector = &mut ui.inspector;
            inspector.apply_selection_visuals(tree);
            DispatchResult::handled()
        }
        PanelAction::EffectParamRightClick(fx_idx, param_idx, default_val) => {
            let tab = ui.inspector.last_effect_tab();
            let (effects_mut, target) = resolve_effects_mut(tab, project, active_layer, selection);
            if let Some(effects) = effects_mut
                && let Some(fx) = effects.get_mut(*fx_idx) {
                    let old = fx.get_base_param(*param_idx);
                    if (old - *default_val).abs() > f32::EPSILON {
                        fx.set_base_param(*param_idx, *default_val);
                        let cmd = ChangeEffectParamCommand::new(
                            target, *fx_idx, *param_idx, old, *default_val,
                        );
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::EffectParamSnapshot(fx_idx, param_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let effects = resolve_effects_ref(tab, project, active_layer, selection);
            if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                let val = fx.get_base_param(*param_idx);
                *drag_snapshot = Some(val);
                *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::EffectParam {
                    tab,
                    layer_id: active_layer.clone().unwrap_or_default(),
                    effect_idx: *fx_idx,
                    param_idx: *param_idx,
                    value: val,
                    clip_id: if tab == InspectorTab::Clip { selection.primary_selected_clip_id.clone() } else { None },
                });
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamChanged(fx_idx, param_idx, val) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut
                    && let Some(fx) = effects.get_mut(*fx_idx) {
                        fx.set_base_param(*param_idx, *val);
                    }
                if let Some(crate::app::ActiveInspectorDrag::EffectParam { value, .. }) = active_inspector_drag {
                    *value = *val;
                }
                let fi = *fx_idx;
                let pi = *param_idx;
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                let clip_id = selection.primary_selected_clip_id.clone();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    let effects: Option<&mut Vec<EffectInstance>> = match tab {
                        InspectorTab::Master => Some(&mut p.settings.master_effects),
                        InspectorTab::Layer => p.timeline.find_layer_by_id_mut(&layer_id).map(|(_, l)| l.effects_mut()),
                        InspectorTab::Clip => clip_id.as_ref().and_then(|cid| p.timeline.find_clip_by_id_mut(cid).map(|c| &mut c.effects)),
                    };
                    if let Some(effects) = effects
                        && let Some(fx) = effects.get_mut(fi) {
                            fx.set_base_param(pi, v);
                        }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamCommit(fx_idx, param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                    let new_val = fx.get_base_param(*param_idx);
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let target = super::resolve_effect_target(tab, active_layer, project);
                        let cmd = ChangeEffectParamCommand::new(
                            target, *fx_idx, *param_idx, old_val, new_val,
                        );
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::EffectDriverToggle(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_target = super::resolve_effect_target(tab, active_layer, project);
            let (effects_ref, _) = resolve_effects_read(tab, project, active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*ei) {
                    let driver_target = DriverTarget::Effect {
                        effect_target,
                        effect_index: *ei,
                    };
                    let driver_idx = fx.drivers.as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32));
                    if let Some(di) = driver_idx {
                        let old = fx.drivers.as_ref().unwrap()[di].enabled;
                        let cmd = ToggleDriverEnabledCommand::new(
                            driver_target, di, old, !old,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                    } else {
                        let driver = ParameterDriver {
                            param_index: *pi as i32,
                            beat_division: BeatDivision::Quarter,
                            waveform: DriverWaveform::Sine,
                            enabled: true,
                            phase: 0.0,
                            base_value: fx.param_values.get(*pi).copied().unwrap_or(0.0),
                            trim_min: 0.0,
                            trim_max: 1.0,
                            reversed: false,
                            is_paused_by_user: false,
                        };
                        let cmd = AddDriverCommand::new(driver_target, driver);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                    }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvelopeToggle(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
            };
            if let Some(et) = effect_type {
                let et_content = et.clone(); // clone before local use for content thread sync
                let pi2 = *pi as i32;
                let layer_idx = super::resolve_active_layer_index(active_layer, project);
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => {
                        layer_idx.and_then(|idx| {
                            project.timeline.layers.get_mut(idx)
                                .map(|l| l.envelopes_mut())
                        })
                    }
                    InspectorTab::Clip | InspectorTab::Master => None,
                };
                if let Some(envs) = envs {
                    let env_idx = envs.iter().position(|e|
                        e.target_effect_type == et && e.param_index == pi2
                    );
                    if let Some(idx) = env_idx {
                        envs[idx].enabled = !envs[idx].enabled;
                    } else {
                        envs.push(ParamEnvelope {
                            target_effect_type: et,
                            param_index: pi2,
                            enabled: true,
                            attack_beats: 0.0,
                            decay_beats: 0.0,
                            sustain_level: 0.0,
                            release_beats: 0.0,
                            target_normalized: 1.0,
                            current_level: 0.0,
                        });
                    }
                }
                // Sync to content thread so the next snapshot doesn't overwrite
                let et2 = et_content;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                        let envs = layer.envelopes_mut();
                        let env_idx = envs.iter().position(|e|
                            e.target_effect_type == et2 && e.param_index == pi2
                        );
                        if let Some(idx) = env_idx {
                            envs[idx].enabled = !envs[idx].enabled;
                        } else {
                            envs.push(ParamEnvelope {
                                target_effect_type: et2,
                                param_index: pi2,
                                enabled: true,
                                attack_beats: 0.0,
                                decay_beats: 0.0,
                                sustain_level: 0.0,
                                release_beats: 0.0,
                                target_normalized: 1.0,
                                current_level: 0.0,
                            });
                        }
                    }
                })));
            }
            DispatchResult::structural()
        }
        PanelAction::EffectDriverConfig(ei, pi, cfg) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_target = super::resolve_effect_target(tab, active_layer, project);
            let target = DriverTarget::Effect {
                effect_target,
                effect_index: *ei,
            };
            let effects = resolve_effects_ref(tab, project, active_layer, selection);
            if let Some(fx) = effects.and_then(|e| e.get(*ei))
                && let Some(di) = fx.drivers.as_ref()
                    .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                {
                    let driver = &fx.drivers.as_ref().unwrap()[di];
                    match cfg {
                        DriverConfigAction::BeatDiv(idx) => {
                            if let Some(new_div) = BeatDivision::from_button_index(*idx) {
                                let cmd = ChangeDriverBeatDivCommand::new(
                                    target, di, driver.beat_division, new_div,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Wave(idx) => {
                            if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                let cmd = ChangeDriverWaveformCommand::new(
                                    target, di, driver.waveform, new_wave,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Dot => {
                            if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                let cmd = ChangeDriverBeatDivCommand::new(
                                    target, di, driver.beat_division, new_div,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Triplet => {
                            if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                let cmd = ChangeDriverBeatDivCommand::new(
                                    target, di, driver.beat_division, new_div,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Reverse => {
                            let cmd = ToggleDriverReversedCommand::new(
                                target, di, driver.reversed, !driver.reversed,
                            );
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        }
                    }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvParamChanged(ei, pi, param, val) => {
            let tab = ui.inspector.last_effect_tab();
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
            };
            if let Some(ref et) = effect_type {
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => layer_idx.and_then(|idx|
                        project.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                    ),
                    InspectorTab::Clip | InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter_mut().find(|e|
                        e.target_effect_type == *et && e.param_index == *pi as i32
                    ) {
                        match param {
                            manifold_ui::EnvelopeParam::Attack => env.attack_beats = *val,
                            manifold_ui::EnvelopeParam::Decay => env.decay_beats = *val,
                            manifold_ui::EnvelopeParam::Sustain => env.sustain_level = *val,
                            manifold_ui::EnvelopeParam::Release => env.release_beats = *val,
                        }
                    }
            }
            // Sync to content thread
            if let Some(et) = effect_type {
                let param_i = *pi as i32;
                let p = *param;
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |proj| {
                    let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                        InspectorTab::Layer => proj.timeline.find_layer_by_id_mut(&layer_id)
                            .map(|(_, l)| l.envelopes_mut()),
                        InspectorTab::Clip | InspectorTab::Master => None,
                    };
                    if let Some(envs) = envs
                        && let Some(env) = envs.iter_mut().find(|e|
                            e.target_effect_type == et && e.param_index == param_i
                        ) {
                            match p {
                                manifold_ui::EnvelopeParam::Attack => env.attack_beats = v,
                                manifold_ui::EnvelopeParam::Decay => env.decay_beats = v,
                                manifold_ui::EnvelopeParam::Sustain => env.sustain_level = v,
                                manifold_ui::EnvelopeParam::Release => env.release_beats = v,
                            }
                        }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTrimChanged(ei, pi, min, max) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _) = resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut
                    && let Some(fx) = effects.get_mut(*ei)
                        && let Some(driver) = fx.drivers_mut().iter_mut()
                            .find(|d| d.param_index == *pi as i32)
                        {
                            driver.trim_min = *min;
                            driver.trim_max = *max;
                        }
                let fi = *ei;
                let param_i = *pi as i32;
                let mn = *min;
                let mx = *max;
                let layer_id = active_layer.clone().unwrap_or_default();
                let clip_id = selection.primary_selected_clip_id.clone();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    let effects: Option<&mut Vec<EffectInstance>> = match tab {
                        InspectorTab::Master => Some(&mut p.settings.master_effects),
                        InspectorTab::Layer => p.timeline.find_layer_by_id_mut(&layer_id).map(|(_, l)| l.effects_mut()),
                        InspectorTab::Clip => clip_id.as_ref().and_then(|cid| p.timeline.find_clip_by_id_mut(cid).map(|c| &mut c.effects)),
                    };
                    if let Some(effects) = effects
                        && let Some(fx) = effects.get_mut(fi)
                            && let Some(driver) = fx.drivers_mut().iter_mut()
                                .find(|d| d.param_index == param_i)
                            {
                                driver.trim_min = mn;
                                driver.trim_max = mx;
                            }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetChanged(ei, pi, norm) => {
            let tab = ui.inspector.last_effect_tab();
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
            };
            if let Some(ref et) = effect_type {
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => layer_idx.and_then(|idx|
                        project.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                    ),
                    InspectorTab::Clip | InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter_mut().find(|e|
                        e.target_effect_type == *et && e.param_index == *pi as i32
                    ) {
                        env.target_normalized = *norm;
                    }
            }
            if let Some(et) = effect_type {
                let param_i = *pi as i32;
                let n = *norm;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                        InspectorTab::Layer => p.timeline.find_layer_by_id_mut(&layer_id)
                            .map(|(_, l)| l.envelopes_mut()),
                        InspectorTab::Clip | InspectorTab::Master => None,
                    };
                    if let Some(envs) = envs
                        && let Some(env) = envs.iter_mut().find(|e|
                            e.target_effect_type == et && e.param_index == param_i
                        ) {
                            env.target_normalized = n;
                        }
                })));
            }
            DispatchResult::handled()
        }

        // ── Modulation undo: snapshot/commit ────────────────────────
        PanelAction::EffectTrimSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effects = resolve_effects_ref(tab, project, active_layer, selection);
            if let Some(fx) = effects.and_then(|e| e.get(*ei))
                && let Some(driver) = fx.drivers.as_ref()
                    .and_then(|ds| ds.iter().find(|d| d.param_index == *pi as i32))
                {
                    *trim_snapshot = Some((driver.trim_min, driver.trim_max));
                }
            DispatchResult::handled()
        }
        PanelAction::EffectTrimCommit(ei, pi) => {
            if let Some((old_min, old_max)) = trim_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                let effect_target = super::resolve_effect_target(tab, active_layer, project);
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*ei))
                    && let Some(di) = fx.drivers.as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                    {
                        let driver = &fx.drivers.as_ref().unwrap()[di];
                        let new_min = driver.trim_min;
                        let new_max = driver.trim_max;
                        if (old_min - new_min).abs() > f32::EPSILON || (old_max - new_max).abs() > f32::EPSILON {
                            let target = DriverTarget::Effect { effect_target, effect_index: *ei };
                            let cmd = ChangeTrimCommand::new(target, di, old_min, old_max, new_min, new_max);
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::EffectTargetSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
            };
            if let Some(et) = effect_type {
                let envs: Option<&[ParamEnvelope]> = match tab {
                    InspectorTab::Layer => layer_idx.and_then(|idx|
                        project.timeline.layers.get(idx)
                            .and_then(|l| l.envelopes.as_deref())
                    ),
                    InspectorTab::Clip => selection.primary_selected_clip_id.as_ref().and_then(|cid|
                        project.timeline.layers.iter()
                            .flat_map(|l| l.clips.iter())
                            .find(|c| c.id == *cid)
                            .and_then(|c| c.envelopes.as_deref())
                    ),
                    InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter().find(|e|
                        e.target_effect_type == et && e.param_index == *pi as i32
                    ) {
                        *target_snapshot = Some(env.target_normalized);
                    }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetCommit(ei, pi) => {
            if let Some(old_target) = target_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                let layer_idx = super::resolve_active_layer_index(active_layer, project);
                let effect_type = {
                    let effects = resolve_effects_ref(tab, project, active_layer, selection);
                    effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
                };
                if let Some(et) = effect_type {
                    match tab {
                        InspectorTab::Layer => {
                            if let Some(idx) = layer_idx
                                && let Some(layer) = project.timeline.layers.get(idx) {
                                    let layer_id = layer.layer_id.clone();
                                    let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                        && (old_target - env.target_normalized).abs() > f32::EPSILON {
                                            let cmd = ChangeLayerEnvelopeTargetCommand::new(layer_id, env_idx, old_target, env.target_normalized);
                                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                                        }
                                }
                        }
                        InspectorTab::Clip | InspectorTab::Master => {}
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
            };
            if let Some(et) = effect_type {
                let envs: Option<&[ParamEnvelope]> = match tab {
                    InspectorTab::Layer => layer_idx.and_then(|idx|
                        project.timeline.layers.get(idx)
                            .and_then(|l| l.envelopes.as_deref())
                    ),
                    InspectorTab::Clip => selection.primary_selected_clip_id.as_ref().and_then(|cid|
                        project.timeline.layers.iter()
                            .flat_map(|l| l.clips.iter())
                            .find(|c| c.id == *cid)
                            .and_then(|c| c.envelopes.as_deref())
                    ),
                    InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter().find(|e|
                        e.target_effect_type == et && e.param_index == *pi as i32
                    ) {
                        *adsr_snapshot = Some((
                            env.attack_beats,
                            env.decay_beats,
                            env.sustain_level,
                            env.release_beats,
                        ));
                    }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamCommit(ei, pi) => {
            if let Some((old_a, old_d, old_s, old_r)) = adsr_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                let layer_idx = super::resolve_active_layer_index(active_layer, project);
                let effect_type = {
                    let effects = resolve_effects_ref(tab, project, active_layer, selection);
                    effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type().clone())
                };
                if let Some(et) = effect_type {
                    match tab {
                        InspectorTab::Layer => {
                            if let Some(idx) = layer_idx
                                && let Some(layer) = project.timeline.layers.get(idx) {
                                    let layer_id = layer.layer_id.clone();
                                    let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                    {
                                        let (na, nd, ns, nr) = (env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats);
                                        if (old_a - na).abs() > f32::EPSILON || (old_d - nd).abs() > f32::EPSILON
                                            || (old_s - ns).abs() > f32::EPSILON || (old_r - nr).abs() > f32::EPSILON
                                        {
                                            let cmd = ChangeLayerEnvelopeADSRCommand::new(layer_id, env_idx, old_a, old_d, old_s, old_r, na, nd, ns, nr);
                                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                                        }
                                    }
                                }
                        }
                        InspectorTab::Clip | InspectorTab::Master => {}
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => {
            DispatchResult::handled()
        }
        PanelAction::BrowserSearchClicked => {
            DispatchResult::handled()
        }
        PanelAction::RemoveEffect(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let (effects_ref, target) = resolve_effects_read(tab, project, active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx) {
                    let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectReorder(from_idx, to_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let target = super::resolve_effect_target(tab, active_layer, project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }
        PanelAction::EffectReorderGroup(source_indices, target_idx) => {
            // Multi-select reorder: move a group of effects to the target position.
            let tab = ui.inspector.last_effect_tab();
            let target = super::resolve_effect_target(tab, active_layer, project);
            let (effects_mut, _target) = resolve_effects_mut(tab, project, active_layer, selection);
            if let Some(effects) = effects_mut {
                // Snapshot before
                let old_effects = effects.clone();

                // Remove selected effects in reverse order (preserving relative order)
                let mut moving: Vec<(usize, EffectInstance)> = Vec::new();
                let mut sorted_sources = source_indices.clone();
                sorted_sources.sort_unstable();
                for &idx in sorted_sources.iter().rev() {
                    if idx < effects.len() {
                        moving.push((idx, effects.remove(idx)));
                    }
                }
                moving.reverse(); // Restore original relative order

                // Compute adjusted insertion point (account for removed items before target)
                let removed_before = sorted_sources.iter().filter(|&&i| i < *target_idx).count();
                let insert_at = target_idx.saturating_sub(removed_before).min(effects.len());

                // Insert the group at the target position
                for (offset, (_, fx)) in moving.into_iter().enumerate() {
                    let pos = (insert_at + offset).min(effects.len());
                    effects.insert(pos, fx);
                }

                // Snapshot after and create undoable command
                let new_effects = effects.clone();
                let cmd = ReorderEffectGroupCommand::new(target, old_effects, new_effects);
                // State already applied — send for undo stack only (don't re-execute)
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }

        // ── Generator card actions ─────────────────────────────────
        PanelAction::GenStringParamClicked(_) => {
            // Intercepted in app_render.rs to open text input.
            DispatchResult::handled()
        }
        PanelAction::GenCollapseToggle => {
            if let Some(gp) = ui.inspector.gen_params_mut() {
                let new_val = !gp.is_collapsed();
                gp.set_collapsed(new_val);
            }
            DispatchResult::structural()
        }
        PanelAction::GenCardClicked => {
            // Select the generator card (blue highlight border), deselect effect cards
            if let Some(gp) = ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ui.tree, true);
            }
            // Deselect all effect cards
            ui.inspector.clear_effect_selection();
            DispatchResult::handled()
        }
        PanelAction::GenCardRightClicked => {
            // Handled by UIRoot::try_open_dropdown — should not reach dispatch
            DispatchResult::handled()
        }
        PanelAction::CopyGenerator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                && let Some(gp) = layer.gen_params()
            {
                ui.gen_clipboard.copy_from(gp);
            }
            DispatchResult::handled()
        }
        PanelAction::PasteGenerator => {
            if let Some(snapshot) = ui.gen_clipboard.get_paste_snapshot() {
                let layer_idx = super::resolve_active_layer_index(active_layer, project);
                if let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                {
                    let layer_id = layer.layer_id.clone();
                    let old_type = gp.generator_type().clone();
                    let old_params = gp.snapshot_params();
                    let old_drivers = gp.snapshot_drivers();
                    let old_envelopes = gp.snapshot_envelopes();

                    let cmd = PasteGeneratorCommand::new(
                        layer_id,
                        old_type, old_params, old_drivers, old_envelopes,
                        snapshot.generator_type, snapshot.param_values,
                        snapshot.drivers, snapshot.envelopes,
                    );
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }

        // ── Generator params ───────────────────────────────────────
        PanelAction::GenTypeClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::GenParamSnapshot(param_idx) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params() {
                        let val = gp.get_param_base(*param_idx);
                        *drag_snapshot = Some(val);
                        *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::GenParam {
                            layer_id: layer.layer_id.clone(),
                            param_idx: *param_idx,
                            value: val,
                        });
                    }
            DispatchResult::handled()
        }
        PanelAction::GenParamChanged(param_idx, val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = layer.gen_params_mut() {
                        gp.set_param_base(*param_idx, *val);
                    }
                if let Some(crate::app::ActiveInspectorDrag::GenParam { value, .. }) = active_inspector_drag {
                    *value = *val;
                }
                let pi = *param_idx;
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id)
                        && let Some(gp) = layer.gen_params_mut() {
                            gp.set_param_base(pi, v);
                        }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamCommit(param_idx) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(old_val) = drag_snapshot.take()
                && let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = layer.gen_params() {
                            let new_val = gp.get_param_base(*param_idx);
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let layer_id = layer.layer_id.clone();
                                let base = gp.base_param_values.as_ref()
                                    .unwrap_or(&gp.param_values);
                                let mut old_params = base.clone();
                                if *param_idx < old_params.len() {
                                    old_params[*param_idx] = old_val;
                                }
                                let new_params = base.clone();
                                let cmd = ChangeGeneratorParamsCommand::new(
                                    layer_id, old_params, new_params,
                                );
                                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                            }
                        }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::GenParamToggle(param_idx) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                    let layer_id = layer.layer_id.clone();
                    if let Some(gp) = layer.gen_params_mut() {
                        let old_val = gp.get_param_base(*param_idx);
                        let new_val = if old_val > 0.5 { 0.0 } else { 1.0 };
                        let base = gp.base_param_values.as_ref()
                            .unwrap_or(&gp.param_values);
                        let old_params = base.clone();
                        gp.set_param_base(*param_idx, new_val);
                        let new_params = gp.base_param_values.as_ref()
                            .unwrap_or(&gp.param_values).clone();
                        let cmd = ChangeGeneratorParamsCommand::new(
                            layer_id, old_params, new_params,
                        );
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            DispatchResult::handled()
        }
        PanelAction::GenParamRightClick(param_idx, default_val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                    let layer_id = layer.layer_id.clone();
                    if let Some(gp) = layer.gen_params_mut() {
                        let old = gp.get_param_base(*param_idx);
                        if (old - *default_val).abs() > f32::EPSILON {
                            let base = gp.base_param_values.as_ref()
                                .unwrap_or(&gp.param_values);
                            let old_params = base.clone();
                            gp.set_param_base(*param_idx, *default_val);
                            let new_params = gp.base_param_values.as_ref()
                                .unwrap_or(&gp.param_values).clone();
                            let cmd = ChangeGeneratorParamsCommand::new(
                                layer_id, old_params, new_params,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Gen modulation ─────────────────────────────────────────
        PanelAction::GenDriverToggle(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                let layer_id = active_layer.clone().unwrap_or_default();
                let target = DriverTarget::GeneratorParam { layer_id };
                if let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params() {
                        let driver_idx = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32));
                        if let Some(di) = driver_idx {
                            let old = gp.drivers.as_ref().unwrap()[di].enabled;
                            let cmd = ToggleDriverEnabledCommand::new(target, di, old, !old);
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        } else {
                            let driver = ParameterDriver {
                                param_index: *pi as i32,
                                beat_division: BeatDivision::Quarter,
                                waveform: DriverWaveform::Sine,
                                enabled: true,
                                phase: 0.0,
                                base_value: gp.param_values.get(*pi).copied().unwrap_or(0.0),
                                trim_min: 0.0,
                                trim_max: 1.0,
                                reversed: false,
                                is_paused_by_user: false,
                            };
                            let cmd = AddDriverCommand::new(target, driver);
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        }
                    }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvelopeToggle(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = layer.gen_params_mut() {
                        let envs = gp.envelopes.get_or_insert_with(Vec::new);
                        let env_idx = envs.iter().position(|e| e.param_index == *pi as i32);
                        if let Some(idx) = env_idx {
                            envs[idx].enabled = !envs[idx].enabled;
                        } else {
                            envs.push(ParamEnvelope {
                                target_effect_type: Default::default(),
                                param_index: *pi as i32,
                                enabled: true,
                                attack_beats: 0.0,
                                decay_beats: 0.0,
                                sustain_level: 0.0,
                                release_beats: 0.0,
                                target_normalized: 1.0,
                                current_level: 0.0,
                            });
                        }
                    }
            // Sync to content thread so the next snapshot doesn't overwrite
            let pi2 = *pi as i32;
            let layer_id = active_layer.clone().unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id)
                    && let Some(gp) = layer.gen_params_mut() {
                        let envs = gp.envelopes.get_or_insert_with(Vec::new);
                        let env_idx = envs.iter().position(|e| e.param_index == pi2);
                        if let Some(idx) = env_idx {
                            envs[idx].enabled = !envs[idx].enabled;
                        } else {
                            envs.push(ParamEnvelope {
                                target_effect_type: Default::default(),
                                param_index: pi2,
                                enabled: true,
                                attack_beats: 0.0,
                                decay_beats: 0.0,
                                sustain_level: 0.0,
                                release_beats: 0.0,
                                target_normalized: 1.0,
                                current_level: 0.0,
                            });
                        }
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::GenDriverConfig(pi, cfg) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                let layer_id = active_layer.clone().unwrap_or_default();
                let target = DriverTarget::GeneratorParam { layer_id };
                if let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                        && let Some(di) = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                        {
                            let driver = &gp.drivers.as_ref().unwrap()[di];
                            match cfg {
                                DriverConfigAction::BeatDiv(idx) => {
                                    if let Some(new_div) = BeatDivision::from_button_index(*idx) {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Wave(idx) => {
                                    if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                        let cmd = ChangeDriverWaveformCommand::new(target, di, driver.waveform, new_wave);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Dot => {
                                    if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Triplet => {
                                    if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Reverse => {
                                    let cmd = ToggleDriverReversedCommand::new(target, di, driver.reversed, !driver.reversed);
                                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                                }
                            }
                        }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvParamChanged(pi, param, val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = layer.gen_params_mut()
                        && let Some(envs) = &mut gp.envelopes
                            && let Some(env) = envs.iter_mut().find(|e| e.param_index == *pi as i32)
                            {
                                match param {
                                    manifold_ui::EnvelopeParam::Attack => env.attack_beats = *val,
                                    manifold_ui::EnvelopeParam::Decay => env.decay_beats = *val,
                                    manifold_ui::EnvelopeParam::Sustain => env.sustain_level = *val,
                                    manifold_ui::EnvelopeParam::Release => env.release_beats = *val,
                                }
                            }
                let param_i = *pi as i32;
                let p = *param;
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |proj| {
                    if let Some((_, layer)) = proj.timeline.find_layer_by_id_mut(&layer_id)
                        && let Some(gp) = layer.gen_params_mut()
                            && let Some(envs) = &mut gp.envelopes
                                && let Some(env) = envs.iter_mut().find(|e| e.param_index == param_i)
                                {
                                    match p {
                                        manifold_ui::EnvelopeParam::Attack => env.attack_beats = v,
                                        manifold_ui::EnvelopeParam::Decay => env.decay_beats = v,
                                        manifold_ui::EnvelopeParam::Sustain => env.sustain_level = v,
                                        manifold_ui::EnvelopeParam::Release => env.release_beats = v,
                                    }
                                }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::GenTrimChanged(pi, min, max) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = layer.gen_params_mut()
                        && let Some(drivers) = &mut gp.drivers
                            && let Some(driver) = drivers.iter_mut().find(|d| d.param_index == *pi as i32)
                            {
                                driver.trim_min = *min;
                                driver.trim_max = *max;
                            }
                let param_i = *pi as i32;
                let mn = *min;
                let mx = *max;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id)
                        && let Some(gp) = layer.gen_params_mut()
                            && let Some(drivers) = &mut gp.drivers
                                && let Some(driver) = drivers.iter_mut().find(|d| d.param_index == param_i)
                                {
                                    driver.trim_min = mn;
                                    driver.trim_max = mx;
                                }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::GenTargetChanged(pi, norm) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = layer.gen_params_mut()
                        && let Some(envs) = &mut gp.envelopes
                            && let Some(env) = envs.iter_mut().find(|e| e.param_index == *pi as i32)
                            {
                                env.target_normalized = *norm;
                            }
                let param_i = *pi as i32;
                let n = *norm;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id)
                        && let Some(gp) = layer.gen_params_mut()
                            && let Some(envs) = &mut gp.envelopes
                                && let Some(env) = envs.iter_mut().find(|e| e.param_index == param_i)
                                {
                                    env.target_normalized = n;
                                }
                })));
            }
            DispatchResult::handled()
        }

        // ── Generator modulation undo: snapshot/commit ──────────────
        PanelAction::GenTrimSnapshot(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                        && let Some(driver) = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().find(|d| d.param_index == *pi as i32))
                        {
                            *trim_snapshot = Some((driver.trim_min, driver.trim_max));
                        }
            DispatchResult::handled()
        }
        PanelAction::GenTrimCommit(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some((old_min, old_max)) = trim_snapshot.take()
                && let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = layer.gen_params()
                            && let Some(di) = gp.drivers.as_ref()
                                .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                            {
                                let driver = &gp.drivers.as_ref().unwrap()[di];
                                let new_min = driver.trim_min;
                                let new_max = driver.trim_max;
                                if (old_min - new_min).abs() > f32::EPSILON || (old_max - new_max).abs() > f32::EPSILON {
                                    let layer_id = active_layer.clone().unwrap_or_default();
                                    let target = DriverTarget::GeneratorParam { layer_id };
                                    let cmd = ChangeTrimCommand::new(target, di, old_min, old_max, new_min, new_max);
                                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                                }
                            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::GenTargetSnapshot(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                        && let Some(envs) = &gp.envelopes
                            && let Some(env) = envs.iter().find(|e| e.param_index == *pi as i32) {
                                *target_snapshot = Some(env.target_normalized);
                            }
            DispatchResult::handled()
        }
        PanelAction::GenTargetCommit(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(old_target) = target_snapshot.take()
                && let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = layer.gen_params()
                            && let Some(envs) = &gp.envelopes
                                && let Some(env_idx) = envs.iter().position(|e| e.param_index == *pi as i32) {
                                    let env = &envs[env_idx];
                                    if (old_target - env.target_normalized).abs() > f32::EPSILON {
                                        let layer_id = layer.layer_id.clone();
                                        let cmd = ChangeLayerEnvelopeTargetCommand::new(
                                            layer_id, env_idx, old_target, env.target_normalized,
                                        );
                                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                                    }
                                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::GenEnvParamSnapshot(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                        && let Some(envs) = &gp.envelopes
                            && let Some(env) = envs.iter().find(|e| e.param_index == *pi as i32) {
                                *adsr_snapshot = Some((env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats));
                            }
            DispatchResult::handled()
        }
        PanelAction::GenEnvParamCommit(pi) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some((old_a, old_d, old_s, old_r)) = adsr_snapshot.take()
                && let Some(layer_idx) = layer_idx
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = layer.gen_params()
                            && let Some(envs) = &gp.envelopes
                                && let Some(env_idx) = envs.iter().position(|e| e.param_index == *pi as i32) {
                                    let env = &envs[env_idx];
                                    let changed = (old_a - env.attack_beats).abs() > f32::EPSILON
                                        || (old_d - env.decay_beats).abs() > f32::EPSILON
                                        || (old_s - env.sustain_level).abs() > f32::EPSILON
                                        || (old_r - env.release_beats).abs() > f32::EPSILON;
                                    if changed {
                                        let layer_id = layer.layer_id.clone();
                                        let cmd = ChangeLayerEnvelopeADSRCommand::new(
                                            layer_id, env_idx,
                                            old_a, old_d, old_s, old_r,
                                            env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats,
                                        );
                                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                                    }
                                }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        PanelAction::AddEffect(tab, effect_type_idx) => {
            use manifold_core::effect_type_registry;
            use manifold_core::effects::EffectInstance;
            let available = effect_type_registry::available_effects();
            let Some(reg) = available.get(*effect_type_idx) else {
                return DispatchResult::handled();
            };
            let effect_type = reg.id.clone();
            let defaults = manifold_core::effect_definition_registry::get_defaults(&effect_type);
            let mut effect = EffectInstance::new(effect_type);
            effect.param_values = defaults;
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer => {
                    if let Some(idx) = layer_idx {
                        let layer_id = project.timeline.layers.get(idx).map(|l| l.layer_id.clone()).unwrap_or_default();
                        EffectTarget::Layer { layer_id }
                    } else {
                        return DispatchResult::handled();
                    }
                }
                InspectorTab::Clip => {
                    log::debug!("Add effect to clip (clip selection not yet implemented)");
                    return DispatchResult::handled();
                }
            };
            let insert_idx = match &target {
                EffectTarget::Master => project.settings.master_effects.len(),
                EffectTarget::Layer { layer_id } => {
                    project.timeline.layers.iter()
                        .find(|l| l.layer_id == *layer_id)
                        .and_then(|l| l.effects.as_ref())
                        .map(|e| e.len())
                        .unwrap_or(0)
                }
            };
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target, effect, insert_idx,
            );
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            DispatchResult::structural()
        }

        PanelAction::PasteEffects => {
            DispatchResult::handled()
        }

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        PanelAction::EffectParamLabelRightClick(..)
        | PanelAction::GenParamLabelRightClick(_) => DispatchResult::handled(),

        // ── Macro mapping ─────────────────────────────────────────
        PanelAction::MapEffectParamToMacro(tab, fx_idx, param_idx, macro_idx) => {
            use manifold_core::{MacroMapping, MacroMappingTarget, MacroCurve};
            let tab = *tab;
            let fx_idx = *fx_idx;
            let param_idx = *param_idx;
            let macro_idx = *macro_idx;

            // Resolve effect type and build mapping target
            let (effects_ref, _target) = resolve_effects_read(tab, project, active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(fx_idx) {
                    let effect_type = fx.effect_type().clone();

                    // Get param range from definition
                    let (min, max) = manifold_core::effect_definition_registry::try_get(&effect_type)
                        .and_then(|def| def.param_defs.get(param_idx))
                        .map(|pd| (pd.min, pd.max))
                        .unwrap_or((0.0, 1.0));

                    let mapping_target = match tab {
                        InspectorTab::Master => MacroMappingTarget::MasterEffect {
                            effect_type,
                            param_index: param_idx,
                        },
                        InspectorTab::Layer | InspectorTab::Clip => {
                            let layer_id = active_layer.clone()
                                .unwrap_or_else(|| project.timeline.layers.first()
                                    .map(|l| l.layer_id.clone())
                                    .unwrap_or_default());
                            MacroMappingTarget::LayerEffect {
                                layer_id,
                                effect_type,
                                param_index: param_idx,
                            }
                        }
                    };

                    let mapping = MacroMapping {
                        target: mapping_target,
                        range_min: min,
                        range_max: max,
                        curve: MacroCurve::Linear,
                    };

                    project.settings.macro_bank.slots[macro_idx].mappings.push(mapping.clone());
                    let mi = macro_idx;
                    ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[mi].mappings.push(mapping);
                    })));
                }
            DispatchResult::handled()
        }
        PanelAction::MapGenParamToMacro(param_idx, macro_idx) => {
            use manifold_core::{MacroMapping, MacroMappingTarget, MacroCurve};
            let param_idx = *param_idx;
            let macro_idx = *macro_idx;

            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                && let Some(gp) = layer.gen_params() {
                    let layer_id = layer.layer_id.clone();

                    // Get param range from definition
                    let (min, max) = manifold_core::generator_definition_registry::try_get(gp.generator_type())
                        .and_then(|def| def.param_defs.get(param_idx))
                        .map(|pd| (pd.min, pd.max))
                        .unwrap_or((0.0, 1.0));

                    let mapping = MacroMapping {
                        target: MacroMappingTarget::GenParam {
                            layer_id,
                            param_index: param_idx,
                        },
                        range_min: min,
                        range_max: max,
                        curve: MacroCurve::Linear,
                    };

                    project.settings.macro_bank.slots[macro_idx].mappings.push(mapping.clone());
                    let mi = macro_idx;
                    ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[mi].mappings.push(mapping);
                    })));
                }
            DispatchResult::handled()
        }
        // Label right-click consumed by try_open_dropdown — shouldn't reach here
        PanelAction::MacroLabelRightClick(_) => DispatchResult::handled(),

        PanelAction::UnmapMacro(macro_idx, mapping_idx) => {
            let macro_idx = *macro_idx;
            let mapping_idx = *mapping_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                let slot = &mut project.settings.macro_bank.slots[macro_idx];
                if mapping_idx < slot.mappings.len() {
                    slot.mappings.remove(mapping_idx);
                    ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                        let slot = &mut p.settings.macro_bank.slots[macro_idx];
                        if mapping_idx < slot.mappings.len() {
                            slot.mappings.remove(mapping_idx);
                        }
                    })));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClearMacroMappings(macro_idx) => {
            let macro_idx = *macro_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                project.settings.macro_bank.slots[macro_idx].mappings.clear();
                ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.macro_bank.slots[macro_idx].mappings.clear();
                })));
            }
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
