//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

use manifold_core::effects::{EffectInstance, ParameterDriver, ParamEnvelope};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
};
use manifold_editing::commands::effects::{
    ToggleEffectCommand, ChangeEffectParamCommand, RemoveEffectCommand, ReorderEffectCommand,
};
use manifold_editing::commands::envelopes::{
    ChangeEnvelopeADSRCommand, ChangeLayerEnvelopeADSRCommand,
    ChangeLayerEnvelopeTargetCommand, ChangeEnvelopeTargetNormalizedCommand,
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
use super::{resolve_effect_target, resolve_effects_read, resolve_effects_ref, resolve_effects_mut};

pub(super) fn dispatch_inspector(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    _content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<usize>,
    drag_snapshot: &mut Option<f32>,
    trim_snapshot: &mut Option<(f32, f32)>,
    adsr_snapshot: &mut Option<(f32, f32, f32, f32)>,
    target_snapshot: &mut Option<f32>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Master chrome ──────────────────────────────────────────
        PanelAction::MasterOpacitySnapshot => {
            *drag_snapshot = Some(project.settings.master_opacity);
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            project.settings.master_opacity = *val;
            let v = *val;
            let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                p.settings.master_opacity = v;
            })));
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                let new_val = project.settings.master_opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                    let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::MasterCollapseToggle => {
            ui.inspector.master_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::MasterExitPathClicked => {
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityRightClick => {
            let old = project.settings.master_opacity;
            if (old - 1.0).abs() > f32::EPSILON {
                project.settings.master_opacity = 1.0;
                let cmd = ChangeMasterOpacityCommand::new(old, 1.0);
                let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::handled()
        }

        // ── Layer chrome ───────────────────────────────────────────
        PanelAction::LayerOpacitySnapshot => {
            if let Some(idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get(idx) {
                    *drag_snapshot = Some(layer.opacity);
                }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityChanged(val) => {
            if let Some(idx) = *active_layer {
                if let Some(layer) = project.timeline.layers.get_mut(idx) {
                    layer.opacity = *val;
                }
                let v = *val;
                let i = idx;
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(layer) = p.timeline.layers.get_mut(i) {
                        layer.opacity = v;
                    }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(idx) = *active_layer
                    && let Some(layer) = project.timeline.layers.get(idx) {
                        let new_val = layer.opacity;
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let cmd = ChangeLayerOpacityCommand::new(idx, old_val, new_val);
                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
            DispatchResult::handled()
        }
        PanelAction::LayerChromeCollapseToggle => {
            ui.inspector.layer_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::LayerOpacityRightClick => {
            if let Some(idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get_mut(idx) {
                    let old = layer.opacity;
                    if (old - 1.0).abs() > f32::EPSILON {
                        layer.opacity = 1.0;
                        let cmd = ChangeLayerOpacityCommand::new(idx, old, 1.0);
                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                    }
                }
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
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipSlipSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    *drag_snapshot = Some(clip.in_point);
                }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.in_point = val.max(0.0);
                }
                let v = val.max(0.0);
                let cid = clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
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
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let cmd = SlipClipCommand::new(clip_id, old_val, new_val);
                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    *drag_snapshot = Some(clip.loop_duration_beats);
                }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.loop_duration_beats = val.max(0.0);
                }
                let v = val.max(0.0);
                let cid = clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
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
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let cmd = ChangeClipLoopCommand::new(
                                clip_id, is_looping, is_looping, old_val, new_val,
                            );
                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            DispatchResult::handled()
        }

        PanelAction::ClipSlipRightClick => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                    let old = clip.in_point;
                    if old.abs() > f32::EPSILON {
                        clip.in_point = 0.0;
                        let cmd = SlipClipCommand::new(clip_id, old, 0.0);
                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopRightClick => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                    let old_dur = clip.loop_duration_beats;
                    let full_dur = clip.duration_beats;
                    let is_looping = clip.is_looping;
                    if (old_dur - full_dur).abs() > f32::EPSILON {
                        clip.loop_duration_beats = full_dur;
                        let cmd = ChangeClipLoopCommand::new(
                            clip_id, is_looping, is_looping, old_dur, full_dur,
                        );
                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect operations ──────────────────────────────────────
        PanelAction::EffectToggle(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx) {
                    let old = fx.enabled;
                    let cmd = ToggleEffectCommand::new(target, *fx_idx, old, !old);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let (effects_mut, _target) = resolve_effects_mut(tab, project, *active_layer, selection);
            if let Some(effects) = effects_mut
                && let Some(fx) = effects.get_mut(*fx_idx) {
                    fx.collapsed = !fx.collapsed;
                }
            DispatchResult::structural()
        }
        PanelAction::EffectCardClicked(_) => {
            let tree = &mut ui.tree;
            let inspector = &mut ui.inspector;
            inspector.apply_selection_visuals(tree);
            DispatchResult::handled()
        }
        PanelAction::EffectParamRightClick(fx_idx, param_idx, default_val) => {
            let tab = ui.inspector.last_effect_tab();
            let (effects_mut, target) = resolve_effects_mut(tab, project, *active_layer, selection);
            if let Some(effects) = effects_mut
                && let Some(fx) = effects.get_mut(*fx_idx) {
                    let old = fx.get_base_param(*param_idx);
                    if (old - *default_val).abs() > f32::EPSILON {
                        fx.set_base_param(*param_idx, *default_val);
                        let cmd = ChangeEffectParamCommand::new(
                            target, *fx_idx, *param_idx, old, *default_val,
                        );
                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            DispatchResult::handled()
        }
        PanelAction::EffectParamSnapshot(fx_idx, param_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let effects = resolve_effects_ref(tab, project, *active_layer, selection);
            if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                *drag_snapshot = Some(fx.get_base_param(*param_idx));
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamChanged(fx_idx, param_idx, val) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut
                    && let Some(fx) = effects.get_mut(*fx_idx) {
                        fx.set_base_param(*param_idx, *val);
                    }
                let fi = *fx_idx;
                let pi = *param_idx;
                let v = *val;
                let layer_idx = *active_layer;
                let clip_id = selection.primary_selected_clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    let effects: Option<&mut Vec<EffectInstance>> = match tab {
                        InspectorTab::Master => Some(&mut p.settings.master_effects),
                        InspectorTab::Layer => layer_idx.and_then(|i| p.timeline.layers.get_mut(i)).map(|l| l.effects_mut()),
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
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                    let new_val = fx.get_base_param(*param_idx);
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let target = resolve_effect_target(tab, *active_layer);
                        let cmd = ChangeEffectParamCommand::new(
                            target, *fx_idx, *param_idx, old_val, new_val,
                        );
                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::EffectDriverToggle(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_target = resolve_effect_target(tab, *active_layer);
            let (effects_ref, _) = resolve_effects_read(tab, project, *active_layer, selection);
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
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
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
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                    }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvelopeToggle(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
            };
            if let Some(et) = effect_type {
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => {
                        active_layer.and_then(|idx| {
                            project.timeline.layers.get_mut(idx)
                                .map(|l| l.envelopes_mut())
                        })
                    }
                    InspectorTab::Clip => {
                        selection.primary_selected_clip_id.as_ref().and_then(|clip_id| {
                            project.timeline.layers.iter_mut()
                                .flat_map(|l| l.clips.iter_mut())
                                .find(|c| c.id == *clip_id)
                                .map(|c| c.envelopes_mut())
                        })
                    }
                    InspectorTab::Master => None,
                };
                if let Some(envs) = envs {
                    let env_idx = envs.iter().position(|e|
                        e.target_effect_type == et && e.param_index == *pi as i32
                    );
                    if let Some(idx) = env_idx {
                        envs[idx].enabled = !envs[idx].enabled;
                    } else {
                        envs.push(ParamEnvelope {
                            target_effect_type: et,
                            param_index: *pi as i32,
                            enabled: true,
                            attack_beats: 0.25,
                            decay_beats: 0.25,
                            sustain_level: 1.0,
                            release_beats: 0.25,
                            target_normalized: 1.0,
                            current_level: 0.0,
                        });
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectDriverConfig(ei, pi, cfg) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_target = resolve_effect_target(tab, *active_layer);
            let target = DriverTarget::Effect {
                effect_target,
                effect_index: *ei,
            };
            let effects = resolve_effects_ref(tab, project, *active_layer, selection);
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
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Wave(idx) => {
                            if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                let cmd = ChangeDriverWaveformCommand::new(
                                    target, di, driver.waveform, new_wave,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Dot => {
                            if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                let cmd = ChangeDriverBeatDivCommand::new(
                                    target, di, driver.beat_division, new_div,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Triplet => {
                            if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                let cmd = ChangeDriverBeatDivCommand::new(
                                    target, di, driver.beat_division, new_div,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                            }
                        }
                        DriverConfigAction::Reverse => {
                            let cmd = ToggleDriverReversedCommand::new(
                                target, di, driver.reversed, !driver.reversed,
                            );
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                        }
                    }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvParamChanged(ei, pi, param, val) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
            };
            if let Some(et) = effect_type {
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => active_layer.and_then(|idx|
                        project.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                    ),
                    InspectorTab::Clip => selection.primary_selected_clip_id.as_ref().and_then(|cid|
                        project.timeline.layers.iter_mut()
                            .flat_map(|l| l.clips.iter_mut())
                            .find(|c| c.id == *cid)
                            .map(|c| c.envelopes_mut())
                    ),
                    InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter_mut().find(|e|
                        e.target_effect_type == et && e.param_index == *pi as i32
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
                let layer_idx = *active_layer;
                let clip_id = selection.primary_selected_clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |proj| {
                    let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                        InspectorTab::Layer => layer_idx.and_then(|idx|
                            proj.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                        ),
                        InspectorTab::Clip => clip_id.as_ref().and_then(|cid|
                            proj.timeline.layers.iter_mut()
                                .flat_map(|l| l.clips.iter_mut())
                                .find(|c| c.id == *cid)
                                .map(|c| c.envelopes_mut())
                        ),
                        InspectorTab::Master => None,
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
                let (effects_mut, _) = resolve_effects_mut(tab, project, *active_layer, selection);
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
                let layer_idx = *active_layer;
                let clip_id = selection.primary_selected_clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    let effects: Option<&mut Vec<EffectInstance>> = match tab {
                        InspectorTab::Master => Some(&mut p.settings.master_effects),
                        InspectorTab::Layer => layer_idx.and_then(|i| p.timeline.layers.get_mut(i)).map(|l| l.effects_mut()),
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
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
            };
            if let Some(et) = effect_type {
                let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                    InspectorTab::Layer => active_layer.and_then(|idx|
                        project.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                    ),
                    InspectorTab::Clip => selection.primary_selected_clip_id.as_ref().and_then(|cid|
                        project.timeline.layers.iter_mut()
                            .flat_map(|l| l.clips.iter_mut())
                            .find(|c| c.id == *cid)
                            .map(|c| c.envelopes_mut())
                    ),
                    InspectorTab::Master => None,
                };
                if let Some(envs) = envs
                    && let Some(env) = envs.iter_mut().find(|e|
                        e.target_effect_type == et && e.param_index == *pi as i32
                    ) {
                        env.target_normalized = *norm;
                    }
            }
            if let Some(et) = effect_type {
                let param_i = *pi as i32;
                let n = *norm;
                let layer_idx = *active_layer;
                let clip_id = selection.primary_selected_clip_id.clone();
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    let envs: Option<&mut Vec<ParamEnvelope>> = match tab {
                        InspectorTab::Layer => layer_idx.and_then(|idx|
                            p.timeline.layers.get_mut(idx).map(|l| l.envelopes_mut())
                        ),
                        InspectorTab::Clip => clip_id.as_ref().and_then(|cid|
                            p.timeline.layers.iter_mut()
                                .flat_map(|l| l.clips.iter_mut())
                                .find(|c| c.id == *cid)
                                .map(|c| c.envelopes_mut())
                        ),
                        InspectorTab::Master => None,
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
            let effects = resolve_effects_ref(tab, project, *active_layer, selection);
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
                let effect_target = resolve_effect_target(tab, *active_layer);
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
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
                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
            };
            if let Some(et) = effect_type {
                let envs: Option<&[ParamEnvelope]> = match tab {
                    InspectorTab::Layer => active_layer.and_then(|idx|
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
                let effect_type = {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
                };
                if let Some(et) = effect_type {
                    match tab {
                        InspectorTab::Layer => {
                            if let Some(idx) = *active_layer
                                && let Some(layer) = project.timeline.layers.get(idx) {
                                    let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                        && (old_target - env.target_normalized).abs() > f32::EPSILON {
                                            let cmd = ChangeLayerEnvelopeTargetCommand::new(idx, env_idx, old_target, env.target_normalized);
                                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                        }
                                }
                        }
                        InspectorTab::Clip => {
                            if let Some(clip_id) = &selection.primary_selected_clip_id {
                                let clip = project.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter())
                                    .find(|c| c.id == *clip_id);
                                if let Some(clip) = clip {
                                    let envs = clip.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                        && (old_target - env.target_normalized).abs() > f32::EPSILON {
                                            let cmd = ChangeEnvelopeTargetNormalizedCommand::new(clip_id.clone(), env_idx, old_target, env.target_normalized);
                                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                        }
                                }
                            }
                        }
                        InspectorTab::Master => {}
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            let effect_type = {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
            };
            if let Some(et) = effect_type {
                let envs: Option<&[ParamEnvelope]> = match tab {
                    InspectorTab::Layer => active_layer.and_then(|idx|
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
                let effect_type = {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
                };
                if let Some(et) = effect_type {
                    match tab {
                        InspectorTab::Layer => {
                            if let Some(idx) = *active_layer
                                && let Some(layer) = project.timeline.layers.get(idx) {
                                    let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                    {
                                        let (na, nd, ns, nr) = (env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats);
                                        if (old_a - na).abs() > f32::EPSILON || (old_d - nd).abs() > f32::EPSILON
                                            || (old_s - ns).abs() > f32::EPSILON || (old_r - nr).abs() > f32::EPSILON
                                        {
                                            let cmd = ChangeLayerEnvelopeADSRCommand::new(idx, env_idx, old_a, old_d, old_s, old_r, na, nd, ns, nr);
                                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                        }
                                    }
                                }
                        }
                        InspectorTab::Clip => {
                            if let Some(clip_id) = &selection.primary_selected_clip_id {
                                let clip = project.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter())
                                    .find(|c| c.id == *clip_id);
                                if let Some(clip) = clip {
                                    let envs = clip.envelopes.as_deref().unwrap_or(&[]);
                                    if let Some((env_idx, env)) = envs.iter().enumerate()
                                        .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                    {
                                        let (na, nd, ns, nr) = (env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats);
                                        if (old_a - na).abs() > f32::EPSILON || (old_d - nd).abs() > f32::EPSILON
                                            || (old_s - ns).abs() > f32::EPSILON || (old_r - nr).abs() > f32::EPSILON
                                        {
                                            let cmd = ChangeEnvelopeADSRCommand::new(clip_id.clone(), env_idx, old_a, old_d, old_s, old_r, na, nd, ns, nr);
                                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                        }
                                    }
                                }
                            }
                        }
                        InspectorTab::Master => {}
                    }
                }
            }
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
            let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx) {
                    let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                }
            DispatchResult::structural()
        }
        PanelAction::EffectReorder(from_idx, to_idx) => {
            let tab = ui.inspector.last_effect_tab();
            let target = resolve_effect_target(tab, *active_layer);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            DispatchResult::structural()
        }

        // ── Generator params ───────────────────────────────────────
        PanelAction::GenTypeClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::GenParamSnapshot(param_idx) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params {
                        *drag_snapshot = Some(gp.get_param_base(*param_idx));
                    }
            DispatchResult::handled()
        }
        PanelAction::GenParamChanged(param_idx, val) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params {
                        gp.set_param_base(*param_idx, *val);
                    }
                let pi = *param_idx;
                let v = *val;
                let li = layer_idx;
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(layer) = p.timeline.layers.get_mut(li)
                        && let Some(gp) = &mut layer.gen_params {
                            gp.set_param_base(pi, v);
                        }
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamCommit(param_idx) => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(layer_idx) = *active_layer
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = &layer.gen_params {
                            let new_val = gp.get_param_base(*param_idx);
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let base = gp.base_param_values.as_ref()
                                    .unwrap_or(&gp.param_values);
                                let mut old_params = base.clone();
                                if *param_idx < old_params.len() {
                                    old_params[*param_idx] = old_val;
                                }
                                let new_params = base.clone();
                                let cmd = ChangeGeneratorParamsCommand::new(
                                    layer_idx, old_params, new_params,
                                );
                                let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                            }
                        }
            DispatchResult::handled()
        }
        PanelAction::GenParamToggle(param_idx) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params {
                        let cur = gp.get_param_base(*param_idx);
                        gp.set_param_base(*param_idx, if cur > 0.5 { 0.0 } else { 1.0 });
                    }
            DispatchResult::handled()
        }
        PanelAction::GenParamRightClick(param_idx, default_val) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params {
                        let old = gp.get_param_base(*param_idx);
                        if (old - *default_val).abs() > f32::EPSILON {
                            let base = gp.base_param_values.as_ref()
                                .unwrap_or(&gp.param_values);
                            let old_params = base.clone();
                            gp.set_param_base(*param_idx, *default_val);
                            let new_params = gp.base_param_values.as_ref()
                                .unwrap_or(&gp.param_values).clone();
                            let cmd = ChangeGeneratorParamsCommand::new(
                                layer_idx, old_params, new_params,
                            );
                            let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
            DispatchResult::handled()
        }

        // ── Gen modulation ─────────────────────────────────────────
        PanelAction::GenDriverToggle(pi) => {
            if let Some(layer_idx) = *active_layer {
                let target = DriverTarget::GeneratorParam { layer_index: layer_idx };
                if let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params {
                        let driver_idx = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32));
                        if let Some(di) = driver_idx {
                            let old = gp.drivers.as_ref().unwrap()[di].enabled;
                            let cmd = ToggleDriverEnabledCommand::new(target, di, old, !old);
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
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
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                        }
                    }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvelopeToggle(pi) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params {
                        let envs = gp.envelopes.get_or_insert_with(Vec::new);
                        let env_idx = envs.iter().position(|e| e.param_index == *pi as i32);
                        if let Some(idx) = env_idx {
                            envs[idx].enabled = !envs[idx].enabled;
                        } else {
                            envs.push(ParamEnvelope {
                                target_effect_type: Default::default(),
                                param_index: *pi as i32,
                                enabled: true,
                                attack_beats: 0.25,
                                decay_beats: 0.25,
                                sustain_level: 1.0,
                                release_beats: 0.25,
                                target_normalized: 1.0,
                                current_level: 0.0,
                            });
                        }
                    }
            DispatchResult::structural()
        }
        PanelAction::GenDriverConfig(pi, cfg) => {
            if let Some(layer_idx) = *active_layer {
                let target = DriverTarget::GeneratorParam { layer_index: layer_idx };
                if let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params
                        && let Some(di) = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                        {
                            let driver = &gp.drivers.as_ref().unwrap()[di];
                            match cfg {
                                DriverConfigAction::BeatDiv(idx) => {
                                    if let Some(new_div) = BeatDivision::from_button_index(*idx) {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Wave(idx) => {
                                    if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                        let cmd = ChangeDriverWaveformCommand::new(target, di, driver.waveform, new_wave);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Dot => {
                                    if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Triplet => {
                                    if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                        let cmd = ChangeDriverBeatDivCommand::new(target, di, driver.beat_division, new_div);
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                                    }
                                }
                                DriverConfigAction::Reverse => {
                                    let cmd = ToggleDriverReversedCommand::new(target, di, driver.reversed, !driver.reversed);
                                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                                }
                            }
                        }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvParamChanged(pi, param, val) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params
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
                let li = layer_idx;
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |proj| {
                    if let Some(layer) = proj.timeline.layers.get_mut(li)
                        && let Some(gp) = &mut layer.gen_params
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
            if let Some(layer_idx) = *active_layer {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params
                        && let Some(drivers) = &mut gp.drivers
                            && let Some(driver) = drivers.iter_mut().find(|d| d.param_index == *pi as i32)
                            {
                                driver.trim_min = *min;
                                driver.trim_max = *max;
                            }
                let param_i = *pi as i32;
                let mn = *min;
                let mx = *max;
                let li = layer_idx;
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(layer) = p.timeline.layers.get_mut(li)
                        && let Some(gp) = &mut layer.gen_params
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
            if let Some(layer_idx) = *active_layer {
                if let Some(layer) = project.timeline.layers.get_mut(layer_idx)
                    && let Some(gp) = &mut layer.gen_params
                        && let Some(envs) = &mut gp.envelopes
                            && let Some(env) = envs.iter_mut().find(|e| e.param_index == *pi as i32)
                            {
                                env.target_normalized = *norm;
                            }
                let param_i = *pi as i32;
                let n = *norm;
                let li = layer_idx;
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(layer) = p.timeline.layers.get_mut(li)
                        && let Some(gp) = &mut layer.gen_params
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
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params
                        && let Some(driver) = gp.drivers.as_ref()
                            .and_then(|ds| ds.iter().find(|d| d.param_index == *pi as i32))
                        {
                            *trim_snapshot = Some((driver.trim_min, driver.trim_max));
                        }
            DispatchResult::handled()
        }
        PanelAction::GenTrimCommit(pi) => {
            if let Some((old_min, old_max)) = trim_snapshot.take()
                && let Some(layer_idx) = *active_layer
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = &layer.gen_params
                            && let Some(di) = gp.drivers.as_ref()
                                .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                            {
                                let driver = &gp.drivers.as_ref().unwrap()[di];
                                let new_min = driver.trim_min;
                                let new_max = driver.trim_max;
                                if (old_min - new_min).abs() > f32::EPSILON || (old_max - new_max).abs() > f32::EPSILON {
                                    let target = DriverTarget::GeneratorParam { layer_index: layer_idx };
                                    let cmd = ChangeTrimCommand::new(target, di, old_min, old_max, new_min, new_max);
                                    let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                }
                            }
            DispatchResult::handled()
        }
        PanelAction::GenTargetSnapshot(pi) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params
                        && let Some(envs) = &gp.envelopes
                            && let Some(env) = envs.iter().find(|e| e.param_index == *pi as i32) {
                                *target_snapshot = Some(env.target_normalized);
                            }
            DispatchResult::handled()
        }
        PanelAction::GenTargetCommit(pi) => {
            if let Some(old_target) = target_snapshot.take()
                && let Some(layer_idx) = *active_layer
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = &layer.gen_params
                            && let Some(envs) = &gp.envelopes
                                && let Some(env_idx) = envs.iter().position(|e| e.param_index == *pi as i32) {
                                    let env = &envs[env_idx];
                                    if (old_target - env.target_normalized).abs() > f32::EPSILON {
                                        let cmd = ChangeLayerEnvelopeTargetCommand::new(
                                            layer_idx, env_idx, old_target, env.target_normalized,
                                        );
                                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                    }
                                }
            DispatchResult::handled()
        }
        PanelAction::GenEnvParamSnapshot(pi) => {
            if let Some(layer_idx) = *active_layer
                && let Some(layer) = project.timeline.layers.get(layer_idx)
                    && let Some(gp) = &layer.gen_params
                        && let Some(envs) = &gp.envelopes
                            && let Some(env) = envs.iter().find(|e| e.param_index == *pi as i32) {
                                *adsr_snapshot = Some((env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats));
                            }
            DispatchResult::handled()
        }
        PanelAction::GenEnvParamCommit(pi) => {
            if let Some((old_a, old_d, old_s, old_r)) = adsr_snapshot.take()
                && let Some(layer_idx) = *active_layer
                    && let Some(layer) = project.timeline.layers.get(layer_idx)
                        && let Some(gp) = &layer.gen_params
                            && let Some(envs) = &gp.envelopes
                                && let Some(env_idx) = envs.iter().position(|e| e.param_index == *pi as i32) {
                                    let env = &envs[env_idx];
                                    let changed = (old_a - env.attack_beats).abs() > f32::EPSILON
                                        || (old_d - env.decay_beats).abs() > f32::EPSILON
                                        || (old_s - env.sustain_level).abs() > f32::EPSILON
                                        || (old_r - env.release_beats).abs() > f32::EPSILON;
                                    if changed {
                                        let cmd = ChangeLayerEnvelopeADSRCommand::new(
                                            layer_idx, env_idx,
                                            old_a, old_d, old_s, old_r,
                                            env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats,
                                        );
                                        let _ = content_tx.try_send(ContentCommand::Execute(Box::new(cmd)));
                                    }
                                }
            DispatchResult::handled()
        }

        PanelAction::AddEffect(tab, effect_type_idx) => {
            use manifold_core::types::EffectType;
            use manifold_core::effects::EffectInstance;
            let Some(effect_type) = EffectType::from_discriminant(*effect_type_idx as i32) else {
                return DispatchResult::handled();
            };
            let defaults = manifold_core::effect_definition_registry::get_defaults(effect_type);
            let effect = EffectInstance {
                effect_type,
                enabled: true,
                collapsed: false,
                param_values: defaults,
                base_param_values: None,
                drivers: None,
                group_id: None,
                legacy_param0: None,
                legacy_param1: None,
                legacy_param2: None,
                legacy_param3: None,
            };
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer => {
                    if let Some(idx) = *active_layer {
                        EffectTarget::Layer { layer_index: idx }
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
                EffectTarget::Layer { layer_index } => {
                    project.timeline.layers.get(*layer_index)
                        .and_then(|l| l.effects.as_ref())
                        .map(|e| e.len())
                        .unwrap_or(0)
                }
                _ => 0,
            };
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target, effect, insert_idx,
            );
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            DispatchResult::structural()
        }

        PanelAction::PasteEffects => {
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
