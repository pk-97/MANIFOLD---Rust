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
    /// Set by the `SaveToLibrary`/`SaveToProject` card-menu actions
    /// (PRESET_LIBRARY_DESIGN D4, P3): the resolved kind + current effective
    /// definition + destination, for the caller to open the shared
    /// name-prompt text-input session with. `dispatch_inspector` resolves the
    /// `GraphParamTarget` and the def (it already has `project` + the
    /// tab/selection context); only the CALLER (`app_render.rs`'s per-frame
    /// loop) has `self.text_input`, so the handoff rides here rather than
    /// `dispatch_inspector` reaching for UI-thread-only state it isn't given.
    pub begin_save_preset: Option<(
        manifold_core::preset_def::PresetKind,
        manifold_core::effect_graph_def::EffectGraphDef,
        crate::text_input::SavePresetDestination,
    )>,
    /// Set by `BrowserRenamePresetClicked` (PRESET_LIBRARY_DESIGN P5, D6):
    /// the resolved kind + id + source + CURRENT display name, for the
    /// caller to open the shared name-prompt text-input session with — same
    /// handoff shape as `begin_save_preset` and for the same reason
    /// (`dispatch_inspector` has `project`/`ui` but not `self.text_input`).
    pub begin_rename_preset: Option<(
        manifold_core::preset_def::PresetKind,
        manifold_core::PresetTypeId,
        manifold_ui::panels::picker_core::Source,
        String,
    )>,
}

impl DispatchResult {
    pub(crate) fn handled() -> Self {
        Self {
            structural_change: false,
            resolution_changed: false,
            begin_save_preset: None,
            begin_rename_preset: None,
        }
    }
    pub(crate) fn structural() -> Self {
        Self {
            structural_change: true,
            resolution_changed: false,
            begin_save_preset: None,
            begin_rename_preset: None,
        }
    }
    pub(crate) fn resolution() -> Self {
        Self {
            structural_change: true,
            resolution_changed: true,
            begin_save_preset: None,
            begin_rename_preset: None,
        }
    }
    pub(crate) fn unhandled() -> Self {
        Self {
            structural_change: false,
            resolution_changed: false,
            begin_save_preset: None,
            begin_rename_preset: None,
        }
    }
}

/// Handle an inspector tab-strip click. A tab is a **view** over the live
/// timeline selection, not a selection change: clicking a rung pins the
/// inspector to that scope (Clip / Layer / Group / Master) and repoints the
/// inspector's layer focus (`active_layer`) at it, but never touches what's
/// selected — so the whole ownership chain stays available and the clip you
/// were on stays selected. The pin auto-clears on the next selection change.
/// See docs/UI_LAYOUT_DESIGN.md.
fn inspector_select_tab(
    tab: InspectorTab,
    project: &Project,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    ui: &mut UIRoot,
) {
    use manifold_core::types::LayerType;
    // The selection's own layer — the clip's layer, or the selected layer. The
    // tab repoints `active_layer` relative to this; the selection itself is
    // never changed.
    let sel_layer = selection
        .selected_layer_id_for_clip
        .clone()
        .or_else(|| selection.primary_selected_layer_id.clone());
    match tab {
        // Master needs no layer focus; just pin the scope.
        InspectorTab::Master => selection.pin_scope(InspectorTab::Master),
        // Clip / Layer both focus the selection's own layer (clip chrome + gen
        // params + that layer's effects).
        InspectorTab::Clip | InspectorTab::Layer => {
            if let Some(lid) = sel_layer.or_else(|| active_layer.clone()) {
                *active_layer = Some(lid);
            }
            ui.inspector.clear_effect_selection(&mut ui.tree);
            selection.pin_scope(tab);
        }
        // Group focuses the group: the selection layer's group parent, or the
        // selection layer itself when it is already a group.
        InspectorTab::Group => {
            let gid = sel_layer
                .as_ref()
                .and_then(|lid| project.timeline.find_layer_index_by_id(lid))
                .and_then(|idx| project.timeline.find_group_parent(idx))
                .map(|(_, l)| l.layer_id.clone())
                .or_else(|| {
                    sel_layer
                        .as_ref()
                        .and_then(|lid| project.timeline.find_layer_by_id(lid))
                        .filter(|(_, l)| l.layer_type == LayerType::Group)
                        .map(|(_, l)| l.layer_id.clone())
                });
            if let Some(gid) = gid {
                *active_layer = Some(gid);
            }
            ui.inspector.clear_effect_selection(&mut ui.tree);
            selection.pin_scope(InspectorTab::Group);
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
    // PARAM_STEP_ACTIONS D8: the Step-Amount drag's undo snapshot. `amount`
    // lives on `TriggerAction::Step`, not `AudioModShape`, so it rides its
    // own slot rather than `audio_shape_snapshot`'s.
    audio_action_snapshot: &mut Option<manifold_core::audio_mod::TriggerAction>,
    audio_crossover_snapshot: &mut Option<(f32, f32)>,
    audio_send_gain_drag_snapshot: &mut Option<f32>,
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
        // Right-click reset of a slider to its default, expressed as the
        // slider's own value-change trio (BUG-061). NOT delegated to a
        // sub-dispatcher — it recurses into this same `dispatch` for each of
        // the three inner actions, in order, reusing every existing
        // value-change handler verbatim. That makes reset literally "a drag
        // that lands on the default": undo behaves exactly like a drag to
        // that value, and there is no separate reset code path to keep in
        // sync with each slider's Snapshot/Changed/Commit handler.
        PanelAction::SliderReset { snapshot, changed, commit } => {
            dispatch(
                snapshot,
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
                audio_action_snapshot,
                audio_crossover_snapshot,
                audio_send_gain_drag_snapshot,
                user_prefs,
                active_inspector_drag,
                editor_target,
            );
            dispatch(
                changed,
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
                audio_action_snapshot,
                audio_crossover_snapshot,
                audio_send_gain_drag_snapshot,
                user_prefs,
                active_inspector_drag,
                editor_target,
            );
            dispatch(
                commit,
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
                audio_action_snapshot,
                audio_crossover_snapshot,
                audio_send_gain_drag_snapshot,
                user_prefs,
                active_inspector_drag,
                editor_target,
            )
        }

        // ── Transport ──────────────────────────────────────────────
        PanelAction::PlayPause
        | PanelAction::Stop
        | PanelAction::Record
        | PanelAction::ResetBpm
        | PanelAction::ClearBpm
        | PanelAction::BpmFieldClicked
        | PanelAction::Seek(_)
        | PanelAction::OverviewScrub(_)
        | PanelAction::TimelineScrollbarH(_)
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
        | PanelAction::InspectorScrolled(_)
        | PanelAction::InspectorSectionClicked(_)
        | PanelAction::ToggleAutomationArm
        | PanelAction::AutomationBackToArrangement
        | PanelAction::ToggleAutomationMode => {
            transport::dispatch_transport(action, project, content_tx, content_state, ui, selection)
        }

        // Inspector tab strip — mirrors the timeline selection (needs
        // `active_layer`, which the transport handler doesn't carry).
        PanelAction::SelectInspectorTab(tab) => {
            inspector_select_tab(*tab, project, selection, active_layer, ui);
            DispatchResult::structural()
        }

        // Opens a param type-in session — handled at the Application layer
        // (app_render) before dispatch, so it never actually reaches here; the
        // arm exists only to keep this match exhaustive.
        PanelAction::BeginParamTextInput { .. } => DispatchResult::handled(),
        // Same: the driver Free-period type-in is opened in app_render before
        // dispatch; this arm only keeps the match exhaustive.
        PanelAction::BeginDriverPeriodTextInput { .. } => DispatchResult::handled(),

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
        | PanelAction::LedEnabledToggle
        | PanelAction::LedBrightnessSnapshot
        | PanelAction::LedBrightnessChanged(_)
        | PanelAction::LedBrightnessCommit
        | PanelAction::LayerOpacitySnapshot
        | PanelAction::LayerOpacityChanged(_)
        | PanelAction::LayerOpacityCommit
        | PanelAction::LayerChromeCollapseToggle
        | PanelAction::ClipChromeCollapseToggle
        | PanelAction::ClipBpmClicked
        | PanelAction::ClipWarpToggled
        | PanelAction::ClipDetectClicked
        | PanelAction::ClipClearTriggersClicked
        | PanelAction::ClipReplaceAudioClicked
        | PanelAction::ClipDetectInstrumentToggled(_)
        | PanelAction::ClipDetectSensitivityChanged(..)
        | PanelAction::ClipDetectOnsetChanged(_)
        | PanelAction::ClipDetectQuantizeClicked
        | PanelAction::ClipDetectLayerClicked(_)
        | PanelAction::ClipDetectSetQuantize(_)
        | PanelAction::ClipDetectSetLayer(..)
        | PanelAction::ClipLoopToggle
        | PanelAction::EffectToggle(_)
        | PanelAction::EffectCollapseToggle(_)
        | PanelAction::SetAllCardsCollapsed { .. }
        | PanelAction::ModConfigTabChanged
        | PanelAction::SectionFoldToggled
        | PanelAction::ModsCompactToggled
        | PanelAction::EffectCardClicked(_)
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
        | PanelAction::AudioModSetTriggerMode(..)
        | PanelAction::AudioModSetActionKind(..)
        | PanelAction::AudioModStepAmountSnapshot(..)
        | PanelAction::AudioModStepAmountChanged(..)
        | PanelAction::AudioModStepAmountCommit(..)
        | PanelAction::AudioModSetWrap(..)
        | PanelAction::AudioTriggerSectionToggle
        | PanelAction::AudioTriggerRowExpandToggle(..)
        | PanelAction::AudioTriggerAdd(..)
        | PanelAction::AudioTriggerRemove(..)
        | PanelAction::AudioTriggerEnabledToggle(..)
        | PanelAction::AudioTriggerSetSource(..)
        | PanelAction::AudioTriggerSetInvert(..)
        | PanelAction::AudioTriggerSetRateOfChange(..)
        | PanelAction::AudioTriggerShapeSnapshot(..)
        | PanelAction::AudioTriggerShapeParamChanged(..)
        | PanelAction::AudioTriggerShapeCommit(..)
        | PanelAction::AudioTriggerSetLength(..)
        | PanelAction::AudioSetDevice(..)
        | PanelAction::AudioAddSend
        | PanelAction::AudioRemoveSend(..)
        | PanelAction::AudioSendGainStep(..)
        | PanelAction::AudioSendGainDragBegin(..)
        | PanelAction::AudioSendGainDragChanged(..)
        | PanelAction::AudioSendGainDragCommit(..)
        | PanelAction::AudioSendFloorStep(..)
        | PanelAction::AudioCrossoverDragBegin
        | PanelAction::AudioCrossoverChanged(..)
        | PanelAction::AudioCrossoverCommit
        | PanelAction::AudioRenameSend(..)
        | PanelAction::AudioSetSendChannels(..)
        | PanelAction::AudioSetupDeviceClicked
        | PanelAction::AudioSendChannelClicked(..)
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
        | PanelAction::ParamToggle(..)
        | PanelAction::ParamFire(..)
        | PanelAction::GenStringParamClicked(_)
        | PanelAction::GenStringParamDropdownClicked(_)
        | PanelAction::GenStringParamSelected(..)
        | PanelAction::GenCollapseToggle
        | PanelAction::GenCardClicked
        | PanelAction::CardRightClicked(..)
        | PanelAction::CopyGenerator
        | PanelAction::PasteGenerator
        | PanelAction::MakePresetUnique(..)
        | PanelAction::SaveToLibrary(..)
        | PanelAction::SaveToProject(..)
        | PanelAction::RevertToLibrary(..)
        | PanelAction::PushToLibrary(..)
        | PanelAction::ExportPreset(..)
        | PanelAction::ImportPreset(..)
        | PanelAction::BrowserCellRightClicked(..)
        | PanelAction::BrowserRenamePresetClicked(..)
        | PanelAction::BrowserDuplicatePresetClicked(..)
        | PanelAction::BrowserDeletePresetClicked(..)
        | PanelAction::BrowserRevealPresetClicked(..)
        | PanelAction::MacrosCollapseToggle
        | PanelAction::MacroSnapshot(_)
        | PanelAction::MacroChanged(..)
        | PanelAction::MacroCommit(_)
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
            audio_action_snapshot,
            audio_crossover_snapshot,
            audio_send_gain_drag_snapshot,
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
        | PanelAction::SceneSetupParamChanged(..)
        | PanelAction::SceneSetupAddEnvironment(..)
        | PanelAction::SceneSetupAddFog(..)
        | PanelAction::SceneSetupNewScene(..) => project::dispatch_project(
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
        // D7 "Open Graph Editor" empty-state action — same watch_generator_graph
        // + pending_open_graph_editor mechanism as OpenGeneratorGraphEditor
        // above, just addressed by an explicit layer_id instead of
        // `active_layer_id`. Only `Application` (app_render.rs) holds those
        // fields, so this never reaches `dispatch` either.
        | PanelAction::SceneSetupOpenGraphEditor(_)
        // (Graph-editor mutations are `GraphEditCommand` now — Phase 4.3 —
        // dispatched in app_render's `graph_edits` loop, not here.)
        | PanelAction::EffectMappingRangeSnapshot { .. }
        | PanelAction::EffectMappingRangeChanged { .. }
        | PanelAction::EffectMappingRangeCommit { .. }
        | PanelAction::EffectMappingLabel { .. }
        | PanelAction::EffectMappingSection { .. }
        | PanelAction::EffectMappingInvert { .. }
        | PanelAction::EffectMappingCurve { .. }
        | PanelAction::EffectMappingAffineSnapshot { .. }
        | PanelAction::EffectMappingAffineChanged { .. }
        | PanelAction::EffectMappingAffineCommit { .. }
        | PanelAction::EffectMappingGotoNode { .. }
        // Consumed in app_render (opens the inline rename editor); no-op here.
        | PanelAction::AudioSendLabelClicked(_)
        // Consumed in ui_root::try_open_dropdown (opens the send picker); no-op here.
        | PanelAction::AudioSendClicked(_) => DispatchResult::handled(),

        // Audio Setup dock toggle (D1). The live app handles this in
        // app_render (with its own structural-sync flag); the headless script
        // harness routes every action through here, so the toggle lives on
        // UIRoot and both call it. Structural: it changes `audio_setup_width`,
        // so the tree must rebuild at the new geometry.
        PanelAction::OpenAudioSetup => {
            ui.toggle_audio_dock();
            DispatchResult::structural()
        }

        // Scene Setup dock toggle — mirror of `OpenAudioSetup` above
        // (SCENE_SETUP_PANEL_DESIGN D2).
        PanelAction::OpenSceneSetup => {
            ui.toggle_scene_dock();
            DispatchResult::structural()
        }
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
    } else if let Some(r) = selection.current_region() {
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

/// D2 (`docs/TIMELINE_INTERACTION_P1_SPEC.md`): shift-click on a CLIP is a
/// contiguous whole-clip range selection, NOT a time-range region — the
/// `select_region_to_with_project` call this arm used to make was S1/S3's
/// root. This is the project-based twin of `interaction_overlay::
/// select_clip_range_to` (manifold-ui), which does the identical thing
/// against a `&dyn TimelineEditingHost` for the live gesture path; both must
/// move together (`select_region_to_with_project` stays for the empty-lane
/// shift path, unaffected). Extends from the current `Clips` selection's
/// anchor to `target_clip_id`, selecting every WHOLE clip on the anchor's
/// layer whose start beat falls between the anchor and the target,
/// inclusive — a gap between clips simply isn't a clip, so nothing
/// synthesizes a region there. No live anchor falls back to a plain
/// single-clip select.
pub(crate) fn select_clip_range_to_with_project(
    target_clip_id: &manifold_core::ClipId,
    selection: &mut SelectionState,
    project: &Project,
) {
    let find = |id: &manifold_core::ClipId| -> Option<(usize, LayerId, manifold_core::Beats)> {
        project.timeline.layers.iter().enumerate().find_map(|(li, l)| {
            l.clips
                .iter()
                .find(|c| c.id == *id)
                .map(|c| (li, l.layer_id.clone(), c.start_beat))
        })
    };

    let Some((_, target_layer_id, target_start)) = find(target_clip_id) else {
        return; // clip vanished under us
    };

    let anchor_id = selection.clip_selection_anchor();
    let anchor_lookup = anchor_id.as_ref().and_then(find);

    let Some((anchor_layer_idx, _, anchor_start)) = anchor_lookup else {
        // No live anchor to extend from — behaves like a plain click on the target.
        selection.select_clip(target_clip_id.clone(), target_layer_id);
        return;
    };

    let min_beat = anchor_start.min(target_start);
    let max_beat = anchor_start.max(target_start);

    let ids: std::collections::HashSet<manifold_core::ClipId> = project.timeline.layers
        [anchor_layer_idx]
        .clips
        .iter()
        .filter(|c| c.start_beat >= min_beat && c.start_beat <= max_beat)
        .map(|c| c.id.clone())
        .collect();

    selection.set_clip_range(
        ids,
        anchor_id.expect("anchor_lookup Some implies anchor_id Some"),
        target_clip_id.clone(),
        target_layer_id,
    );
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
        InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => {
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
        InspectorTab::Layer | InspectorTab::Group => {
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
        InspectorTab::Layer | InspectorTab::Group => {
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

#[cfg(test)]
mod d2_d3_tests {
    //! `docs/TIMELINE_INTERACTION_P1_SPEC.md` D2/D3 regression tests — the
    //! project-based twin of the clip-range gesture, and the Cmd+D mode
    //! dispatch it feeds. Mirrors the `ui_snapshot` harness's `selectionclips`
    //! fixture shape (4 clips on one layer) without depending on the
    //! feature-gated `ui_snapshot` module, so these run under a plain
    //! `cargo test -p manifold-app --lib`.
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, ClipId, Seconds};
    use manifold_editing::service::EditingService;
    use manifold_ui::UIState;

    fn clip(id: &str, start: f64, dur: f64) -> manifold_core::clip::TimelineClip {
        let mut c = TimelineClip::new_video(id.into(), Beats(start), Beats(dur), Seconds::ZERO);
        c.id = ClipId::new(id);
        c
    }

    /// `clip_a`[0,8) `clip_b`[8,16) — contiguous — then a GAP (16..24, no
    /// clip there) — then `clip_c`[24,32) `clip_d`[32,40). A second layer
    /// carries `other`[0,40), spanning the same beats, to prove the range is
    /// scoped to the anchor's layer only.
    fn gap_fixture_project() -> Project {
        let mut main = Layer::new("MAIN".into(), LayerType::Video, 0);
        main.layer_id = LayerId::new("main");
        main.clips.push(clip("clip_a", 0.0, 8.0));
        main.clips.push(clip("clip_b", 8.0, 8.0));
        main.clips.push(clip("clip_c", 24.0, 8.0));
        main.clips.push(clip("clip_d", 32.0, 8.0));

        let mut other = Layer::new("OTHER".into(), LayerType::Video, 1);
        other.layer_id = LayerId::new("other");
        other.clips.push(clip("other", 0.0, 40.0));

        let mut project = Project::default();
        project.timeline.layers = vec![main, other];
        project
    }

    #[test]
    fn shift_click_clip_range_across_a_gap_selects_only_whole_clips() {
        let project = gap_fixture_project();
        let mut selection = UIState::new();

        // Anchor: plain click on clip_a.
        selection.select_clip(ClipId::new("clip_a"), LayerId::new("main"));
        // Shift-click extends the range to clip_c — spanning the empty
        // 16..24 gap and clip_b, but NOT clip_d (past the target) or `other`
        // (a different layer, even though its span covers the same beats).
        select_clip_range_to_with_project(&ClipId::new("clip_c"), &mut selection, &project);

        let ids: std::collections::HashSet<ClipId> =
            selection.get_selected_clip_ids().into_iter().collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from([
                ClipId::new("clip_a"),
                ClipId::new("clip_b"),
                ClipId::new("clip_c"),
            ]),
            "range selects every whole clip between anchor and target, gap included, nothing beyond"
        );
        // The mode leak this phase kills: no synthesized region, ever.
        assert!(
            selection.current_region().is_none(),
            "a clip-range shift-click must produce a Clips selection, never a TimeRange region"
        );
    }

    #[test]
    fn shift_click_clip_range_no_live_anchor_falls_back_to_plain_click() {
        // Fresh selection (no anchor yet) — shift-clicking a clip behaves
        // like a plain click on it, per the D2 fallback.
        let project = gap_fixture_project();
        let mut selection = UIState::new();
        select_clip_range_to_with_project(&ClipId::new("clip_b"), &mut selection, &project);

        let ids: Vec<ClipId> = selection.get_selected_clip_ids();
        assert_eq!(ids, vec![ClipId::new("clip_b")]);
        assert!(selection.current_region().is_none());
    }

    /// Mirrors `input_host.rs`'s `duplicate_clips` dispatch exactly (region =
    /// `selection.current_region().cloned().unwrap_or_default()`, mode =
    /// `region.is_active`) — the D3 typed dispatch this phase's fix is
    /// downstream of (D1 already made `current_region()` return `None` for a
    /// `Clips` selection; D2's fix is what keeps shift-click FROM producing a
    /// stray `TimeRange` in the first place).
    fn duplicate_selected_clips(
        project: &mut Project,
        selection: &UIState,
    ) -> (Vec<Box<dyn manifold_editing::command::Command>>, bool) {
        let clip_ids = selection.get_selected_clip_ids();
        let region = selection.current_region().cloned().unwrap_or_default();
        let used_region_mode = region.is_active;
        let region_core = crate::ui_translate::selection_region_to_core(&region);
        let spb = 60.0 / project.settings.bpm.0.max(1.0);
        let commands = EditingService::duplicate_clips(project, &clip_ids, &region_core, spb);
        (commands, used_region_mode)
    }

    #[test]
    fn cmd_d_on_four_contiguous_clips_lands_flush_via_the_clips_path() {
        let mut project = gap_fixture_project();
        let mut selection = UIState::new();
        // Select all 4 clips as a plain `Clips` set (as D2's fixed shift-click
        // range now produces) — NOT via a region.
        selection.select_clips(vec![
            ClipId::new("clip_a"),
            ClipId::new("clip_b"),
            ClipId::new("clip_c"),
            ClipId::new("clip_d"),
        ]);
        assert!(
            selection.current_region().is_none(),
            "precondition: a Clips selection must not carry a region"
        );

        let (mut commands, used_region_mode) = duplicate_selected_clips(&mut project, &selection);
        assert!(
            !used_region_mode,
            "Clips selection must dispatch the individual/clip-span path, not region mode"
        );
        assert_eq!(commands.len(), 4);

        for c in commands.iter_mut() {
            c.execute(&mut project);
        }

        // Old span: clip_a start=0 .. clip_d end=40 (with the 16..24 gap
        // inside it). New copies must land flush at the span's END (40) —
        // not offset by some other span/region duration.
        let main = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_id == LayerId::new("main"))
            .unwrap();
        let new_min_start = main
            .clips
            .iter()
            .filter(|c| {
                !["clip_a", "clip_b", "clip_c", "clip_d"].contains(&c.id.as_str())
            })
            .map(|c| c.start_beat)
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
            .expect("4 new copies were created");
        assert_eq!(
            new_min_start,
            Beats(40.0),
            "copies land flush at the old selection's span end, not gapped"
        );
    }

    #[test]
    fn cmd_d_on_a_rubber_band_time_range_preserves_region_mode() {
        let mut project = gap_fixture_project();
        let mut selection = UIState::new();
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        // A rubber-band region covering clip_a..clip_b on the main layer only.
        selection.set_region(Beats(0.0), Beats(16.0), 0, 0, &ui_layers);
        assert!(selection.current_region().is_some());

        let (commands, used_region_mode) = duplicate_selected_clips(&mut project, &selection);
        assert!(
            used_region_mode,
            "a TimeRange selection must still dispatch today's region-duplicate mode"
        );
        assert!(!commands.is_empty());
    }
}
