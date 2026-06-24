//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

use manifold_core::effects::{PresetInstance, ParamEnvelope, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, LayerId, Seconds};
use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_core::audio_clip_detection::DetectionConfig;
use manifold_editing::commands::clip::{
    ChangeClipLoopCommand, ChangeClipRecordedBpmCommand, SlipClipCommand,
};
use manifold_editing::commands::clip_detection::SetClipDetectionConfigCommand;
use manifold_editing::commands::drivers::{
    AddDriverCommand, ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand, ChangeTrimCommand,
    SetDriverFreePeriodCommand, ToggleDriverEnabledCommand, ToggleDriverReversedCommand,
};
use manifold_editing::commands::audio_mod::{
    AddAudioModCommand, RemoveAudioModCommand, SetAudioModShapeCommand, SetAudioModSourceCommand,
    ToggleAudioModEnabledCommand,
};
use manifold_editing::commands::audio_setup::{
    AddAudioSendCommand, RemoveAudioSendCommand, RenameAudioSendCommand, SetAudioCrossoversCommand,
    SetAudioInputDeviceCommand, SetAudioSendChannelsCommand, SetAudioSendFloorCommand,
    SetAudioSendGainCommand, SetAudioSendTriggersCommand,
};
use manifold_editing::commands::effect_target::{DriverTarget, EffectTarget};
use manifold_editing::commands::effects::{
    ChangeGraphParamCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    ToggleEffectCommand,
};
use manifold_editing::commands::envelopes::{
    ChangeEnvelopeDecayCommand, ChangeEnvelopeTargetCommand,
};
use manifold_editing::commands::settings::{
    ChangeLayerOpacityCommand, ChangeLedBrightnessCommand, ChangeMacroCommand,
    ChangeMasterOpacityCommand, PasteGeneratorCommand,
};
use manifold_ui::{
    AudioShapeParam, DriverConfigAction, GraphParamTarget, InspectorTab, PanelAction, TrimKind,
};

use super::DispatchResult;
use super::{resolve_effects_mut, resolve_effects_read};
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

/// Apply `edit` to the envelope matched by `param_id` on `target`, in both the
/// local UI project and the content thread (the next snapshot must not stomp
/// the live tweak). Edits the existing envelope only — no create. The unified
/// non-undoable live-drag envelope helper, for effects and generators alike:
/// the kind fork lives entirely inside `with_preset_graph_mut`.
fn graph_env_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParamEnvelope) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(env) = inst
            .envelopes
            .as_mut()
            .and_then(|es| es.iter_mut().find(|e| e.param_id == param_id))
        {
            edit(env);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(env) = inst
                    .envelopes
                    .as_mut()
                    .and_then(|es| es.iter_mut().find(|e| e.param_id == pid))
                {
                    edit2(env);
                }
            });
        })),
    );
}

/// Driver twin of [`graph_env_dual_edit`]: apply `edit` to the driver matched
/// by `param_id` on `target`, locally and on the content thread.
fn graph_driver_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParameterDriver) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(driver) = inst
            .drivers
            .as_mut()
            .and_then(|ds| ds.iter_mut().find(|d| d.param_id == param_id))
        {
            edit(driver);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(driver) = inst
                    .drivers
                    .as_mut()
                    .and_then(|ds| ds.iter_mut().find(|d| d.param_id == pid))
                {
                    edit2(driver);
                }
            });
        })),
    );
}

/// Live dual-edit of one audio modulation, mirroring [`graph_driver_dual_edit`]
/// for the green trim-handle drag: applies `edit` to the matching
/// `ParameterAudioMod` on both the UI-thread project mirror and (via
/// `MutateProjectLive`) the content thread, so the handle tracks under the
/// cursor without an undo entry per frame (the undo command lands on commit).
fn graph_audio_mod_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut manifold_core::audio_mod::ParameterAudioMod) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(m) = inst
            .audio_mods
            .as_mut()
            .and_then(|ms| ms.iter_mut().find(|a| a.param_id == param_id))
        {
            edit(m);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(m) = inst
                    .audio_mods
                    .as_mut()
                    .and_then(|ms| ms.iter_mut().find(|a| a.param_id == pid))
                {
                    edit2(m);
                }
            });
        })),
    );
}

/// Resolve a `GraphParamTarget` (the card's effect-row index or generator
/// marker) to a stable `GraphTarget`, for routing through
/// `Project::with_preset_graph_mut` and the GraphTarget-keyed editing
/// commands. The single resolver behind every collapsed param/modulation
/// dispatch arm: effects address by stable `EffectId` (editor-aware via
/// `resolve_effect_id`), generators by the active layer's `LayerId`.
fn resolve_graph_target(
    gpt: &GraphParamTarget,
    editor_target: Option<&manifold_core::GraphTarget>,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    project: &Project,
) -> Option<manifold_core::GraphTarget> {
    match gpt {
        GraphParamTarget::Effect(idx) => {
            super::resolve_effect_id(editor_target, tab, active_layer, selection, project, *idx)
                .map(manifold_core::GraphTarget::Effect)
        }
        GraphParamTarget::Generator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project)?;
            let lid = project.timeline.layers.get(layer_idx)?.layer_id.clone();
            Some(manifold_core::GraphTarget::Generator(lid))
        }
    }
}

/// The `AbletonMappingTarget` for a resolved param `target` on `tab`, so the
/// Ableton trim/invert arms route through the shared
/// `Project::ableton_param_mappings_mut` locate-fork (effects addressed by
/// `effect_type` within master/layer — first match — generators by layer).
/// `None` for the clip tab (no clip-scoped Ableton mappings).
fn ableton_mapping_target(
    target: &manifold_core::GraphTarget,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    project: &Project,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::ableton_mapping::AbletonMappingTarget> {
    use manifold_core::ableton_mapping::AbletonMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => {
            let effect_type = project.find_effect_by_id(eid)?.effect_type().clone();
            match tab {
                InspectorTab::Master => Some(T::MasterEffect {
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Layer | InspectorTab::Group => Some(T::LayerEffect {
                    layer_id: active_layer.clone()?,
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Clip => None,
            }
        }
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `MacroMappingTarget` for a resolved param `target` (macro twin of
/// [`ableton_mapping_target`]). Effects address by stable `EffectId`, which
/// reaches master / layer / clip effects directly — so the macro drives the
/// exact instance the user mapped, even with two same-type effects on a layer.
fn macro_mapping_target(
    target: &manifold_core::GraphTarget,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::MacroMappingTarget> {
    use manifold_core::MacroMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => Some(T::Effect {
            effect_id: eid.clone(),
            param_id: param_id.clone(),
        }),
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `(min, max)` range for `param_id` on `inst`, graph-authority-first:
/// the per-instance graph `preset_metadata` (where graph-backed presets —
/// notably generators — carry their authoritative ranges) takes precedence
/// over the registry, falling back to [`PresetInstance::resolve_param`] (and
/// `(0.0, 1.0)` if unresolved). Unifies the old per-kind range lookups.
fn resolve_param_range(inst: &PresetInstance, param_id: &str) -> (f32, f32) {
    if let Some(spec) = inst
        .graph
        .as_ref()
        .and_then(|g| g.preset_metadata.as_ref())
        .and_then(|m| m.params.iter().find(|p| p.id == param_id))
    {
        return (spec.min, spec.max);
    }
    inst.resolve_param(param_id)
        .map(|r| (r.min, r.max))
        .unwrap_or((0.0, 1.0))
}

/// The preset graph def to fork or export for a resolved `target`: the
/// per-instance diverged graph if the instance carries one, else the catalog
/// canonical def from the loaded preset view. Paired with the current preset
/// id (the export filename stem). One path for both kinds — the fork / export
/// dispatch arms resolve a `GraphTarget` (effect or generator) and hand it
/// here, so make-unique / export behave identically on either card.
fn preset_source_def(
    target: &manifold_core::GraphTarget,
    project: &Project,
) -> Option<(
    manifold_core::effect_graph_def::EffectGraphDef,
    manifold_core::PresetTypeId,
)> {
    let inst = project.preset_instance(target)?;
    let preset_id = inst.effect_type().clone();
    let mut def = inst.graph.clone().or_else(|| {
        manifold_renderer::node_graph::loaded_preset_view_by_id(&preset_id)
            .map(|v| v.canonical_def.clone())
    })?;
    // Snapshot the card's current slider values into the def's defaults so Make
    // Unique / Export freeze the configured look rather than the stock template.
    // The live values stay on the instance (the performance surface); this only
    // makes the def reproduce them on a later add/import/load.
    inst.snapshot_values_into_def(&mut def);
    Some((def, preset_id))
}

/// Apply a project-level audio-setup command locally and forward it to the
/// content thread. The shared tail for every Audio Setup action (they all
/// mutate `project.audio_setup` and need no target resolution).
fn audio_setup_command(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    mut cmd: Box<dyn manifold_editing::command::Command + Send>,
) -> DispatchResult {
    cmd.execute(project);
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Execute(cmd),
    );
    DispatchResult::structural()
}

/// Apply an edit to a clip's `DetectionConfig` and re-place its triggers from the
/// cached analysis. Reads the current config (or default), mutates it, records the
/// config change (local + content thread), then asks the orchestrator to re-plan
/// — instant, no backend run. See `docs/AUDIO_CLIP_DETECTION_DESIGN.md`.
fn apply_detection_edit(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    clip_id: &manifold_core::ClipId,
    mutate: impl FnOnce(&mut DetectionConfig),
) {
    use crate::content_command::ContentCommand;
    let mut config = project
        .timeline
        .find_clip_by_id(clip_id)
        .and_then(|c| c.audio_detection.as_ref())
        .map(|d| d.config.clone())
        .unwrap_or_default();
    mutate(&mut config);

    let mut cmd: Box<dyn manifold_editing::command::Command + Send> =
        Box::new(SetClipDetectionConfigCommand::new(clip_id.clone(), config));
    cmd.execute(project);
    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
    ContentCommand::send(content_tx, ContentCommand::ReplanClip(clip_id.clone()));
}

pub(super) fn dispatch_inspector(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    drag_snapshot: &mut Option<f32>,
    trim_snapshot: &mut Option<(f32, f32)>,
    target_snapshot: &mut Option<f32>,
    decay_snapshot: &mut Option<f32>,
    audio_shape_snapshot: &mut Option<manifold_core::audio_mod::AudioModShape>,
    audio_crossover_snapshot: &mut Option<(f32, f32)>,
    active_inspector_drag: &mut Option<crate::app::ActiveInspectorDrag>,
    editor_target: Option<&manifold_core::GraphTarget>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;

    // The single-effect VALUE / expose / mapping arms address their instance by
    // stable `EffectId` via `super::resolve_effect_id(editor_target, …)` and
    // ignore `effective_tab` / `active_layer` when the editor supplies an
    // identity. The MODULATION arms (drivers, layer-stored envelopes, trims,
    // envelope targets) still resolve positionally through `(tab, active_layer)`
    // + the effect's row index, so they need a tab/layer that points at the
    // editor's WATCHED effect — not the main window's selection — when a card
    // action is dispatched from the editor. `editor_dispatch_context` expresses
    // the editor's identity in those positional terms (Master / its Layer /
    // Clip), and is byte-identical to the inspector's own context on the
    // perform path (`editor_target == None`).
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(
        editor_target,
        project,
        ui.inspector.last_effect_tab(),
        active_layer,
    );
    // Shadow the &mut param: no arm in this function mutates `active_layer`, so an
    // immutable shadow is sound and routes every downstream resolver through the
    // effective layer.
    let active_layer: &Option<LayerId> = &effective_active_layer;

    match action {
        // ── Macros panel collapse ─────────────────────────────────
        PanelAction::MacrosCollapseToggle => {
            ui.inspector.macros_panel_mut().toggle_collapsed();
            DispatchResult::structural()
        }

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
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    manifold_core::macro_bank::MacroBank::apply_macro(p, idx, val);
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::MacroCommit(idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let idx = *idx;
                if idx < manifold_core::macro_bank::MACRO_COUNT {
                    let new_val = project.settings.macro_bank.slots[idx].value;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMacroCommand::new(idx, old_val, new_val);
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
        PanelAction::MacroLabelRename(_) => DispatchResult::handled(),

        // ── Master chrome ──────────────────────────────────────────
        PanelAction::MasterOpacitySnapshot => {
            *drag_snapshot = Some(project.settings.master_opacity);
            *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::MasterOpacity(
                project.settings.master_opacity,
            ));
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            project.settings.master_opacity = *val;
            if let Some(crate::app::ActiveInspectorDrag::MasterOpacity(v)) = active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.settings.master_opacity = v;
                })),
            );
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
        // ── Audio-layer gain slider (layer header) ─────────────────
        PanelAction::AudioGainSnapshot(idx) => {
            *drag_snapshot = project
                .timeline
                .layers
                .get(*idx)
                .map(|l| l.audio_gain_db);
            DispatchResult::handled()
        }
        PanelAction::AudioGainChanged(idx, db) => {
            let db = *db;
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.audio_gain_db = db;
                let id = layer.layer_id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        if let Some((_, l)) = p.timeline.find_layer_by_id_mut(&id) {
                            l.audio_gain_db = db;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::AudioGainCommit(idx) => {
            if let Some(old_db) = drag_snapshot.take()
                && let Some(layer) = project.timeline.layers.get(*idx)
            {
                let new_db = layer.audio_gain_db;
                if (old_db - new_db).abs() > f32::EPSILON {
                    let cmd = manifold_editing::commands::layer::SetLayerAudioGainCommand::new(
                        layer.layer_id.clone(),
                        old_db,
                        new_db,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
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

        // ── LED enabled toggle ───────────────────────────────────
        PanelAction::LedEnabledToggle => {
            let new_enabled = !content_state.led_enabled;
            // Persist the new ON/OFF state in project settings so the LED
            // pipeline auto-initialises on project load.
            project.settings.led_enabled = new_enabled;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_enabled = new_enabled;
                })),
            );
            if new_enabled {
                let settings = manifold_led::LedSettings {
                    enabled: true,
                    ..Default::default()
                };
                ContentCommand::send(
                    content_tx,
                    ContentCommand::InitLedOutput(Box::new(settings)),
                );
            } else {
                ContentCommand::send(content_tx, ContentCommand::ShutdownLedOutput);
            }
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
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_brightness = v;
                })),
            );
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
                && let Some(layer) = project.timeline.layers.get(idx)
            {
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
                if let Some(crate::app::ActiveInspectorDrag::LayerOpacity { value, .. }) =
                    active_inspector_drag
                {
                    *value = *val;
                }
                let v = *val;
                let layer_id = active_layer.clone().unwrap_or_default();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                            layer.opacity = v;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityCommit => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(old_val) = drag_snapshot.take()
                && let Some(idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(idx)
            {
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
                && let Some(layer) = project.timeline.layers.get_mut(idx)
            {
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
        PanelAction::ClipBpmClicked => DispatchResult::handled(),
        PanelAction::ClipWarpToggled => {
            // Audio warp toggle: off (recorded_bpm 0, native speed) ⇄ on (lock to
            // the project tempo as a sensible default). One BPM command, which
            // also rescales the clip's timeline length to hold the audio span.
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                let project_bpm = project.settings.bpm.0;
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let old_bpm = clip.recorded_bpm;
                    let new_bpm = if old_bpm > 0.0 { 0.0 } else { project_bpm };
                    let cmd = ChangeClipRecordedBpmCommand::new(clip_id, old_bpm, new_bpm);
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectClicked => {
            // Per-clip detection: analyze the selected audio clip's file and place
            // its triggers. The orchestrator (content thread) does the work and the
            // result syncs back; status shows via the global percussion status.
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                ContentCommand::send(content_tx, ContentCommand::DetectClip(clip_id.clone()));
            }
            DispatchResult::handled()
        }
        PanelAction::ClipClearTriggersClicked => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::ClearClipTriggers(clip_id.clone()),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ClipDetectInstrumentToggled(idx) => {
            let idx = *idx;
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.enabled = !inst.enabled;
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSensitivityChanged(idx, value) => {
            let (idx, value) = (*idx, *value);
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.sensitivity = value.clamp(0.0, 1.0);
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectOnsetChanged(ms) => {
            let secs = manifold_core::Seconds((*ms / 1000.0) as f64);
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    c.onset_compensation = secs;
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetQuantize(step) => {
            let step = *step;
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| match step {
                    Some(beats) => {
                        c.quantize_on = true;
                        c.quantize_step_beats = beats;
                    }
                    None => c.quantize_on = false,
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetLayer(idx, layer) => {
            let (idx, layer) = (*idx, layer.clone());
            if let Some(clip_id) = selection.primary_selected_clip_id.clone() {
                apply_detection_edit(project, content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.target_layer = layer;
                    }
                });
            }
            DispatchResult::structural()
        }
        // The open actions are consumed by UIRoot::try_open_dropdown before
        // dispatch; these arms are defensive no-ops.
        PanelAction::ClipDetectQuantizeClicked | PanelAction::ClipDetectLayerClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let old_loop = clip.is_looping;
                    let old_dur = clip.loop_duration_beats;
                    let cmd =
                        ChangeClipLoopCommand::new(clip_id, old_loop, !old_loop, old_dur, old_dur);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipSlipSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id)
            {
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
                if let Some(crate::app::ActiveInspectorDrag::ClipSlip { value, .. }) =
                    active_inspector_drag
                {
                    *value = val.max(0.0);
                }
                let v = Seconds::from_f32(val.max(0.0));
                let cid = clip_id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        if let Some(clip) = p.timeline.find_clip_by_id_mut(&cid) {
                            clip.in_point = v;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipCommit => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(clip_id) = &selection.primary_selected_clip_id
            {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let new_val = clip.in_point;
                    if (old_val - new_val.as_f32()).abs() > f32::EPSILON {
                        let cmd =
                            SlipClipCommand::new(clip_id, Seconds::from_f32(old_val), new_val);
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id
                && let Some(clip) = project.timeline.find_clip_by_id(clip_id)
            {
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
                if let Some(crate::app::ActiveInspectorDrag::ClipLoop { value, .. }) =
                    active_inspector_drag
                {
                    *value = val.max(0.0);
                }
                let v = Beats::from_f32(val.max(0.0));
                let cid = clip_id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        if let Some(clip) = p.timeline.find_clip_by_id_mut(&cid) {
                            clip.loop_duration_beats = v;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopCommit => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(clip_id) = &selection.primary_selected_clip_id
            {
                let clip_id = clip_id.clone();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    let new_val = clip.loop_duration_beats;
                    let is_looping = clip.is_looping;
                    if (old_val - new_val.as_f32()).abs() > f32::EPSILON {
                        let cmd = ChangeClipLoopCommand::new(
                            clip_id,
                            is_looping,
                            is_looping,
                            Beats::from_f32(old_val),
                            new_val,
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
            let tab = effective_tab;
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            // New state = inverse of the clicked card, applied to every selected.
            let new_enabled = super::resolve_effect_id(
                editor_target,
                tab,
                active_layer,
                selection,
                project,
                *fx_idx,
            )
            .and_then(|eid| project.find_effect_by_id(&eid).map(|fx| !fx.enabled))
            .unwrap_or(true);
            // Resolve every affected card to its stable id + current state. The
            // editor toggles its single watched effect (id wins over `idx`); the
            // inspector resolves each selected index against its own context.
            let targets: Vec<(manifold_core::EffectId, bool)> = indices
                .iter()
                .filter_map(|&idx| {
                    let eid = super::resolve_effect_id(
                        editor_target,
                        tab,
                        active_layer,
                        selection,
                        project,
                        idx,
                    )?;
                    let enabled = project.find_effect_by_id(&eid)?.enabled;
                    Some((eid, enabled))
                })
                .collect();
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for (eid, old_enabled) in &targets {
                if *old_enabled != new_enabled {
                    commands.push(Box::new(ToggleEffectCommand::new(
                        eid.clone(),
                        *old_enabled,
                        new_enabled,
                    )));
                }
            }
            // Apply locally for immediate visual feedback.
            for (eid, _) in &targets {
                if let Some(fx) = project.find_effect_by_id_mut(eid) {
                    fx.enabled = new_enabled;
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
            let tab = effective_tab;
            let selected = ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            let new_collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, project, active_layer, selection);
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
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for &idx in &indices_owned {
                            if let Some(fx) = effects.get_mut(idx) {
                                fx.collapsed = new_collapsed;
                            }
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        PanelAction::SetAllCardsCollapsed { collapsed } => {
            // Collapse/expand every effect card in the active column at once.
            // Mirrors EffectCollapseToggle's two-write pattern (snapshot now,
            // MutateProject so the content thread's snapshot doesn't overwrite).
            let tab = effective_tab;
            let collapsed = *collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, project, active_layer, selection);
                if let Some(effects) = effects_mut {
                    for fx in effects.iter_mut() {
                        fx.collapsed = collapsed;
                    }
                }
            }
            let target = super::resolve_effect_target(tab, active_layer, project);
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for fx in effects.iter_mut() {
                            fx.collapsed = collapsed;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        PanelAction::ModConfigTabChanged => {
            // The card already switched its own active-tab UI state in
            // handle_click; this just forces a rebuild so the drawer repaints
            // with the newly-selected config. No model mutation.
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
        PanelAction::ParamRightClick(gpt, param_id, default_val) => {
            // Reset-to-default for both kinds. Routing through
            // with_preset_graph_mut is what fixed the generator snap-back
            // (#5): the committed command now resolves and writes the slot
            // for a generator exactly as for an effect.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let changed = project
                    .with_preset_graph_mut(&target, |inst| {
                        let slot = inst.param_id_to_value_index(param_id.as_ref())?;
                        let old = inst.get_base_param(slot);
                        if (old - *default_val).abs() > f32::EPSILON {
                            inst.set_base_param(slot, *default_val);
                            Some(old)
                        } else {
                            None
                        }
                    })
                    .flatten();
                if let Some(old) = changed {
                    let cmd = ChangeGraphParamCommand::new(
                        target,
                        param_id.clone(),
                        old,
                        *default_val,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::ParamSnapshot(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.param_id_to_value_index(param_id.as_ref())
                            .map(|slot| inst.get_base_param(slot))
                    })
                    .flatten();
                if let Some(val) = val {
                    *drag_snapshot = Some(val);
                    *active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Param {
                        target,
                        param_id: param_id.clone(),
                        value: val,
                    });
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ParamChanged(gpt, param_id, val) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                project.with_preset_graph_mut(&target, |inst| {
                    if let Some(slot) = inst.param_id_to_value_index(param_id.as_ref()) {
                        inst.set_base_param(slot, *val);
                    }
                });
                if let Some(crate::app::ActiveInspectorDrag::Param { value, .. }) =
                    active_inspector_drag
                {
                    *value = *val;
                }
                let pid = param_id.clone();
                let v = *val;
                let t = target.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        p.with_preset_graph_mut(&t, |inst| {
                            if let Some(slot) = inst.param_id_to_value_index(pid.as_ref()) {
                                inst.set_base_param(slot, v);
                            }
                        });
                    })),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ParamCommit(gpt, param_id) => {
            if let Some(old_val) = drag_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let new_val = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.param_id_to_value_index(param_id.as_ref())
                            .map(|slot| inst.get_base_param(slot))
                    })
                    .flatten();
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    let cmd = ChangeGraphParamCommand::new(
                        target,
                        param_id.clone(),
                        old_val,
                        new_val,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::DriverToggle(gpt, param_id) => {
            let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            else {
                return DispatchResult::structural();
            };
            // Read the driver state off the SAME instance the command targets,
            // by target — never an ambient row index — so an editor-card driver
            // edit can't split (command -> watched instance, di -> another).
            let Some((existing, base_value)) = project.with_preset_graph_mut(&target, |inst| {
                let existing = inst
                    .drivers
                    .as_ref()
                    .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                    .map(|di| (di, inst.drivers.as_ref().unwrap()[di].enabled));
                let base_value = inst
                    .param_id_to_value_index(param_id.as_ref())
                    .and_then(|slot| inst.param_values.get(slot))
                    .map(|p| p.value)
                    .unwrap_or(0.0);
                (existing, base_value)
            }) else {
                return DispatchResult::structural();
            };
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some((di, old)) = existing {
                    Box::new(ToggleDriverEnabledCommand::new(driver_target, di, old, !old))
                } else {
                    let driver = ParameterDriver {
                        param_id: param_id.clone(),
                        beat_division: BeatDivision::Quarter,
                        waveform: DriverWaveform::Sine,
                        enabled: true,
                        phase: 0.0,
                        base_value,
                        trim_min: 0.0,
                        trim_max: 1.0,
                        reversed: false,
                        free_period_beats: None,
                        legacy_param_index: None,
                        is_paused_by_user: false,
                    };
                    Box::new(AddDriverCommand::new(driver_target, driver))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModToggle(gpt, param_id) => {
            let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            else {
                return DispatchResult::structural();
            };
            // Existing mod's enabled state, read off the resolved instance.
            let existing = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.enabled)
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = existing {
                    Box::new(ToggleAudioModEnabledCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        !old,
                    ))
                } else {
                    // Arm: assign the project's first audio send. No sends → inert
                    // (the audio button stays a no-op until the Audio Setup defines one).
                    let Some(send_id) = project.audio_setup.sends.first().map(|s| s.id.clone())
                    else {
                        return DispatchResult::structural();
                    };
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id,
                        manifold_core::AudioFeature::default(),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModSetSource(gpt, param_id, send_id, feature) => {
            let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            else {
                return DispatchResult::structural();
            };
            let old_source = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.source.clone())
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let new_source = manifold_core::audio_mod::AudioModSource {
                send_id: send_id.clone(),
                feature: crate::ui_translate::audio_feature_to_core(*feature),
            };
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = old_source {
                    Box::new(SetAudioModSourceCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        new_source,
                    ))
                } else {
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id.clone(),
                        crate::ui_translate::audio_feature_to_core(*feature),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModRemove(gpt, param_id) => {
            let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            else {
                return DispatchResult::structural();
            };
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveAudioModCommand::new(driver_target, param_id.clone()));
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioModSetInvert(gpt, param_id) => {
            // Flip the mod's invert flag in one undo step. Reads the current
            // shape, flips `invert`, commits old→new via the shape command.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.invert = !old_shape.invert;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::AudioModSetRateOfChange(gpt, param_id) => {
            // Flip the mod's rate-of-change flag in one undo step — same shape
            // path as invert: read the current shape, flip `rate_of_change`,
            // commit old→new via the shape command.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let old_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.rate_of_change = !old_shape.rate_of_change;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::AudioModShapeSnapshot(gpt, param_id) => {
            // Capture the pre-drag shape so the commit can record one undo step.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                *audio_shape_snapshot = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModShapeParamChanged(gpt, param_id, which, value) => {
            // Live edit (no undo entry per frame) — the handle tracks the cursor.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let which = *which;
                let v = *value;
                graph_audio_mod_dual_edit(project, content_tx, &target, param_id.clone(), move |m| {
                    match which {
                        AudioShapeParam::Sensitivity => m.shape.sensitivity = v,
                        AudioShapeParam::Attack => m.shape.attack_ms = v,
                        AudioShapeParam::Release => m.shape.release_ms = v,
                    }
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioModShapeCommit(gpt, param_id) => {
            // One undo step: snapshot (old) → current shape (new) via the shape command.
            if let Some(old_shape) = audio_shape_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let new_shape = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(new_shape) = new_shape
                    && new_shape != old_shape
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModShapeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_shape,
                            new_shape,
                        ));
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }

        // ── Audio Setup (project-level send routing) ──────────────
        PanelAction::AudioSetDevice(device) => {
            let old = project.audio_setup.device.clone();
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioInputDeviceCommand::new(
                    old,
                    device.as_ref().map(crate::ui_translate::audio_device_ref_to_core),
                )),
            )
        }
        PanelAction::AudioAddSend => {
            let send = manifold_core::audio_setup::AudioSend::new(format!(
                "Audio {}",
                project.audio_setup.sends.len() + 1
            ));
            audio_setup_command(project, content_tx, Box::new(AddAudioSendCommand::new(send)))
        }
        PanelAction::AudioRemoveSend(id) => audio_setup_command(
            project,
            content_tx,
            Box::new(RemoveAudioSendCommand::new(id.clone())),
        ),
        PanelAction::AudioRenameSend(id, label) => {
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.label.clone())
                .unwrap_or_default();
            audio_setup_command(
                project,
                content_tx,
                Box::new(RenameAudioSendCommand::new(id.clone(), old, label.clone())),
            )
        }
        PanelAction::AudioSetSendChannels(id, ch) => {
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.channels.clone())
                .unwrap_or_default();
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendChannelsCommand::new(id.clone(), old, ch.clone())),
            )
        }
        PanelAction::AudioSendStereoToggle(id) => {
            // Mono ↔ stereo: stereo routes the primary channel and its pair
            // partner; mono keeps just the primary. Out-of-range channels are
            // ignored by the analysis downmix, so no device-bound clamp here.
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.channels.clone())
                .unwrap_or_default();
            let first = old.first().copied().unwrap_or(0);
            let new = if old.len() >= 2 { vec![first] } else { vec![first, first + 1] };
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendChannelsCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioSendGainStep(id, delta_db) => {
            // The project is the source of truth: read current gain, apply the
            // delta, clamp to a sensible trim range, commit old→new. Capture
            // restart is avoided — the worker reads gain live (AudioModRuntime).
            const GAIN_MIN_DB: f32 = -24.0;
            const GAIN_MAX_DB: f32 = 24.0;
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.gain_db)
                .unwrap_or(0.0);
            let new = (old + delta_db).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendGainCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioSendFloorStep(id, delta_db) => {
            // Pre-analysis squelch (dB). Off is a sentinel below the usable range:
            // stepping up from off engages the gate at its bottom; stepping below
            // the bottom turns it back off. Applied live (AudioModRuntime).
            const FLOOR_MIN_DB: f32 = -100.0;
            const FLOOR_MAX_DB: f32 = -6.0;
            let off = manifold_core::audio_setup::FLOOR_DB_OFF;
            let old = project
                .audio_setup
                .find_send(id)
                .map(|s| s.floor_db)
                .unwrap_or(off);
            let new = if old <= off {
                if *delta_db > 0.0 { FLOOR_MIN_DB } else { off }
            } else {
                let v = old + *delta_db;
                if v < FLOOR_MIN_DB { off } else { v.min(FLOOR_MAX_DB) }
            };
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendFloorCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioTriggerToggled(id, band) => {
            let Some(send) = project.audio_setup.find_send(id) else {
                return DispatchResult::structural();
            };
            let old = send.triggers.clone();
            let new = send.triggers_with_route(crate::ui_translate::audio_band_to_core(*band), |r| r.enabled = !r.enabled);
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendTriggersCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioTriggerSensitivityStep(id, band, delta) => {
            let Some(send) = project.audio_setup.find_send(id) else {
                return DispatchResult::structural();
            };
            let old = send.triggers.clone();
            let new = send
                .triggers_with_route(crate::ui_translate::audio_band_to_core(*band), |r| r.sensitivity = (r.sensitivity + *delta).clamp(0.0, 1.0));
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendTriggersCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioTriggerLengthStep(id, band, factor) => {
            let Some(send) = project.audio_setup.find_send(id) else {
                return DispatchResult::structural();
            };
            let old = send.triggers.clone();
            // Multiplicative (halve/double), clamped to a musical 1/4..16 beat range.
            let new = send.triggers_with_route(crate::ui_translate::audio_band_to_core(*band), |r| {
                let beats = (r.one_shot_beats.as_f32() * *factor).clamp(0.25, 16.0);
                r.one_shot_beats = manifold_core::units::Beats::from_f32(beats);
            });
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendTriggersCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioTriggerSetLayer(id, band, layer) => {
            let Some(send) = project.audio_setup.find_send(id) else {
                return DispatchResult::structural();
            };
            let old = send.triggers.clone();
            let new = send.triggers_with_route(crate::ui_translate::audio_band_to_core(*band), |r| r.target_layer = layer.clone());
            audio_setup_command(
                project,
                content_tx,
                Box::new(SetAudioSendTriggersCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioTriggerLayerClicked(_, _) => {
            // The layer dropdown is opened by UIRoot::try_open_dropdown before
            // dispatch; reaching here (e.g. no candidate layers) is a no-op.
            DispatchResult::structural()
        }
        PanelAction::AudioCrossoverDragBegin => {
            // Snapshot the pre-drag crossovers so the commit records one undo step.
            *audio_crossover_snapshot =
                Some((project.audio_setup.low_hz, project.audio_setup.mid_hz));
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverChanged(band, hz) => {
            // Live edit (no per-frame undo): clamp the dragged line against the
            // other and the band edges, then apply to the local project and the
            // content thread so the divider + analysis bands track the cursor.
            let dragging_low = matches!(band, manifold_ui::BandDivider::Low);
            let (cur_low, cur_mid) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            let (low, mid) = if dragging_low {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(*hz, cur_mid, true)
            } else {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(cur_low, *hz, false)
            };
            project.audio_setup.low_hz = low;
            project.audio_setup.mid_hz = mid;
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.audio_setup.low_hz = low;
                    p.audio_setup.mid_hz = mid;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverCommit => {
            // One undo step: snapshot (old) → current crossovers (new).
            if let Some(old) = audio_crossover_snapshot.take() {
                let new = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
                if new != old {
                    return audio_setup_command(
                        project,
                        content_tx,
                        Box::new(SetAudioCrossoversCommand::new(old, new)),
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EnvelopeToggle(gpt, param_id) => {
            // Envelope-home unification: the envelope rides on the resolved
            // instance (keyed by param_id) for effects and generators alike.
            // Toggle the existing one's `enabled`, or create a fresh enabled
            // envelope. Effects are clip-timed, so only layer effects get
            // envelopes (master/clip have no trigger — the button is inert
            // there); generators are layer-scoped, always permitted.
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let env_allowed = match &target {
                    manifold_core::GraphTarget::Generator(_) => true,
                    manifold_core::GraphTarget::Effect(_) => {
                        matches!(effective_tab, InspectorTab::Layer)
                    }
                };
                if env_allowed {
                    let pid = param_id.clone();
                    let toggle = move |p: &mut Project| {
                        p.with_preset_graph_mut(&target, |inst| {
                            let envs = inst.envelopes.get_or_insert_with(Vec::new);
                            if let Some(idx) = envs.iter().position(|e| e.param_id == pid) {
                                envs[idx].enabled = !envs[idx].enabled;
                            } else {
                                envs.push(ParamEnvelope::new(pid.clone()));
                            }
                        });
                    };
                    toggle(project);
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::MutateProject(Box::new(toggle)),
                    );
                }
            }
            DispatchResult::structural()
        }
        PanelAction::DriverConfig(gpt, param_id, cfg) => {
            let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            else {
                return DispatchResult::structural();
            };
            let driver_target = DriverTarget::from(&target);
            // Read the driver's current config off the same instance the
            // command targets (by GraphTarget), so an editor-card edit can't
            // split command vs row index.
            let info = project
                .with_preset_graph_mut(&target, |inst| {
                    inst.drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                        .map(|di| {
                            let d = &inst.drivers.as_ref().unwrap()[di];
                            (di, d.beat_division, d.waveform, d.reversed, d.free_period_beats)
                        })
                })
                .flatten();
            if let Some((di, beat_division, waveform, reversed, free)) = info {
                type BoxedCmd = Box<dyn manifold_editing::command::Command + Send>;
                // The feel segment sets the division's modifier from its base; a
                // base without a dotted/triplet variant (e.g. 1/32) keeps the base.
                let base = beat_division.base_division();
                let cmd: Option<BoxedCmd> = match cfg {
                    DriverConfigAction::BeatDiv(idx) => BeatDivision::from_button_index(*idx)
                        .map(|new_div| {
                            Box::new(ChangeDriverBeatDivCommand::new(
                                driver_target,
                                di,
                                beat_division,
                                new_div,
                                free,
                            )) as BoxedCmd
                        }),
                    DriverConfigAction::Wave(idx) => DriverWaveform::from_index(*idx).map(|new_wave| {
                        Box::new(ChangeDriverWaveformCommand::new(
                            driver_target,
                            di,
                            waveform,
                            new_wave,
                        )) as BoxedCmd
                    }),
                    DriverConfigAction::Straight => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base,
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Dotted => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_dotted().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Triplet => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_triplet().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Invert => Some(Box::new(ToggleDriverReversedCommand::new(
                        driver_target,
                        di,
                        reversed,
                        !reversed,
                    )) as BoxedCmd),
                    DriverConfigAction::SetFreePeriod(p) => {
                        Some(Box::new(SetDriverFreePeriodCommand::new(
                            driver_target,
                            di,
                            free,
                            Some(*p),
                        )) as BoxedCmd)
                    }
                };
                if let Some(mut boxed) = cmd {
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        // Live trim-range edit for driver / Ableton / audio — one arm,
        // `TrimKind` selects the backing store. Each kind keeps the exact
        // edit it had before the unification (driver dual-edit, audio dual-edit,
        // Ableton mapping local + content-sync).
        PanelAction::TrimChanged(kind, gpt, param_id, min, max) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let mn = *min;
                let mx = *max;
                match kind {
                    TrimKind::Driver => {
                        graph_driver_dual_edit(project, content_tx, &target, param_id.clone(), move |d| {
                            d.trim_min = mn;
                            d.trim_max = mx;
                        });
                    }
                    TrimKind::Audio => {
                        graph_audio_mod_dual_edit(project, content_tx, &target, param_id.clone(), move |m| {
                            m.shape.range_min = mn;
                            m.shape.range_max = mx;
                        });
                    }
                    TrimKind::Ableton => {
                        if let Some(mapping_target) =
                            ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
                        {
                            // Local edit + content sync both route through the
                            // shared locate-fork, so they can't split (effects
                            // locate by effect_type — first match — on both
                            // sides now).
                            if let Some(ms) = project
                                .ableton_param_mappings_mut(&mapping_target)
                                .and_then(|opt| opt.as_mut())
                                && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                            {
                                m.range_min = mn;
                                m.range_max = mx;
                            }
                            let mt = mapping_target.clone();
                            let pid = param_id.clone();
                            ContentCommand::send(
                                content_tx,
                                ContentCommand::MutateProject(Box::new(move |p| {
                                    if let Some(ms) =
                                        p.ableton_param_mappings_mut(&mt).and_then(|opt| opt.as_mut())
                                        && let Some(m) = ms.iter_mut().find(|m| m.param_id == pid)
                                    {
                                        m.range_min = mn;
                                        m.range_max = mx;
                                    }
                                })),
                            );
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TargetChanged(gpt, param_id, norm) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let n = *norm;
                graph_env_dual_edit(project, content_tx, &target, param_id.clone(), move |env| {
                    env.target_normalized = n;
                });
            }
            DispatchResult::handled()
        }
        PanelAction::EnvDecayChanged(gpt, param_id, decay) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let d = *decay;
                graph_env_dual_edit(project, content_tx, &target, param_id.clone(), move |env| {
                    env.decay_beats = d;
                });
            }
            DispatchResult::handled()
        }

        // ── Modulation undo: snapshot/commit ────────────────────────
        // Snapshot the kind's pre-drag range into the shared `trim_snapshot`.
        // Only one trim handle drags at a time, so one slot serves all kinds.
        PanelAction::TrimSnapshot(kind, gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let range = match kind {
                    TrimKind::Driver => project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.drivers
                                .as_ref()
                                .and_then(|ds| ds.iter().find(|d| d.param_id == *param_id))
                                .map(|d| (d.trim_min, d.trim_max))
                        })
                        .flatten(),
                    TrimKind::Audio => project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.audio_mods
                                .as_ref()
                                .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                .map(|m| (m.shape.range_min, m.shape.range_max))
                        })
                        .flatten(),
                    TrimKind::Ableton => {
                        ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
                            .and_then(|mt| {
                                project
                                    .ableton_param_mappings(&mt)
                                    .and_then(|opt| opt.as_ref())
                                    .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                                    .map(|m| (m.range_min, m.range_max))
                            })
                    }
                };
                if let Some(range) = range {
                    *trim_snapshot = Some(range);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TrimCommit(kind, gpt, param_id) => {
            match kind {
                TrimKind::Driver => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                    {
                        let info = project
                            .with_preset_graph_mut(&target, |inst| {
                                inst.drivers
                                    .as_ref()
                                    .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                                    .map(|di| {
                                        let d = &inst.drivers.as_ref().unwrap()[di];
                                        (di, d.trim_min, d.trim_max)
                                    })
                            })
                            .flatten();
                        if let Some((di, new_min, new_max)) = info
                            && ((old_min - new_min).abs() > f32::EPSILON
                                || (old_max - new_max).abs() > f32::EPSILON)
                        {
                            let cmd = ChangeTrimCommand::new(
                                DriverTarget::from(&target),
                                di,
                                old_min,
                                old_max,
                                new_min,
                                new_max,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
                TrimKind::Audio => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                    {
                        let new_shape = project
                            .with_preset_graph_mut(&target, |inst| {
                                inst.audio_mods
                                    .as_ref()
                                    .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                    .map(|m| m.shape)
                            })
                            .flatten();
                        if let Some(new_shape) = new_shape
                            && ((old_min - new_shape.range_min).abs() > f32::EPSILON
                                || (old_max - new_shape.range_max).abs() > f32::EPSILON)
                        {
                            let mut old_shape = new_shape;
                            old_shape.range_min = old_min;
                            old_shape.range_max = old_max;
                            let cmd = SetAudioModShapeCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_shape,
                                new_shape,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
                // Ableton trim now records undo like driver/audio: the live
                // edit already landed via `TrimChanged`, so the command just
                // re-applies the same range and captures the pre-drag range for
                // undo. One step per drag (snapshot on grab, commit on release).
                TrimKind::Ableton => {
                    if let Some((old_min, old_max)) = trim_snapshot.take()
                        && let Some(target) = resolve_graph_target(
                            gpt, editor_target, effective_tab, active_layer, selection, project,
                        )
                        && let Some(mt) = ableton_mapping_target(
                            &target, effective_tab, active_layer, project, param_id,
                        )
                    {
                        let new = project
                            .ableton_param_mappings(&mt)
                            .and_then(|opt| opt.as_ref())
                            .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                            .map(|m| (m.range_min, m.range_max));
                        if let Some((new_min, new_max)) = new
                            && ((old_min - new_min).abs() > f32::EPSILON
                                || (old_max - new_max).abs() > f32::EPSILON)
                        {
                            let cmd = ChangeAbletonTrimCommand::new(
                                mt, old_min, old_max, new_min, new_max,
                            );
                            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::TargetSnapshot(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let t = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                            .map(|e| e.target_normalized)
                    })
                    .flatten();
                if let Some(t) = t {
                    *target_snapshot = Some(t);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::TargetCommit(gpt, param_id) => {
            if let Some(old_target) = target_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let info = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                            .map(|idx| (idx, inst.envelopes.as_ref().unwrap()[idx].target_normalized))
                    })
                    .flatten();
                if let Some((env_idx, new_t)) = info
                    && (old_target - new_t).abs() > f32::EPSILON
                {
                    let cmd =
                        ChangeEnvelopeTargetCommand::new(target, env_idx, old_target, new_t);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }
        PanelAction::EnvDecaySnapshot(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let d = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                            .map(|e| e.decay_beats)
                    })
                    .flatten();
                if let Some(d) = d {
                    *decay_snapshot = Some(d);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EnvDecayCommit(gpt, param_id) => {
            if let Some(old_decay) = decay_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
            {
                let info = project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.envelopes
                            .as_ref()
                            .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                            .map(|idx| (idx, inst.envelopes.as_ref().unwrap()[idx].decay_beats))
                    })
                    .flatten();
                if let Some((env_idx, new_d)) = info
                    && (old_decay - new_d).abs() > f32::EPSILON
                {
                    let cmd =
                        ChangeEnvelopeDecayCommand::new(target, env_idx, old_decay, new_d);
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            *active_inspector_drag = None;
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => DispatchResult::handled(),
        PanelAction::BrowserSearchClicked => DispatchResult::handled(),
        PanelAction::RemoveEffect(fx_idx) => {
            let tab = effective_tab;
            let (effects_ref, target) = resolve_effects_read(tab, project, active_layer, selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx)
            {
                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectReorder(from_idx, to_idx) => {
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }
        // `PanelAction::ToggleNodeParamExpose` is handled in
        // `app_render.rs` alongside the other graph commands so it can
        // access `watched_graph_target` + `watched_catalog_default`
        // directly. No fork on Effect vs Generator at the dispatch
        // layer — the command itself handles both.
        PanelAction::EffectReorderGroup(source_indices, target_idx) => {
            // Multi-select reorder: move a group of effects to the target position.
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, project);
            let (effects_mut, _target) = resolve_effects_mut(tab, project, active_layer, selection);
            if let Some(effects) = effects_mut {
                // Snapshot before
                let old_effects = effects.clone();

                // Remove selected effects in reverse order (preserving relative order)
                let mut moving: Vec<(usize, PresetInstance)> = Vec::new();
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
        PanelAction::GenStringParamClicked(_) | PanelAction::GenStringParamDropdownClicked(_) => {
            // Intercepted in app_render.rs to open text input / dropdown.
            DispatchResult::handled()
        }
        PanelAction::GenStringParamSelected(sp_idx, selected_value) => {
            // A dropdown string param was selected (e.g. font family).
            // Commit it as a SetClipStringParamCommand.
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get(layer_idx)
            {
                let gen_type = layer.generator_type();
                if let Some(def) = manifold_core::preset_definition_registry::try_get(gen_type)
                    && let Some(sp_def) = def.string_param_defs.get(*sp_idx)
                {
                    let key = sp_def.key.to_string();
                    let new_value: Option<String> = if selected_value.is_empty() {
                        None
                    } else {
                        Some(selected_value.clone())
                    };

                    // Find clip: selected clip on this layer, or first clip
                    let clip = selection
                        .primary_selected_clip_id
                        .as_ref()
                        .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                        .or_else(|| layer.clips.first());
                    if let Some(c) = clip {
                        let old_value = c.string_params.as_ref().and_then(|m| m.get(&key)).cloned();
                        if old_value != new_value {
                            let clip_id = c.id.clone();
                            let cmd =
                                manifold_editing::commands::clip::SetClipStringParamCommand::new(
                                    clip_id, key, old_value, new_value,
                                );
                            ContentCommand::send(
                                content_tx,
                                ContentCommand::Execute(Box::new(cmd)),
                            );
                        }
                    }
                }
            }
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
            ui.inspector.clear_effect_selection(&mut ui.tree);
            DispatchResult::handled()
        }
        PanelAction::CardRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown (opens the card context menu)
            // — should not reach dispatch.
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
                        old_type,
                        old_params,
                        old_drivers,
                        old_envelopes,
                        snapshot.generator_type,
                        snapshot.param_values,
                        snapshot.drivers,
                        snapshot.envelopes,
                    );
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::MakePresetUnique(gpt) => {
            // Fork the targeted preset (effect OR generator) into a
            // project-embedded copy and retarget the instance to it. One path
            // for both kinds: resolve the GraphTarget, take its source def
            // (diverged per-instance graph else catalog canonical), fork via
            // the shared command keyed off `target.preset_kind()`.
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((source_def, _)) = preset_source_def(&target, project)
            {
                let cmd = ForkPresetCommand::new(target.clone(), target.preset_kind(), source_def);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::ExportPreset(gpt) => {
            // Export the targeted preset's graph to a .json via a native save
            // dialog. Source def is the diverged per-instance graph else the
            // catalog canonical; the preset id is the filename stem.
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some((def, preset_id)) = preset_source_def(&target, project)
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MANIFOLD Preset", &["json"])
                    .set_file_name(format!("{}.json", preset_id.as_str()))
                    .save_file()
                && let Err(e) = manifold_io::preset_file::export_preset(&def, &path)
            {
                log::error!("[preset] export failed: {e}");
            }
            DispatchResult::handled()
        }
        PanelAction::ImportPreset(gpt) => {
            // Import a .json preset and retarget the targeted instance to it
            // (registered as a project-embedded preset via the shared fork
            // command, so it rides undo + the overlay refresh).
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                editor_target,
                effective_tab,
                active_layer,
                selection,
                project,
            ) && let Some(path) = rfd::FileDialog::new()
                .add_filter("MANIFOLD Preset", &["json"])
                .pick_file()
            {
                match manifold_io::preset_file::import_preset(&path) {
                    Ok(def) => {
                        let cmd =
                            ForkPresetCommand::importing(target.clone(), target.preset_kind(), def);
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                    Err(e) => log::error!("[preset] import failed: {e}"),
                }
            }
            DispatchResult::structural()
        }

        // ── Generator params ───────────────────────────────────────
        PanelAction::GenTypeClicked(_) => DispatchResult::handled(),
        PanelAction::GenParamToggle(param_id) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
            {
                let layer_id = layer.layer_id.clone();
                let slot = layer.gen_params().and_then(|gp| gp.param_id_to_value_index(param_id.as_ref()));
                if let Some(slot) = slot
                    && let Some(gp) = layer.gen_params_mut()
                {
                    let old_val = gp.get_base_param(slot);
                    let new_val = if old_val > 0.5 { 0.0 } else { 1.0 };
                    gp.set_base_param(slot, new_val);
                    let cmd = ChangeGraphParamCommand::new(
                        manifold_core::GraphTarget::Generator(layer_id),
                        param_id.clone(),
                        old_val,
                        new_val,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamFire(param_id) => {
            // Trigger button click: increment the monotonic counter
            // by one. Mirrors GenParamToggle's plumbing exactly except
            // the value transform is `+1` instead of `0↔1`.
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
            {
                let layer_id = layer.layer_id.clone();
                let slot = layer.gen_params().and_then(|gp| gp.param_id_to_value_index(param_id.as_ref()));
                if let Some(slot) = slot
                    && let Some(gp) = layer.gen_params_mut()
                {
                    let old_val = gp.get_base_param(slot);
                    let new_val = old_val + 1.0;
                    gp.set_base_param(slot, new_val);
                    let cmd = ChangeGraphParamCommand::new(
                        manifold_core::GraphTarget::Generator(layer_id),
                        param_id.clone(),
                        old_val,
                        new_val,
                    );
                    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        PanelAction::AddEffect(tab, effect_type) => {
            use manifold_core::effects::PresetInstance;
            // The action carries the chosen preset id directly (registry
            // entries AND project-embedded presets), so no index lookup.
            let effect_type = crate::ui_translate::preset_type_id_to_core(effect_type);
            let defaults = manifold_core::preset_definition_registry::get_defaults(&effect_type);
            let mut effect = PresetInstance::new(effect_type.clone());
            effect.param_values = defaults;
            let layer_idx = super::resolve_active_layer_index(active_layer, project);
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer | InspectorTab::Group => {
                    if let Some(idx) = layer_idx {
                        let layer_id = project
                            .timeline
                            .layers
                            .get(idx)
                            .map(|l| l.layer_id.clone())
                            .unwrap_or_default();
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
                EffectTarget::Layer { layer_id } => project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| l.layer_id == *layer_id)
                    .and_then(|l| l.effects.as_ref())
                    .map(|e| e.len())
                    .unwrap_or(0),
            };
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target, effect, insert_idx,
            );
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        PanelAction::PasteEffects => DispatchResult::handled(),

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        PanelAction::ParamLabelRightClick(..) => {
            DispatchResult::handled()
        }

        // ── Macro mapping ─────────────────────────────────────────
        PanelAction::MapParamToMacro(gpt, param_id, macro_idx) => {
            use manifold_core::{MacroCurve, MacroMapping};
            let macro_idx = *macro_idx;
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) = macro_mapping_target(&target, param_id)
            {
                // Graph-authority-first range so a generator's (or graph-backed
                // effect's) true slider range isn't squashed to the registry's.
                let (min, max) = project
                    .with_preset_graph_mut(&target, |inst| resolve_param_range(inst, param_id.as_ref()))
                    .unwrap_or((0.0, 1.0));
                let mapping = MacroMapping {
                    target: mapping_target,
                    range_min: min,
                    range_max: max,
                    curve: MacroCurve::Linear,
                    legacy_param_index: None,
                    legacy_effect_addr: None,
                };
                project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .push(mapping.clone());
                let mi = macro_idx;
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[mi].mappings.push(mapping);
                    })),
                );
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
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::MutateProject(Box::new(move |p| {
                            let slot = &mut p.settings.macro_bank.slots[macro_idx];
                            if mapping_idx < slot.mappings.len() {
                                slot.mappings.remove(mapping_idx);
                            }
                        })),
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClearMacroMappings(macro_idx) => {
            let macro_idx = *macro_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .clear();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[macro_idx].mappings.clear();
                    })),
                );
            }
            DispatchResult::handled()
        }

        // ── Ableton mapping ────────────────────────────────────────
        // Map + unmap run ONE path: resolve the unified `GraphTarget`, derive
        // the `AbletonMappingTarget` via the shared `ableton_mapping_target`
        // helper (effect by stable EffectId within master/layer; generator by
        // layer; clip tab → None, no clip-scoped Ableton mappings), then send
        // the content command. Mirrors `UnmapParamAbleton` below exactly — the
        // only difference is AbletonMapParam (with address) vs AbletonUnmapParam.
        PanelAction::MapParamToAbleton(gpt, param_id, address) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::AbletonMapParam {
                        target: mapping_target,
                        address: crate::ui_translate::ableton_macro_address_to_core(address),
                    },
                );
            }
            DispatchResult::handled()
        }
        PanelAction::UnmapParamAbleton(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::AbletonUnmapParam {
                        target: mapping_target,
                    },
                );
            }
            DispatchResult::handled()
        }

        PanelAction::MapMacroToAbleton(slot_idx, address) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(
                content_tx,
                ContentCommand::AbletonMapParam {
                    target,
                    address: crate::ui_translate::ableton_macro_address_to_core(address),
                },
            );
            DispatchResult::handled()
        }
        PanelAction::UnmapMacroAbleton(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(content_tx, ContentCommand::AbletonUnmapParam { target });
            DispatchResult::handled()
        }
        // Picker open is consumed by try_open_dropdown — never reaches dispatch.
        PanelAction::OpenAbletonPickerForMacro(_) => DispatchResult::handled(),

        // Driver / Ableton / audio trim handles are unified into the
        // `Trim{Changed,Snapshot,Commit}(TrimKind, …)` arms above.

        PanelAction::AbletonMacroTrimChanged(slot_idx, min, max) => {
            let slot_idx = *slot_idx;
            let min = *min;
            let max = *max;
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.range_min = min;
                m.range_max = max;
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.range_min = min;
                        m.range_max = max;
                    }
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimSnapshot(slot_idx) => {
            if let Some(range) = project
                .settings
                .macro_bank
                .slots
                .get(*slot_idx)
                .and_then(|s| s.ableton_mapping.as_ref())
                .map(|m| (m.range_min, m.range_max))
            {
                *trim_snapshot = Some(range);
            }
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimCommit(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            if let Some((old_min, old_max)) = trim_snapshot.take()
                && let Some((new_min, new_max)) = project
                    .settings
                    .macro_bank
                    .slots
                    .get(*slot_idx)
                    .and_then(|s| s.ableton_mapping.as_ref())
                    .map(|m| (m.range_min, m.range_max))
                && ((old_min - new_min).abs() > f32::EPSILON
                    || (old_max - new_max).abs() > f32::EPSILON)
            {
                let cmd = ChangeAbletonTrimCommand::new(
                    AbletonMappingTarget::MacroSlot { slot_index: *slot_idx },
                    old_min,
                    old_max,
                    new_min,
                    new_max,
                );
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::handled()
        }

        PanelAction::AbletonInvertToggle(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, project, param_id)
            {
                if let Some(ms) = project
                    .ableton_param_mappings_mut(&mapping_target)
                    .and_then(|opt| opt.as_mut())
                    && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                {
                    m.inverted = !m.inverted;
                }
                let mt = mapping_target.clone();
                let pid = param_id.clone();
                ContentCommand::send(
                    content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        if let Some(ms) =
                            p.ableton_param_mappings_mut(&mt).and_then(|opt| opt.as_mut())
                            && let Some(m) = ms.iter_mut().find(|m| m.param_id == pid)
                        {
                            m.inverted = !m.inverted;
                        }
                    })),
                );
            }
            DispatchResult::structural()
        }

        PanelAction::AbletonMacroInvertToggle(slot_idx) => {
            let slot_idx = *slot_idx;
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.inverted = !m.inverted;
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.inverted = !m.inverted;
                    }
                })),
            );
            DispatchResult::structural()
        }

        _ => DispatchResult::unhandled(),
    }
}
