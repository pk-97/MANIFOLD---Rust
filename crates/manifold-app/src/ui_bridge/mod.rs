//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

mod transport;
mod editing;
mod inspector;
mod layer;
mod project;
mod state_sync;

use manifold_core::LayerId;
use manifold_core::effects::EffectInstance;
use manifold_core::project::Project;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_ui::{PanelAction, InspectorTab};

use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;

/// Result of dispatching a panel action.
#[allow(dead_code)]
pub struct DispatchResult {
    /// True if the action was handled.
    pub handled: bool,
    /// True if the action changed project structure (needs sync_project_data).
    pub structural_change: bool,
    /// True if the output resolution changed (needs compositor + generator resize).
    pub resolution_changed: bool,
}

#[allow(dead_code)]
impl DispatchResult {
    pub(crate) fn handled() -> Self { Self { handled: true, structural_change: false, resolution_changed: false } }
    pub(crate) fn structural() -> Self { Self { handled: true, structural_change: true, resolution_changed: false } }
    pub(crate) fn resolution() -> Self { Self { handled: true, structural_change: true, resolution_changed: true } }
    pub(crate) fn unhandled() -> Self { Self { handled: false, structural_change: false, resolution_changed: false } }
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
    adsr_snapshot: &mut Option<(f32, f32, f32, f32)>,
    target_snapshot: &mut Option<f32>,
    user_prefs: &mut UserPrefs,
    active_inspector_drag: &mut Option<crate::app::ActiveInspectorDrag>,
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
        | PanelAction::ContextDeleteLayer(_)
        | PanelAction::ContextPasteAtLayer(_)
        | PanelAction::ContextImportMidi(_)
        | PanelAction::ContextGroupSelectedLayers
        | PanelAction::ContextUngroup(_)
        | PanelAction::ClipRightClicked(_)
        | PanelAction::TrackRightClicked(..)
        | PanelAction::LayerHeaderRightClicked(_)
        | PanelAction::DropdownSelected(_) => {
            editing::dispatch_editing(action, project, content_tx, content_state, ui, selection, active_layer, user_prefs)
        }

        // ── Inspector: chrome, effects, generators ────────────────
        PanelAction::MasterOpacitySnapshot
        | PanelAction::MasterOpacityChanged(_)
        | PanelAction::MasterOpacityCommit
        | PanelAction::MasterCollapseToggle
        | PanelAction::MasterExitPathClicked
        | PanelAction::MasterOpacityRightClick
        | PanelAction::LayerOpacitySnapshot
        | PanelAction::LayerOpacityChanged(_)
        | PanelAction::LayerOpacityCommit
        | PanelAction::LayerChromeCollapseToggle
        | PanelAction::LayerOpacityRightClick
        | PanelAction::ClipChromeCollapseToggle
        | PanelAction::ClipBpmClicked
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
        | PanelAction::EffectParamRightClick(..)
        | PanelAction::EffectParamSnapshot(..)
        | PanelAction::EffectParamChanged(..)
        | PanelAction::EffectParamCommit(..)
        | PanelAction::EffectDriverToggle(..)
        | PanelAction::EffectEnvelopeToggle(..)
        | PanelAction::EffectDriverConfig(..)
        | PanelAction::EffectEnvParamChanged(..)
        | PanelAction::EffectTrimChanged(..)
        | PanelAction::EffectTargetChanged(..)
        | PanelAction::EffectTrimSnapshot(..)
        | PanelAction::EffectTrimCommit(..)
        | PanelAction::EffectTargetSnapshot(..)
        | PanelAction::EffectTargetCommit(..)
        | PanelAction::EffectEnvParamSnapshot(..)
        | PanelAction::EffectEnvParamCommit(..)
        | PanelAction::AddEffectClicked(_)
        | PanelAction::BrowserSearchClicked
        | PanelAction::RemoveEffect(_)
        | PanelAction::EffectReorder(..)
        | PanelAction::EffectReorderGroup(..)
        | PanelAction::GenTypeClicked(_)
        | PanelAction::GenParamSnapshot(_)
        | PanelAction::GenParamChanged(..)
        | PanelAction::GenParamCommit(_)
        | PanelAction::GenParamToggle(_)
        | PanelAction::GenParamRightClick(..)
        | PanelAction::GenDriverToggle(_)
        | PanelAction::GenEnvelopeToggle(_)
        | PanelAction::GenDriverConfig(..)
        | PanelAction::GenEnvParamChanged(..)
        | PanelAction::GenTrimChanged(..)
        | PanelAction::GenTargetChanged(..)
        | PanelAction::GenTrimSnapshot(_)
        | PanelAction::GenTrimCommit(_)
        | PanelAction::GenTargetSnapshot(_)
        | PanelAction::GenTargetCommit(_)
        | PanelAction::GenEnvParamSnapshot(_)
        | PanelAction::GenEnvParamCommit(_)
        | PanelAction::AddEffect(..)
        | PanelAction::PasteEffects => {
            inspector::dispatch_inspector(action, project, content_tx, content_state, ui, selection, active_layer, drag_snapshot, trim_snapshot, adsr_snapshot, target_snapshot, active_inspector_drag)
        }

        // ── Layer operations ──────────────────────────────────────
        PanelAction::ToggleMute(_)
        | PanelAction::ToggleSolo(_)
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
        | PanelAction::LayerDragStarted(_)
        | PanelAction::LayerDragMoved(..)
        | PanelAction::LayerDragEnded(..)
        | PanelAction::AddLayerClicked
        | PanelAction::DeleteLayerClicked(_) => {
            layer::dispatch_layer(action, project, content_tx, content_state, ui, selection, active_layer)
        }

        // ── Project/file/export/audio ─────────────────────────────
        PanelAction::ToggleHdr
        | PanelAction::TogglePercussion
        | PanelAction::ToggleMonitor
        | PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs
        | PanelAction::ExportVideo
        | PanelAction::ExportXml
        | PanelAction::SetMidiNote(..)
        | PanelAction::SetMidiChannel(..)
        | PanelAction::SetResolution(_)
        | PanelAction::SetDisplayResolution(..)
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
        | PanelAction::StemSoloToggled(_) => {
            project::dispatch_project(action, project, content_tx, content_state, ui, selection, active_layer, user_prefs)
        }
    }
}

/// Update the selection region to encompass all currently selected clips.
/// Called after Ctrl+Click multi-select, paste, and duplicate.
/// From Unity InteractionOverlay.UpdateRegionFromClipSelection.
#[allow(dead_code)]
fn update_region_from_clip_selection(selection: &mut SelectionState, project: &Project) {
    if selection.selected_clip_ids.len() < 2 {
        return;
    }
    {
        let mut min_beat = f32::MAX;
        let mut max_beat = f32::MIN;
        let mut min_layer = i32::MAX;
        let mut max_layer = i32::MIN;
        let mut found = false;

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if selection.selected_clip_ids.contains(&clip.id) {
                    min_beat = min_beat.min(clip.start_beat);
                    max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                    min_layer = min_layer.min(clip.layer_index);
                    max_layer = max_layer.max(clip.layer_index);
                    found = true;
                }
            }
        }

        if found {
            selection.set_region_from_clip_bounds(min_beat, max_beat, min_layer, max_layer, &project.timeline.layers);
        }
    }
}

/// Update region from clip selection — public version taking &Project directly.
/// Used by app.rs keyboard handlers that can't pass &PlaybackEngine.
pub fn update_region_from_clip_selection_inline(selection: &mut SelectionState, project: &manifold_core::project::Project) {
    if selection.selected_clip_ids.len() < 2 {
        return;
    }
    let mut min_beat = f32::MAX;
    let mut max_beat = f32::MIN;
    let mut min_layer = i32::MAX;
    let mut max_layer = i32::MIN;
    let mut found = false;

    for layer in &project.timeline.layers {
        for clip in &layer.clips {
            if selection.selected_clip_ids.contains(&clip.id) {
                min_beat = min_beat.min(clip.start_beat);
                max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                min_layer = min_layer.min(clip.layer_index);
                max_layer = max_layer.max(clip.layer_index);
                found = true;
            }
        }
    }

    if found {
        selection.set_region_from_clip_bounds(min_beat, max_beat, min_layer, max_layer, &project.timeline.layers);
    }
}

/// Shift+Click region selection with correct anchor precedence.
pub(crate) fn select_region_to_with_project(
    target_beat: f32,
    target_layer: usize,
    selection: &mut SelectionState,
    project: &Project,
) {
    let layer_count = project.timeline.layers.len();
    if layer_count == 0 { return; }

    let anchor: Option<(f32, usize)> = if selection.has_insert_cursor() {
        let anchor_idx = selection.insert_cursor_layer_id.as_ref()
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .unwrap_or(0);
        Some((
            selection.insert_cursor_beat.unwrap_or(0.0),
            anchor_idx,
        ))
    } else if selection.has_region() {
        let r = selection.get_region();
        Some((r.start_beat, r.start_layer_index as usize))
    } else if let Some(ref clip_id) = selection.primary_selected_clip_id.clone() {
        project.timeline.layers.iter()
            .find_map(|l| l.clips.iter()
                .find(|c| c.id == *clip_id)
                .map(|c| (c.start_beat, c.layer_index as usize)))
    } else {
        None
    };

    match anchor {
        Some((anchor_beat, anchor_layer)) => {
            let min_beat = anchor_beat.min(target_beat);
            let max_beat = anchor_beat.max(target_beat);
            let min_layer = anchor_layer.min(target_layer).min(layer_count - 1) as i32;
            let max_layer = anchor_layer.max(target_layer).min(layer_count - 1) as i32;
            selection.set_region(min_beat, max_beat, min_layer, max_layer, &project.timeline.layers);
        }
        None => {
            let lid = project.timeline.layers.get(target_layer)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            selection.set_insert_cursor(target_beat, lid);
        }
    }
}

/// Handle undo (called from keyboard shortcut). Sends to content thread.
pub fn undo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(content_tx, crate::content_command::ContentCommand::Undo);
}

/// Handle redo (called from keyboard shortcut). Sends to content thread.
pub fn redo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(content_tx, crate::content_command::ContentCommand::Redo);
}

// ── Effect tab routing helpers ───────────────────────────────────

/// Resolve the active layer index from an active LayerId.
pub(crate) fn resolve_active_layer_index(
    active_layer: &Option<LayerId>,
    project: &Project,
) -> Option<usize> {
    active_layer.as_ref().and_then(|id| project.timeline.find_layer_index_by_id(id))
}

/// Build an EffectTarget for the given tab.
pub(crate) fn resolve_effect_target(tab: InspectorTab, active_layer: &Option<LayerId>, project: &Project) -> EffectTarget {
    match tab {
        InspectorTab::Master => EffectTarget::Master,
        InspectorTab::Layer | InspectorTab::Clip => {
            let layer_id = active_layer.clone()
                .unwrap_or_else(|| project.timeline.layers.first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default());
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
) -> (Option<&'a [EffectInstance]>, EffectTarget) {
    match tab {
        InspectorTab::Master => (
            Some(&project.settings.master_effects),
            EffectTarget::Master,
        ),
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
                project.timeline.layers.iter()
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
) -> Option<&'a [EffectInstance]> {
    resolve_effects_read(tab, project, active_layer, selection).0
}

/// Get a mutable reference to the effects list and EffectTarget.
pub(crate) fn resolve_effects_mut<'a>(
    tab: InspectorTab,
    project: &'a mut Project,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
) -> (Option<&'a mut Vec<EffectInstance>>, EffectTarget) {
    match tab {
        InspectorTab::Master => {
            let effects = &mut project.settings.master_effects;
            (Some(effects), EffectTarget::Master)
        }
        InspectorTab::Layer => {
            let layer_id = active_layer.clone()
                .unwrap_or_else(|| project.timeline.layers.first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default());
            let target = EffectTarget::Layer { layer_id };
            let effects = resolve_active_layer_index(active_layer, project)
                .and_then(move |idx| project.timeline.layers.get_mut(idx))
                .map(|l| l.effects_mut());
            (effects, target)
        }
        InspectorTab::Clip => {
            let layer_id = active_layer.clone()
                .unwrap_or_else(|| project.timeline.layers.first()
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default());
            let target = EffectTarget::Layer { layer_id };
            let clip_id = selection.primary_selected_clip_id.clone();
            let effects = clip_id.and_then(|cid| {
                project.timeline.find_clip_by_id_mut(&cid)
                    .map(|c| &mut c.effects)
            });
            (effects, target)
        }
    }
}

// Re-export public functions from sub-modules
pub use state_sync::{push_state, sync_project_data, sync_clip_positions, sync_inspector_data, check_auto_scroll};
