//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

mod editing;
mod inspector;
mod layer;
mod marker;
mod project;
mod state_sync;
mod transport;

use manifold_core::LayerId;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_ui::{InspectorTab, PanelAction};

use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;

/// Result of dispatching a panel action.
pub struct DispatchResult {
    /// True if the action changed project structure (needs sync_project_data).
    pub structural_change: bool,
    /// True if the output resolution changed (needs compositor + generator resize).
    pub resolution_changed: bool,
}

impl DispatchResult {
    pub(crate) fn handled() -> Self {
        Self {
            structural_change: false,
            resolution_changed: false,
        }
    }
    pub(crate) fn structural() -> Self {
        Self {
            structural_change: true,
            resolution_changed: false,
        }
    }
    pub(crate) fn resolution() -> Self {
        Self {
            structural_change: true,
            resolution_changed: true,
        }
    }
    pub(crate) fn unhandled() -> Self {
        Self {
            structural_change: false,
            resolution_changed: false,
        }
    }
}

/// Dispatch a panel action. Mutates local_project for immediate feedback;
/// sends commands to the content thread for authoritative execution.
pub fn dispatch(
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
    user_prefs: &mut UserPrefs,
    active_inspector_drag: &mut Option<crate::app::ActiveInspectorDrag>,
    // `Some(GraphTarget)` when the graph editor dispatches one of its left-lane
    // card actions: the edit targets that effect/generator by stable identity,
    // regardless of the main window's active selection. `None` for the
    // inspector / perform path, which resolves its own active context. Only
    // consulted by `dispatch_inspector`.
    editor_target: Option<&manifold_core::GraphTarget>,
) -> DispatchResult {
    match action {
        // ── Transport ──────────────────────────────────────────────
        PanelAction::PlayPause
        | PanelAction::Stop
        | PanelAction::Record
        | PanelAction::ResetBpm
        | PanelAction::ClearBpm
        | PanelAction::BpmFieldClicked
        | PanelAction::Seek(_)
        | PanelAction::OverviewScrub(_)
        | PanelAction::SetInsertCursor(_)
        | PanelAction::CycleClockAuthority
        | PanelAction::ToggleLink
        | PanelAction::ToggleMidiClock
        | PanelAction::ToggleSyncOutput
        | PanelAction::SelectClkDevice
        | PanelAction::SetMidiClockDevice(_)
        | PanelAction::CycleQuantize
        | PanelAction::ResolutionClicked
        | PanelAction::FpsFieldClicked
        | PanelAction::ZoomIn
        | PanelAction::ZoomOut
        | PanelAction::SelectInspectorTab(_)
        | PanelAction::InspectorScrolled(_)
        | PanelAction::InspectorSectionClicked(_) => {
            transport::dispatch_transport(action, project, content_tx, content_state, ui, selection)
        }

        // ── Viewport clip interaction + context menus ──────────────
        PanelAction::ClipClicked(..)
        | PanelAction::ClipDoubleClicked(_)
        | PanelAction::TrackClicked(..)
        | PanelAction::TrackDoubleClicked(..)
        | PanelAction::ViewportHoverChanged(_)
        | PanelAction::ContextSplitAtPlayhead(_)
        | PanelAction::ContextDeleteClip(_)
        | PanelAction::ContextDuplicateClip(_)
        | PanelAction::ContextPasteAtTrack(..)
        | PanelAction::ContextAddVideoLayer(_)
        | PanelAction::ContextAddGeneratorLayer(_)
        | PanelAction::ContextAddAudioLayer(_)
        | PanelAction::ContextDeleteLayer(_)
        | PanelAction::ContextDuplicateLayer(_)
        | PanelAction::ContextPasteAtLayer(_)
        | PanelAction::ContextImportMidi(_)
        | PanelAction::ContextGroupSelectedLayers
        | PanelAction::ContextUngroup(_)
        | PanelAction::ContextSetLayerColor(..)
        | PanelAction::ClipRightClicked(_)
        | PanelAction::TrackRightClicked(..)
        | PanelAction::LayerHeaderRightClicked(_)
        | PanelAction::DropdownSelected(_) => editing::dispatch_editing(
            action,
            project,
            content_tx,
            content_state,
            ui,
            selection,
            active_layer,
            user_prefs,
        ),

        // ── Inspector: chrome, effects, generators ────────────────
        PanelAction::MasterOpacitySnapshot
        | PanelAction::MasterOpacityChanged(_)
        | PanelAction::MasterOpacityCommit
        | PanelAction::AudioGainSnapshot(_)
        | PanelAction::AudioGainChanged(..)
        | PanelAction::AudioGainCommit(_)
        | PanelAction::MasterCollapseToggle
        | PanelAction::MasterExitPathClicked
        | PanelAction::SetLedExitIndex(_)
        | PanelAction::MasterOpacityRightClick
        | PanelAction::LedEnabledToggle
        | PanelAction::LedBrightnessSnapshot
        | PanelAction::LedBrightnessChanged(_)
        | PanelAction::LedBrightnessCommit
        | PanelAction::LedBrightnessRightClick
        | PanelAction::LayerOpacitySnapshot
        | PanelAction::LayerOpacityChanged(_)
        | PanelAction::LayerOpacityCommit
        | PanelAction::LayerChromeCollapseToggle
        | PanelAction::LayerOpacityRightClick
        | PanelAction::ClipChromeCollapseToggle
        | PanelAction::ClipBpmClicked
        | PanelAction::ClipWarpToggled
        | PanelAction::ClipDetectClicked
        | PanelAction::ClipClearTriggersClicked
        | PanelAction::ClipDetectInstrumentToggled(_)
        | PanelAction::ClipDetectSensitivityChanged(..)
        | PanelAction::ClipDetectOnsetChanged(_)
        | PanelAction::ClipDetectQuantizeClicked
        | PanelAction::ClipDetectLayerClicked(_)
        | PanelAction::ClipDetectSetQuantize(_)
        | PanelAction::ClipDetectSetLayer(..)
        | PanelAction::ClipLoopToggle
        | PanelAction::ClipSlipSnapshot
        | PanelAction::ClipSlipChanged(_)
        | PanelAction::ClipSlipCommit
        | PanelAction::ClipLoopSnapshot
        | PanelAction::ClipLoopChanged(_)
        | PanelAction::ClipLoopCommit
        | PanelAction::ClipSlipRightClick
        | PanelAction::ClipLoopRightClick
        | PanelAction::EffectToggle(_)
        | PanelAction::EffectCollapseToggle(_)
        | PanelAction::EffectCardClicked(_)
        | PanelAction::ParamRightClick(..)
        | PanelAction::ParamSnapshot(..)
        | PanelAction::ParamChanged(..)
        | PanelAction::ParamCommit(..)
        | PanelAction::DriverToggle(..)
        | PanelAction::EnvelopeToggle(..)
        | PanelAction::DriverConfig(..)
        | PanelAction::AudioModToggle(..)
        | PanelAction::AudioModSetSource(..)
        | PanelAction::AudioModRemove(..)
        | PanelAction::AudioModSetInvert(..)
        | PanelAction::AudioModSetRateOfChange(..)
        | PanelAction::AudioModShapeSnapshot(..)
        | PanelAction::AudioModShapeParamChanged(..)
        | PanelAction::AudioModShapeCommit(..)
        | PanelAction::AudioSetDevice(..)
        | PanelAction::AudioAddSend
        | PanelAction::AudioRemoveSend(..)
        | PanelAction::AudioSendGainStep(..)
        | PanelAction::AudioSendFloorStep(..)
        | PanelAction::AudioCrossoverDragBegin
        | PanelAction::AudioCrossoverChanged(..)
        | PanelAction::AudioCrossoverCommit
        | PanelAction::AudioRenameSend(..)
        | PanelAction::AudioSetSendChannels(..)
        | PanelAction::AudioSendStereoToggle(..)
        | PanelAction::AudioSendRoutingsClicked(..)
        | PanelAction::AudioSetupDeviceClicked
        | PanelAction::AudioSendChannelClicked(..)
        | PanelAction::AudioTriggerToggled(..)
        | PanelAction::AudioTriggerSensitivityStep(..)
        | PanelAction::AudioTriggerLengthStep(..)
        | PanelAction::AudioTriggerLayerClicked(..)
        | PanelAction::AudioTriggerSetLayer(..)
        | PanelAction::TrimChanged(..)
        | PanelAction::TargetChanged(..)
        | PanelAction::TrimSnapshot(..)
        | PanelAction::TrimCommit(..)
        | PanelAction::TargetSnapshot(..)
        | PanelAction::TargetCommit(..)
        | PanelAction::EnvDecayChanged(..)
        | PanelAction::EnvDecaySnapshot(..)
        | PanelAction::EnvDecayCommit(..)
        | PanelAction::ParamLabelRightClick(..)
        | PanelAction::AddEffectClicked(_)
        | PanelAction::BrowserSearchClicked
        | PanelAction::RemoveEffect(_)
        | PanelAction::EffectReorder(..)
        | PanelAction::EffectReorderGroup(..)
        | PanelAction::GenTypeClicked(_)
        | PanelAction::GenParamToggle(_)
        | PanelAction::GenParamFire(_)
        | PanelAction::GenStringParamClicked(_)
        | PanelAction::GenStringParamDropdownClicked(_)
        | PanelAction::GenStringParamSelected(..)
        | PanelAction::GenCollapseToggle
        | PanelAction::GenCardClicked
        | PanelAction::CardRightClicked(..)
        | PanelAction::CopyGenerator
        | PanelAction::PasteGenerator
        | PanelAction::MakePresetUnique(..)
        | PanelAction::ExportPreset(..)
        | PanelAction::ImportPreset(..)
        | PanelAction::MacrosCollapseToggle
        | PanelAction::MacroSnapshot(_)
        | PanelAction::MacroChanged(..)
        | PanelAction::MacroCommit(_)
        | PanelAction::MacroRightClick(_)
        | PanelAction::MacroReset(_)
        | PanelAction::MacroLabelRightClick(_)
        | PanelAction::MacroLabelRename(_)
        | PanelAction::MapParamToMacro(..)
        | PanelAction::UnmapMacro(..)
        | PanelAction::ClearMacroMappings(_)
        | PanelAction::MapParamToAbleton(..)
        | PanelAction::UnmapParamAbleton(..)
        | PanelAction::OpenAbletonPickerForParam(..)
        | PanelAction::MapMacroToAbleton(..)
        | PanelAction::UnmapMacroAbleton(_)
        | PanelAction::OpenAbletonPickerForMacro(_)
        | PanelAction::AbletonMacroTrimSnapshot(_)
        | PanelAction::AbletonMacroTrimChanged(..)
        | PanelAction::AbletonMacroTrimCommit(_)
        | PanelAction::AbletonInvertToggle(..)
        | PanelAction::AbletonMacroInvertToggle(_)
        | PanelAction::AddEffect(..)
        | PanelAction::PasteEffects => inspector::dispatch_inspector(
            action,
            project,
            content_tx,
            content_state,
            ui,
            selection,
            active_layer,
            drag_snapshot,
            trim_snapshot,
            target_snapshot,
            decay_snapshot,
            audio_shape_snapshot,
            audio_crossover_snapshot,
            active_inspector_drag,
            editor_target,
        ),

        // ── Layer operations ──────────────────────────────────────
        PanelAction::ToggleMute(_)
        | PanelAction::ToggleSolo(_)
        | PanelAction::ToggleAnalysisOnly(_)
        | PanelAction::ToggleLed(_)
        | PanelAction::LayerClicked(..)
        | PanelAction::LayerDoubleClicked(_)
        | PanelAction::ChevronClicked(_)
        | PanelAction::BlendModeClicked(_)
        | PanelAction::SetBlendMode(..)
        | PanelAction::ExpandLayer(_)
        | PanelAction::CollapseLayer(_)
        | PanelAction::FolderClicked(_)
        | PanelAction::NewClipClicked(_)
        | PanelAction::AddGenClipClicked(_)
        | PanelAction::MidiInputClicked(_)
        | PanelAction::MidiChannelClicked(_)
        | PanelAction::MidiDeviceClicked(_)
        | PanelAction::LayerDragStarted(_)
        | PanelAction::LayerDragMoved(..)
        | PanelAction::LayerDragEnded(..)
        | PanelAction::AddLayerClicked
        | PanelAction::DeleteLayerClicked(_)
        | PanelAction::SetLayerAudioSend(..) => layer::dispatch_layer(
            action,
            project,
            content_tx,
            content_state,
            ui,
            selection,
            active_layer,
        ),

        // ── Timeline markers ─────────────────────────────────────────
        PanelAction::MarkerClicked(..)
        | PanelAction::MarkerDoubleClicked(_)
        | PanelAction::MarkerDragStarted(_)
        | PanelAction::MarkerDragMoved(..)
        | PanelAction::MarkerDragEnded(..)
        | PanelAction::MarkerRightClicked(_)
        | PanelAction::DeleteSelectedMarkers => {
            marker::dispatch_marker(action, project, content_tx, ui, selection, drag_snapshot)
        }

        // ── Project/file/export/audio ─────────────────────────────
        PanelAction::ToggleHdr
        | PanelAction::TogglePercussion
        | PanelAction::ToggleLiveRecording
        | PanelAction::SelectAudioInputDevice
        | PanelAction::SetAudioInputDevice(_)
        | PanelAction::ToggleMonitor
        | PanelAction::EnterPerformMode
        | PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs
        | PanelAction::ExportVideo
        | PanelAction::ExportFrame
        | PanelAction::ExportXml
        | PanelAction::SetMidiNote(..)
        | PanelAction::SetMidiChannel(..)
        | PanelAction::SetMidiDevice(..)
        | PanelAction::SetMidiTriggerMode(..)
        | PanelAction::MidiTriggerModeClicked(_)
        | PanelAction::SetResolution(_)
        | PanelAction::SetDisplayResolution(..)
        | PanelAction::SetRenderScale(_)
        | PanelAction::SetTonemapCurve(_)
        | PanelAction::SetGenType(..)
        | PanelAction::ImportAudioClicked
        | PanelAction::RemoveAudioClicked
        | PanelAction::WaveformScrub(..)
        | PanelAction::WaveformDragDelta(_)
        | PanelAction::WaveformDragEnd(_)
        | PanelAction::ExpandStemsToggled(_)
        | PanelAction::ReAnalyzeDrums
        | PanelAction::ReAnalyzeBass
        | PanelAction::ReAnalyzeSynth
        | PanelAction::ReAnalyzeVocal
        | PanelAction::ReImportStems
        | PanelAction::StemMuteToggled(_)
        | PanelAction::StemSoloToggled(_) => project::dispatch_project(
            action,
            project,
            content_tx,
            content_state,
            ui,
            selection,
            active_layer,
            user_prefs,
        ),

        // Handled in app_render.rs (Application-level intercept, never reaches dispatch)
        PanelAction::CopyOscAddress(_)
        | PanelAction::OpenGraphEditor(_)
        | PanelAction::OpenCardMapping(_)
        | PanelAction::OpenGeneratorGraphEditor
        // (Graph-editor mutations are `GraphEditCommand` now — Phase 4.3 —
        // dispatched in app_render's `graph_edits` loop, not here.)
        | PanelAction::EffectMappingRangeSnapshot { .. }
        | PanelAction::EffectMappingRangeChanged { .. }
        | PanelAction::EffectMappingRangeCommit { .. }
        | PanelAction::EffectMappingLabel { .. }
        | PanelAction::EffectMappingInvert { .. }
        | PanelAction::EffectMappingCurve { .. }
        | PanelAction::EffectMappingAffineSnapshot { .. }
        | PanelAction::EffectMappingAffineChanged { .. }
        | PanelAction::EffectMappingAffineCommit { .. }
        | PanelAction::EffectMappingGotoNode { .. }
        | PanelAction::OpenAudioSetup
        // Consumed in app_render (opens the inline rename editor); no-op here.
        | PanelAction::AudioSendLabelClicked(_)
        // Consumed in ui_root::try_open_dropdown (opens the send picker); no-op here.
        | PanelAction::AudioSendClicked(_) => DispatchResult::handled(),
    }
}

/// Update region from clip selection — public version taking &Project directly.
/// Used by app.rs keyboard handlers that can't pass &PlaybackEngine.
pub fn update_region_from_clip_selection_inline(
    selection: &mut SelectionState,
    project: &manifold_core::project::Project,
) {
    if selection.selected_clip_ids.len() < 2 {
        return;
    }
    let mut min_beat = manifold_core::Beats(f64::MAX);
    let mut max_beat = manifold_core::Beats(-f64::MAX);
    let mut min_layer = i32::MAX;
    let mut max_layer = i32::MIN;
    let mut found = false;

    for (li, layer) in project.timeline.layers.iter().enumerate() {
        let li = li as i32;
        for clip in &layer.clips {
            if selection.selected_clip_ids.contains(&clip.id) {
                min_beat = min_beat.min(clip.start_beat);
                max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                min_layer = min_layer.min(li);
                max_layer = max_layer.max(li);
                found = true;
            }
        }
    }

    if found {
        selection.set_region_from_clip_bounds(
            min_beat,
            max_beat,
            min_layer,
            max_layer,
            &crate::ui_translate::layers_to_ui(&project.timeline.layers),
        );
    }
}

/// Shift+Click region selection with correct anchor precedence.
pub(crate) fn select_region_to_with_project(
    target_beat: manifold_core::Beats,
    target_layer: usize,
    selection: &mut SelectionState,
    project: &Project,
) {
    let layer_count = project.timeline.layers.len();
    if layer_count == 0 {
        return;
    }

    let anchor: Option<(manifold_core::Beats, usize)> = if selection.has_insert_cursor() {
        let anchor_idx = selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .unwrap_or(0);
        Some((
            selection
                .insert_cursor_beat
                .unwrap_or(manifold_core::Beats::ZERO),
            anchor_idx,
        ))
    } else if selection.has_region() {
        let r = selection.get_region();
        let start_idx = r
            .layer_index_range(&crate::ui_translate::layers_to_ui(&project.timeline.layers))
            .map(|(lo, _)| lo)
            .unwrap_or(0);
        Some((r.start_beat, start_idx))
    } else if let Some(ref clip_id) = selection.primary_selected_clip_id.clone() {
        project
            .timeline
            .layers
            .iter()
            .enumerate()
            .find_map(|(li, l)| {
                l.clips
                    .iter()
                    .find(|c| c.id == *clip_id)
                    .map(|_c| (_c.start_beat, li))
            })
    } else {
        None
    };

    match anchor {
        Some((anchor_beat, anchor_layer)) => {
            let min_beat = anchor_beat.min(target_beat);
            let max_beat = anchor_beat.max(target_beat);
            let min_layer = anchor_layer.min(target_layer).min(layer_count - 1) as i32;
            let max_layer = anchor_layer.max(target_layer).min(layer_count - 1) as i32;
            selection.set_region(
                min_beat,
                max_beat,
                min_layer,
                max_layer,
                &crate::ui_translate::layers_to_ui(&project.timeline.layers),
            );
        }
        None => {
            let lid = project
                .timeline
                .layers
                .get(target_layer)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();
            selection.set_insert_cursor(target_beat, lid);
        }
    }
}

/// Handle undo (called from keyboard shortcut). Sends to content thread.
pub fn undo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Undo,
    );
}

/// Handle redo (called from keyboard shortcut). Sends to content thread.
pub fn redo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Redo,
    );
}

// ── Effect tab routing helpers ───────────────────────────────────

/// Resolve the active layer index from an active LayerId.
pub(crate) fn resolve_active_layer_index(
    active_layer: &Option<LayerId>,
    project: &Project,
) -> Option<usize> {
    active_layer
        .as_ref()
        .and_then(|id| project.timeline.find_layer_index_by_id(id))
}

/// Build an EffectTarget for the given tab.
pub(crate) fn resolve_effect_target(
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    project: &Project,
) -> EffectTarget {
    match tab {
        InspectorTab::Master => EffectTarget::Master,
        InspectorTab::Layer | InspectorTab::Clip => {
            let layer_id = active_layer.clone().unwrap_or_else(|| {
                project
                    .timeline
                    .layers
                    .first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default()
            });
            EffectTarget::Layer { layer_id }
        }
    }
}

/// Get a read-only reference to the effects list and the EffectTarget.
pub(crate) fn resolve_effects_read<'a>(
    tab: InspectorTab,
    project: &'a Project,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
) -> (Option<&'a [PresetInstance]>, EffectTarget) {
    match tab {
        InspectorTab::Master => (Some(&project.settings.master_effects), EffectTarget::Master),
        InspectorTab::Layer => {
            let target = resolve_effect_target(tab, active_layer, project);
            let effects = resolve_active_layer_index(active_layer, project)
                .and_then(|idx| project.timeline.layers.get(idx))
                .and_then(|l| l.effects.as_deref());
            (effects, target)
        }
        InspectorTab::Clip => {
            let target = resolve_effect_target(tab, active_layer, project);
            let effects = selection.primary_selected_clip_id.as_ref().and_then(|cid| {
                project
                    .timeline
                    .layers
                    .iter()
                    .flat_map(|l| l.clips.iter())
                    .find(|c| c.id == *cid)
                    .map(|c| c.effects.as_slice())
            });
            (effects, target)
        }
    }
}

/// Get a read-only reference to effects (simpler version for snapshot/commit).
pub(crate) fn resolve_effects_ref<'a>(
    tab: InspectorTab,
    project: &'a Project,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
) -> Option<&'a [PresetInstance]> {
    resolve_effects_read(tab, project, active_layer, selection).0
}

/// Get a mutable reference to the effects list and EffectTarget.
pub(crate) fn resolve_effects_mut<'a>(
    tab: InspectorTab,
    project: &'a mut Project,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
) -> (Option<&'a mut Vec<PresetInstance>>, EffectTarget) {
    match tab {
        InspectorTab::Master => {
            let effects = &mut project.settings.master_effects;
            (Some(effects), EffectTarget::Master)
        }
        InspectorTab::Layer => {
            let layer_id = active_layer.clone().unwrap_or_else(|| {
                project
                    .timeline
                    .layers
                    .first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default()
            });
            let target = EffectTarget::Layer { layer_id };
            let effects = resolve_active_layer_index(active_layer, project)
                .and_then(move |idx| project.timeline.layers.get_mut(idx))
                .map(|l| l.effects_mut());
            (effects, target)
        }
        InspectorTab::Clip => {
            let layer_id = active_layer.clone().unwrap_or_else(|| {
                project
                    .timeline
                    .layers
                    .first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default()
            });
            let target = EffectTarget::Layer { layer_id };
            let clip_id = selection.primary_selected_clip_id.clone();
            let effects = clip_id.and_then(|cid| {
                project
                    .timeline
                    .find_clip_by_id_mut(&cid)
                    .map(|c| &mut c.effects)
            });
            (effects, target)
        }
    }
}

/// Resolve "which effect instance" a single-effect card edit targets, as a
/// stable [`EffectId`].
///
/// Two callers, one rule:
/// - **Graph editor** (`editor_target == Some(Effect(id))`): the edited card IS
///   one effect, named by identity. Its id wins outright — the positional `idx`
///   from the action payload is ignored, and the resolution reaches master /
///   layer / clip effects uniformly (so clip-scoped effects edit correctly).
/// - **Inspector / perform path** (`editor_target == None`, or a `Generator`
///   target that an effect arm never sees): resolve the host's OWN active
///   context — the inspector tab + active layer + selected clip — to the effect
///   list, then read `effects[idx].id`. This is legitimate context, not the
///   ambient-mis-targeting bug: the inspector edits exactly what it is showing.
pub(crate) fn resolve_effect_id(
    editor_target: Option<&manifold_core::GraphTarget>,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    project: &Project,
    idx: usize,
) -> Option<manifold_core::EffectId> {
    if let Some(manifold_core::GraphTarget::Effect(eid)) = editor_target {
        return Some(eid.clone());
    }
    resolve_effects_ref(tab, project, active_layer, selection)?
        .get(idx)
        .map(|fx| fx.id.clone())
}

/// The `(inspector tab, active layer)` the dispatch arms that still resolve
/// positionally — the **modulation** family (drivers, layer-stored envelopes,
/// trims, envelope targets) — must use so a card edit dispatched from the graph
/// editor targets the editor's WATCHED effect, not the main window's current
/// selection.
///
/// The single-effect *value / expose / mapping* arms already address by stable
/// `EffectId` (via [`resolve_effect_id`]) and ignore this. But the modulation
/// arms key on `(tab, active_layer)` + the effect's position, and the editor is
/// a separate window whose watched effect routinely diverges from the main
/// window's selection. This expresses the editor's identity in the positional
/// terms those arms still speak (a Stage-C cleanup would have every action carry
/// its own id and retire this):
///
/// - **Generator** target → the generator's layer.
/// - **Effect** target → the effect's *container*: `Master` for a master
///   effect, its owning `Layer` for a layer-scoped effect, or `Clip` for a
///   clip-scoped effect. The `Clip` mapping is deliberate: layer envelopes are
///   keyed by `(effect_type, param_id)` and cannot represent a clip effect
///   without colliding with a same-type layer effect, and positional resolution
///   can't reach it — so the modulation arms (whose `Clip` branch is `None`)
///   safely no-op, while the value/mapping arms still address it by id.
/// - **`None`** (inspector / perform path) → the inspector's own ambient
///   context, byte-identical to before.
pub(crate) fn editor_dispatch_context(
    editor_target: Option<&manifold_core::GraphTarget>,
    project: &Project,
    inspector_tab: InspectorTab,
    active_layer: &Option<LayerId>,
) -> (InspectorTab, Option<LayerId>) {
    match editor_target {
        Some(manifold_core::GraphTarget::Generator(lid)) => {
            (InspectorTab::Layer, Some(lid.clone()))
        }
        Some(manifold_core::GraphTarget::Effect(eid)) => {
            if project.settings.master_effects.iter().any(|fx| &fx.id == eid) {
                (InspectorTab::Master, None)
            } else if let Some(lid) = project.timeline.layers.iter().find_map(|l| {
                l.effects
                    .as_deref()
                    .filter(|e| e.iter().any(|fx| &fx.id == eid))
                    .map(|_| l.layer_id.clone())
            }) {
                (InspectorTab::Layer, Some(lid))
            } else {
                // Clip-scoped or unresolved: layer modulation no-ops safely.
                (InspectorTab::Clip, active_layer.clone())
            }
        }
        None => (inspector_tab, active_layer.clone()),
    }
}

/// Build the display label for the LED exit path dropdown button.
pub(crate) fn led_exit_path_label(
    led_exit_index: i32,
    master_effects: &[manifold_core::effects::PresetInstance],
) -> String {
    use manifold_core::preset_type_registry;
    match led_exit_index {
        -1 => "After All FX".into(),
        0 => "Before FX".into(),
        n => {
            let idx = (n - 1) as usize;
            if let Some(fx) = master_effects.get(idx) {
                format!(
                    "After {}",
                    preset_type_registry::display_name(fx.effect_type())
                )
            } else {
                "After All FX".into()
            }
        }
    }
}

// Re-export public functions from sub-modules
pub use state_sync::{
    TransportDisplayCache, check_auto_scroll, push_state, sync_clip_positions, sync_inspector_data,
    sync_project_data,
};
// Crate-internal: the graph editor's left-lane card resolver.
pub(crate) use state_sync::editor_card_config;
