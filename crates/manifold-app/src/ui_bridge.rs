//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

use manifold_core::types::{
    BlendMode, GeneratorType, LayerType,
    BeatDivision, DriverWaveform,
};
use manifold_core::effects::{EffectInstance, ParameterDriver, ParamEnvelope};
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
    ChangeQuantizeModeCommand, ChangeLayerBlendModeCommand,
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
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::service::EditingService;
use manifold_ui::{PanelAction, InspectorTab, DriverConfigAction};
use manifold_ui::node::Color32;
use manifold_ui::color;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::TrackInfo;
use manifold_ui::panels::effect_card::{EffectCardConfig, EffectParamInfo};
use manifold_ui::panels::gen_param::{GenParamConfig, GenParamInfo};


use crate::app::SelectionState;
use crate::dialog_path_memory::{self, DialogContext};
use crate::user_prefs::UserPrefs;
use crate::ui_root::UIRoot;

/// Result of dispatching a panel action.
pub struct DispatchResult {
    /// True if the action was handled.
    pub handled: bool,
    /// True if the action changed project structure (needs sync_project_data).
    pub structural_change: bool,
    /// True if the output resolution changed (needs compositor + generator resize).
    pub resolution_changed: bool,
}

impl DispatchResult {
    fn handled() -> Self { Self { handled: true, structural_change: false, resolution_changed: false } }
    fn structural() -> Self { Self { handled: true, structural_change: true, resolution_changed: false } }
    fn resolution() -> Self { Self { handled: true, structural_change: true, resolution_changed: true } }
    fn unhandled() -> Self { Self { handled: false, structural_change: false, resolution_changed: false } }
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
    active_layer: &mut Option<usize>,
    drag_snapshot: &mut Option<f32>,
    trim_snapshot: &mut Option<(f32, f32)>,
    adsr_snapshot: &mut Option<(f32, f32, f32, f32)>,
    target_snapshot: &mut Option<f32>,
    user_prefs: &mut UserPrefs,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Transport ──────────────────────────────────────────────
        PanelAction::PlayPause => {
            if content_state.is_playing {
                let _ = content_tx.try_send(ContentCommand::Pause);
            } else {
                if let Some(cursor_beat) = selection.insert_cursor_beat {
                    let _ = content_tx.try_send(ContentCommand::SeekToBeat(cursor_beat));
                }
                let _ = content_tx.try_send(ContentCommand::Play);
            }
            DispatchResult::handled()
        }
        PanelAction::Stop => {
            let _ = content_tx.try_send(ContentCommand::Stop);
            if let Some(cursor_beat) = selection.insert_cursor_beat {
                let _ = content_tx.try_send(ContentCommand::SeekToBeat(cursor_beat));
            }
            DispatchResult::handled()
        }
        PanelAction::Record => {
            let _ = content_tx.try_send(ContentCommand::SetRecording(!content_state.is_recording));
            DispatchResult::handled()
        }
        PanelAction::ResetBpm => {
            // Intercepted by Application before dispatch
            DispatchResult::handled()
        }
        PanelAction::ClearBpm => {
            {
                let old_points = project.tempo_map.clone_points();
                let bpm = project.settings.bpm;
                let cmd = manifold_editing::commands::settings::ClearTempoMapCommand::new(old_points, bpm);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::handled()
        }
        PanelAction::BpmFieldClicked => {
            log::debug!("BPM field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::Seek(beat) => {
            let _ = content_tx.try_send(ContentCommand::SeekToBeat(*beat));
            DispatchResult::handled()
        }
        PanelAction::OverviewScrub(norm) => {
            // Unity: ViewportManager.OnOverviewStripScrub — center viewport on click
            let ppb = ui.viewport.pixels_per_beat();
            let viewport_w = ui.viewport.get_tracks_rect().width;
            // Compute total content width from clips
            let max_beat = ui.viewport.max_content_beat().max(1.0);
            let content_w = max_beat * ppb;
            let target_scroll = (norm * content_w - viewport_w * 0.5) / ppb;
            let target_scroll = target_scroll.max(0.0);
            ui.viewport.set_scroll(target_scroll, ui.viewport.scroll_y_px());
            DispatchResult::structural()
        }
        PanelAction::SetInsertCursor(beat) => {
            // Legacy path — when no layer context available.
            // Uses set_insert_cursor_beat (non-clearing variant)
            // since we don't have a layer index here.
            selection.set_insert_cursor_beat(*beat);
            DispatchResult::structural()
        }

        // ── Viewport clip interaction ─────────────────────────────
        PanelAction::ClipClicked(clip_id, modifiers) => {
            // Find the clip's layer index and end beat for UIState
            let (layer_idx, clip_end_beat) = Some(&*project)
                .and_then(|p| p.timeline.layers.iter().enumerate()
                    .find_map(|(i, l)| l.clips.iter()
                        .find(|c| c.id == *clip_id)
                        .map(|c| (i, c.start_beat + c.duration_beats))))
                .unwrap_or((0, 0.0));

            if modifiers.shift {
                // Shift+Click: extend region from anchor to clip end.
                // From Unity InteractionOverlay.OnPointerClick (line 206-207).
                select_region_to_with_project(clip_end_beat, layer_idx, selection, &*project);
            } else if modifiers.command || modifiers.ctrl {
                // Cmd/Ctrl+Click: toggle clip in/out of selection, then update region bounds.
                // From Unity InteractionOverlay.OnPointerClick (line 208-211).
                selection.toggle_clip_selection(clip_id.clone(), layer_idx);
                // Update region to encompass all selected clips (Fix #3)
                update_region_from_clip_selection_inline(selection, &*project);
            } else {
                // Plain click: select single clip (clears region, layers, insert cursor)
                selection.select_clip(clip_id.clone(), layer_idx);
            }
            *active_layer = Some(layer_idx);
            DispatchResult::structural()
        }
        PanelAction::ClipDoubleClicked(_clip_id) => {
            // Future: open clip properties or enter clip editing mode
            DispatchResult::handled()
        }
        PanelAction::TrackClicked(beat, layer, modifiers) => {
            if modifiers.shift {
                // Shift+Click on empty area: extend region from anchor to beat/layer.
                // From Unity InteractionOverlay.OnPointerClick (line 177-180).
                let snapped = ui.viewport.snap_to_grid(*beat);
                select_region_to_with_project(snapped, *layer, selection, &*project);
            } else {
                // Plain click: set insert cursor (clears everything, Ableton behavior).
                // From Unity InteractionOverlay.OnPointerClick (line 183).
                selection.set_insert_cursor(*beat, *layer);
            }
            *active_layer = Some(*layer);
            DispatchResult::structural()
        }
        PanelAction::TrackDoubleClicked(beat, layer) => {
            // From Unity InteractionOverlay.OnPointerClick double-click path:
            // Use FloorBeatToGrid (grid cell start), NOT SnapBeatToGrid (nearest line).
            let grid_step = ui.viewport.grid_step();
            let snapped = manifold_ui::snap::floor_beat_to_grid(*beat, grid_step);
            {
                let (cmd, _clip_id) = EditingService::create_clip_at_position(project, snapped, *layer, 4.0);
                { let mut cmd = cmd; cmd.execute(project); let _ = content_tx.try_send(ContentCommand::Record(cmd)); }
                // Enforce non-overlap for the newly created clip
                if let Some(new_layer) = project.timeline.layers.get(*layer) {
                    if let Some(new_clip) = new_layer.clips.last() {
                        let new_clip_clone = new_clip.clone();
                        let ignore = std::collections::HashSet::new();
                        let spb = 60.0 / project.settings.bpm;
                        let overlap_cmds = EditingService::enforce_non_overlap(
                            project, &new_clip_clone, *layer, &ignore, spb,
                        );
                        for cmd in overlap_cmds {
                            { let mut cmd = cmd; cmd.execute(project); let _ = content_tx.try_send(ContentCommand::Record(cmd)); }
                        }
                        // Select the newly created clip
                        selection.select_clip(new_clip_clone.id, *layer);
                    }
                }
            }
            *active_layer = Some(*layer);
            DispatchResult::structural()
        }
        // ── Drag actions — handled by InteractionOverlay (not dispatch) ──
        // These PanelActions are no longer emitted; the overlay handles all
        // drag interaction directly through the TimelineEditingHost trait.
        PanelAction::ClipDragStarted(..) => DispatchResult::handled(),
        PanelAction::ClipDragMoved(..) => DispatchResult::handled(),
        PanelAction::ClipDragEnded => DispatchResult::handled(),
        PanelAction::RegionDragStarted(..) => DispatchResult::handled(),
        PanelAction::RegionDragMoved(..) => DispatchResult::handled(),
        PanelAction::RegionDragEnded => DispatchResult::handled(),
        PanelAction::ViewportHoverChanged(_clip_id) => {
            // Hover state is already tracked on viewport panel
            DispatchResult::handled()
        }

        // ── Clock/Sync (handled at Application level, these are fallbacks) ──
        PanelAction::CycleClockAuthority
        | PanelAction::ToggleLink
        | PanelAction::ToggleMidiClock
        | PanelAction::ToggleSyncOutput => {
            // Intercepted by Application before dispatch — should not reach here
            DispatchResult::handled()
        }
        PanelAction::SelectClkDevice => {
            log::info!("Select clock device (dropdown not yet implemented)");
            DispatchResult::handled()
        }

        // ── Export/Header/Footer ───────────────────────────────────
        PanelAction::ToggleHdr => {
            {
                project.settings.export_hdr = !project.settings.export_hdr;
                log::info!("HDR export → {}", project.settings.export_hdr);
            }
            DispatchResult::handled()
        }
        PanelAction::TogglePercussion => {
            // Unity: PERC button → percussionImportController.OnImportPercussionMap()
            // Same as ImportAudioClicked — open file dialog for audio analysis.
            let last_dir = dialog_path_memory::get_last_directory(
                DialogContext::PercussionImport, user_prefs,
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import Audio for Percussion Analysis")
                .add_filter("Audio Files", &["wav", "mp3", "m4a", "aac", "flac", "ogg", "aif", "aiff", "wma", "json"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::PercussionImport, &path_str, user_prefs,
                );
                // Send percussion import request to content thread
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |_p| {
                    log::info!("[Percussion] Import requested for: {}", path_str);
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::ToggleMonitor => {
            // Deferred — needs ActiveEventLoop which is only available in about_to_wait.
            // Set flag; app.rs processes it in the next frame.
            DispatchResult {
                handled: true,
                structural_change: false,
                resolution_changed: false,
            }
        }
        PanelAction::CycleQuantize => {
            {
                let old = project.settings.quantize_mode;
                let new = old.next();
                let cmd = ChangeQuantizeModeCommand::new(old, new);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::handled()
        }
        PanelAction::ResolutionClicked => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::FpsFieldClicked => {
            log::debug!("FPS field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }

        // ── Zoom ───────────────────────────────────────────────────
        PanelAction::ZoomIn => {
            let ppb = ui.viewport.pixels_per_beat();
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let current_idx = levels.iter()
                .position(|&l| (l - ppb).abs() < 0.01)
                .unwrap_or(manifold_ui::color::DEFAULT_ZOOM_INDEX);
            let new_idx = (current_idx + 1).min(levels.len() - 1);
            if new_idx != current_idx {
                ui.viewport.set_zoom(levels[new_idx]);
            }
            DispatchResult::structural()
        }
        PanelAction::ZoomOut => {
            let ppb = ui.viewport.pixels_per_beat();
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let current_idx = levels.iter()
                .position(|&l| (l - ppb).abs() < 0.01)
                .unwrap_or(manifold_ui::color::DEFAULT_ZOOM_INDEX);
            let new_idx = current_idx.saturating_sub(1);
            if new_idx != current_idx {
                ui.viewport.set_zoom(levels[new_idx]);
            }
            DispatchResult::structural()
        }

        // ── Inspector navigation ───────────────────────────────────
        PanelAction::SelectInspectorTab(tab) => {
            log::debug!("Inspector tab: {:?}", tab);
            DispatchResult::handled()
        }
        PanelAction::InspectorScrolled(delta) => {
            ui.inspector.handle_scroll(*delta);
            DispatchResult::handled()
        }
        PanelAction::InspectorSectionClicked(idx) => {
            log::debug!("Inspector section clicked: {}", idx);
            DispatchResult::handled()
        }

        // ── Layer operations ───────────────────────────────────────
        PanelAction::ToggleMute(idx) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_muted = !layer.is_muted;
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ToggleSolo(idx) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_solo = !layer.is_solo;
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerClicked(idx, modifiers) => {
            // From Unity UIState.cs layer selection methods (lines 247-333).
            *active_layer = Some(*idx);

            {
                let layer_id = project.timeline.layers.get(*idx)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();

                if modifiers.shift {
                    // Shift+Click: range select from primary to target
                    selection.select_layer_range(&layer_id, &project.timeline.layers);
                } else if modifiers.ctrl || modifiers.command {
                    // Cmd/Ctrl+Click: toggle layer in/out of selection
                    selection.toggle_layer_selection(layer_id);
                } else {
                    // Plain click: select single layer (clears clips, region, insert cursor)
                    selection.select_layer(layer_id);
                }
            }

            DispatchResult::structural()
        }
        PanelAction::LayerDoubleClicked(_idx) => {
            // Intercepted by app.rs — opens text input for layer rename
            DispatchResult::handled()
        }
        PanelAction::ChevronClicked(idx) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_collapsed = !layer.is_collapsed;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::BlendModeClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::SetBlendMode(idx, mode_str) => {
            {
                if let Some(layer) = project.timeline.layers.get(*idx) {
                    let old_mode = layer.default_blend_mode;
                    if let Some(new_mode) = BlendMode::ALL.iter().find(|m| format!("{:?}", m) == *mode_str) {
                        let cmd = ChangeLayerBlendModeCommand::new(*idx, old_mode, *new_mode);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ExpandLayer(idx) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_collapsed = false;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::CollapseLayer(idx) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_collapsed = true;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::FolderClicked(_idx) => {
            log::info!("Folder clicked (file picker not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::NewClipClicked(idx) => {
            let beat = content_state.current_beat;
            {
                let (cmd, _) = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                { let mut cmd = cmd; cmd.execute(project); let _ = content_tx.try_send(ContentCommand::Record(cmd)); }
            }
            DispatchResult::structural()
        }
        PanelAction::AddGenClipClicked(idx) => {
            let beat = content_state.current_beat;
            {
                let (cmd, _) = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                { let mut cmd = cmd; cmd.execute(project); let _ = content_tx.try_send(ContentCommand::Record(cmd)); }
            }
            DispatchResult::structural()
        }
        PanelAction::MidiInputClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::MidiChannelClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::LayerDragStarted(_idx) => {
            DispatchResult::handled()
        }
        PanelAction::LayerDragMoved(_from, _to) => {
            DispatchResult::handled()
        }
        PanelAction::LayerDragEnded(from, to) => {
            // From Unity LayerHeaderPanel.HandleDragEnd + ReorderLayerCommand.
            // Atomically reorder layers and update parent_layer_id when moving into/out of groups.
            if from != to {
                {
                    let old_order = project.timeline.layers.clone();
                    let mut new_order = old_order.clone();

                    if *from < new_order.len() && *to <= new_order.len() {
                        let layer = new_order.remove(*from);
                        let insert_at = if *to > *from { to.saturating_sub(1) } else { *to };
                        let insert_at = insert_at.min(new_order.len());

                        // Determine parent group for the target position
                        let target_parent = if insert_at < new_order.len() {
                            new_order[insert_at].parent_layer_id.clone()
                        } else if !new_order.is_empty() {
                            new_order.last().and_then(|l| l.parent_layer_id.clone())
                        } else {
                            None
                        };

                        new_order.insert(insert_at, layer);

                        // Build parent ID maps for undo
                        let mut old_parents = std::collections::HashMap::new();
                        let mut new_parents = std::collections::HashMap::new();
                        for (_i, l) in old_order.iter().enumerate() {
                            old_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                        }
                        // Update moved layer's parent
                        for l in &new_order {
                            new_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                        }
                        let moved_id = new_order[insert_at].layer_id.clone();
                        new_parents.insert(moved_id, target_parent);

                        let cmd = manifold_editing::commands::layer::ReorderLayerCommand::new(
                            old_order, new_order, old_parents, new_parents,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }

        // ── Layer management ───────────────────────────────────────
        PanelAction::AddLayerClicked => {
            {
                let count = project.timeline.layers.len();
                let name = format!("Layer {}", count + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorType::None,
                    count,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::DeleteLayerClicked(idx) => {
            {
                if project.timeline.layers.len() > 1 {
                    if let Some(layer) = project.timeline.layers.get(*idx) {
                        let layer_clone = layer.clone();
                        let cmd = DeleteLayerCommand::new(layer_clone, *idx);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                        // Fix active_layer if needed
                        if let Some(al) = active_layer {
                            if *al >= project.timeline.layers.len() {
                                *active_layer = Some(project.timeline.layers.len().saturating_sub(1));
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }

        // ── Master chrome ──────────────────────────────────────────
        PanelAction::MasterOpacitySnapshot => {
            {
                *drag_snapshot = Some(project.settings.master_opacity);
            }
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            {
                project.settings.master_opacity = *val;
            }
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                {
                    let new_val = project.settings.master_opacity;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                        let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                    }
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
            // Reset master opacity to 1.0
            {
                let old = project.settings.master_opacity;
                if (old - 1.0).abs() > f32::EPSILON {
                    project.settings.master_opacity = 1.0;
                    let cmd = ChangeMasterOpacityCommand::new(old, 1.0);
                    let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── Layer chrome ───────────────────────────────────────────
        PanelAction::LayerOpacitySnapshot => {
            if let Some(idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get(idx) {
                        *drag_snapshot = Some(layer.opacity);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityChanged(val) => {
            if let Some(idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(idx) {
                        layer.opacity = *val;
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(idx) = *active_layer {
                    {
                        if let Some(layer) = project.timeline.layers.get(idx) {
                            let new_val = layer.opacity;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = ChangeLayerOpacityCommand::new(idx, old_val, new_val);
                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerChromeCollapseToggle => {
            ui.inspector.layer_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::LayerOpacityRightClick => {
            // Reset layer opacity to 1.0
            if let Some(idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(idx) {
                        let old = layer.opacity;
                        if (old - 1.0).abs() > f32::EPSILON {
                            layer.opacity = 1.0;
                            let cmd = ChangeLayerOpacityCommand::new(idx, old, 1.0);
                            let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                        }
                    }
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
            // Intercepted by app.rs — opens text input overlay
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                {
                    if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                        let old_loop = clip.is_looping;
                        let old_dur = clip.loop_duration_beats;
                        let cmd = ChangeClipLoopCommand::new(
                            clip_id, old_loop, !old_loop, old_dur, old_dur,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipSlipSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                {
                    if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                        *drag_snapshot = Some(clip.in_point);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                        clip.in_point = val.max(0.0);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(clip_id) = &selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    {
                        if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                            let new_val = clip.in_point;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = SlipClipCommand::new(clip_id, old_val, new_val);
                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                {
                    if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                        *drag_snapshot = Some(clip.loop_duration_beats);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopChanged(val) => {
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                        clip.loop_duration_beats = val.max(0.0);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(clip_id) = &selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    {
                        if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                            let new_val = clip.loop_duration_beats;
                            let is_looping = clip.is_looping;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = ChangeClipLoopCommand::new(
                                    clip_id, is_looping, is_looping, old_val, new_val,
                                );
                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        PanelAction::ClipSlipRightClick => {
            // Reset clip slip (in_point) to 0.0
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                        let old = clip.in_point;
                        if old.abs() > f32::EPSILON {
                            clip.in_point = 0.0;
                            let cmd = SlipClipCommand::new(clip_id, old, 0.0);
                            let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopRightClick => {
            // Reset clip loop duration to clip's full duration
            if let Some(clip_id) = &selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                        let old_dur = clip.loop_duration_beats;
                        let full_dur = clip.duration_beats;
                        let is_looping = clip.is_looping;
                        if (old_dur - full_dur).abs() > f32::EPSILON {
                            clip.loop_duration_beats = full_dur;
                            let cmd = ChangeClipLoopCommand::new(
                                clip_id, is_looping, is_looping, old_dur, full_dur,
                            );
                            let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect operations ──────────────────────────────────────
        // NOTE: All effect actions route through last_effect_tab() to
        // write to the correct location (master / layer / clip effects).
        PanelAction::EffectToggle(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
                if let Some(effects) = effects_ref {
                    if let Some(fx) = effects.get(*fx_idx) {
                        let old = fx.enabled;
                        let cmd = ToggleEffectCommand::new(target, *fx_idx, old, !old);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(fx_idx) => {
            // Unity: EffectCardPresenter.OnToggleCardCollapse — mutates effect.collapsed
            // on the data model, then requests rebuild so card height recalculates.
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*fx_idx) {
                        fx.collapsed = !fx.collapsed;
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectCardClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::EffectParamRightClick(fx_idx, param_idx, default_val) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*fx_idx) {
                        let old = fx.get_base_param(*param_idx);
                        if (old - *default_val).abs() > f32::EPSILON {
                            fx.set_base_param(*param_idx, *default_val);
                            let cmd = ChangeEffectParamCommand::new(
                                target, *fx_idx, *param_idx, old, *default_val,
                            );
                            let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamSnapshot(fx_idx, param_idx) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                    *drag_snapshot = Some(fx.get_base_param(*param_idx));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamChanged(fx_idx, param_idx, val) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*fx_idx) {
                        fx.set_base_param(*param_idx, *val);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamCommit(fx_idx, param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                        let new_val = fx.get_base_param(*param_idx);
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let target = resolve_effect_target(tab, *active_layer);
                            let cmd = ChangeEffectParamCommand::new(
                                target, *fx_idx, *param_idx, old_val, new_val,
                            );
                            let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        // Unity: EffectCardPresenter.ToggleEffectDriverConfig — routes via
        // IEffectCardHost which resolves to Master/Layer/Clip context.
        PanelAction::EffectDriverToggle(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let effect_target = resolve_effect_target(tab, *active_layer);
                let (effects_ref, _) = resolve_effects_read(tab, project, *active_layer, selection);
                if let Some(effects) = effects_ref {
                    if let Some(fx) = effects.get(*ei) {
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
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
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
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvelopeToggle(ei, pi) => {
            // Unity: EffectCardPresenter.ToggleEffectEnvelopeConfig
            // Envelopes live on the container (layer/clip), not the effect instance.
            // Route via last_effect_tab to support master/layer/clip.
            let tab = ui.inspector.last_effect_tab();
            {
                // Get the effect type first (immutable access)
                let effect_type = {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
                };
                if let Some(et) = effect_type {
                    // Get the envelope container (mutable access)
                    // Master doesn't have envelopes in Unity — only layer and clip do.
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
                        InspectorTab::Master => None, // Master has no envelopes
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
            }
            DispatchResult::structural()
        }
        PanelAction::EffectDriverConfig(ei, pi, cfg) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let effect_target = resolve_effect_target(tab, *active_layer);
                let target = DriverTarget::Effect {
                    effect_target,
                    effect_index: *ei,
                };
                {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    if let Some(fx) = effects.and_then(|e| e.get(*ei)) {
                        if let Some(di) = fx.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                        {
                            let driver = &fx.drivers.as_ref().unwrap()[di];
                            match cfg {
                                DriverConfigAction::BeatDiv(idx) => {
                                    if let Some(new_div) = BeatDivision::from_button_index(*idx) {
                                        let cmd = ChangeDriverBeatDivCommand::new(
                                            target, di, driver.beat_division, new_div,
                                        );
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                    }
                                }
                                DriverConfigAction::Wave(idx) => {
                                    if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                        let cmd = ChangeDriverWaveformCommand::new(
                                            target, di, driver.waveform, new_wave,
                                        );
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                    }
                                }
                                DriverConfigAction::Dot => {
                                    if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                        let cmd = ChangeDriverBeatDivCommand::new(
                                            target, di, driver.beat_division, new_div,
                                        );
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                    }
                                }
                                DriverConfigAction::Triplet => {
                                    if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                        let cmd = ChangeDriverBeatDivCommand::new(
                                            target, di, driver.beat_division, new_div,
                                        );
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                    }
                                }
                                DriverConfigAction::Reverse => {
                                    let cmd = ToggleDriverReversedCommand::new(
                                        target, di, driver.reversed, !driver.reversed,
                                    );
                                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvParamChanged(ei, pi, param, val) => {
            // Live ADSR mutation during drag — routes via last_effect_tab.
            let tab = ui.inspector.last_effect_tab();
            {
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
                    if let Some(envs) = envs {
                        if let Some(env) = envs.iter_mut().find(|e|
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
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTrimChanged(ei, pi, min, max) => {
            // Live trim mutation during drag — routes via last_effect_tab.
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_mut, _) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*ei) {
                        if let Some(driver) = fx.drivers_mut().iter_mut()
                            .find(|d| d.param_index == *pi as i32)
                        {
                            driver.trim_min = *min;
                            driver.trim_max = *max;
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetChanged(ei, pi, norm) => {
            // Live target normalized mutation during drag — routes via last_effect_tab.
            let tab = ui.inspector.last_effect_tab();
            {
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
                    if let Some(envs) = envs {
                        if let Some(env) = envs.iter_mut().find(|e|
                            e.target_effect_type == et && e.param_index == *pi as i32
                        ) {
                            env.target_normalized = *norm;
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Modulation undo: snapshot/commit ────────────────────────
        // Unity: onTrimSnapshot/onTrimCommit, onTargetSnapshot/onTargetCommit,
        //        onEnvConfigSnapshot/onEnvConfigCommit
        PanelAction::EffectTrimSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*ei)) {
                    if let Some(driver) = fx.drivers.as_ref()
                        .and_then(|ds| ds.iter().find(|d| d.param_index == *pi as i32))
                    {
                        *trim_snapshot = Some((driver.trim_min, driver.trim_max));
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTrimCommit(ei, pi) => {
            if let Some((old_min, old_max)) = trim_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                {
                    let effect_target = resolve_effect_target(tab, *active_layer);
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    if let Some(fx) = effects.and_then(|e| e.get(*ei)) {
                        if let Some(di) = fx.drivers.as_ref()
                            .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                        {
                            let driver = &fx.drivers.as_ref().unwrap()[di];
                            let new_min = driver.trim_min;
                            let new_max = driver.trim_max;
                            if (old_min - new_min).abs() > f32::EPSILON || (old_max - new_max).abs() > f32::EPSILON {
                                let target = DriverTarget::Effect { effect_target, effect_index: *ei };
                                let cmd = ChangeTrimCommand::new(target, di, old_min, old_max, new_min, new_max);
                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            {
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
                    if let Some(envs) = envs {
                        if let Some(env) = envs.iter().find(|e|
                            e.target_effect_type == et && e.param_index == *pi as i32
                        ) {
                            *target_snapshot = Some(env.target_normalized);
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetCommit(ei, pi) => {
            // Unity: CommitEnvelopeTargetUndo — records ChangeParamEnvelopeCommand.
            if let Some(old_target) = target_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                {
                    let effect_type = {
                        let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                        effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
                    };
                    if let Some(et) = effect_type {
                        match tab {
                            InspectorTab::Layer => {
                                if let Some(idx) = *active_layer {
                                    if let Some(layer) = project.timeline.layers.get(idx) {
                                        let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                        if let Some((env_idx, env)) = envs.iter().enumerate()
                                            .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                        {
                                            if (old_target - env.target_normalized).abs() > f32::EPSILON {
                                                let cmd = ChangeLayerEnvelopeTargetCommand::new(idx, env_idx, old_target, env.target_normalized);
                                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                                            }
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
                                            if (old_target - env.target_normalized).abs() > f32::EPSILON {
                                                let cmd = ChangeEnvelopeTargetNormalizedCommand::new(clip_id.clone(), env_idx, old_target, env.target_normalized);
                                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                                            }
                                        }
                                    }
                                }
                            }
                            InspectorTab::Master => {}
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamSnapshot(ei, pi) => {
            let tab = ui.inspector.last_effect_tab();
            {
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
                    if let Some(envs) = envs {
                        if let Some(env) = envs.iter().find(|e|
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
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamCommit(ei, pi) => {
            // Unity: CommitEnvelopeConfigUndo — records ChangeParamEnvelopeCommand.
            if let Some((old_a, old_d, old_s, old_r)) = adsr_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                {
                    let effect_type = {
                        let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                        effects.and_then(|e| e.get(*ei)).map(|fx| fx.effect_type)
                    };
                    if let Some(et) = effect_type {
                        match tab {
                            InspectorTab::Layer => {
                                if let Some(idx) = *active_layer {
                                    if let Some(layer) = project.timeline.layers.get(idx) {
                                        let envs = layer.envelopes.as_deref().unwrap_or(&[]);
                                        if let Some((env_idx, env)) = envs.iter().enumerate()
                                            .find(|(_, e)| e.target_effect_type == et && e.param_index == *pi as i32)
                                        {
                                            let (na, nd, ns, nr) = (env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats);
                                            if (old_a - na).abs() > f32::EPSILON || (old_d - nd).abs() > f32::EPSILON
                                                || (old_s - ns).abs() > f32::EPSILON || (old_r - nr).abs() > f32::EPSILON
                                            {
                                                let cmd = ChangeLayerEnvelopeADSRCommand::new(idx, env_idx, old_a, old_d, old_s, old_r, na, nd, ns, nr);
                                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                                            }
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
                                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                                            }
                                        }
                                    }
                                }
                            }
                            InspectorTab::Master => {}
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => {
            // Intercepted by UIRoot::try_open_dropdown (opens browser popup at button).
            DispatchResult::handled()
        }
        PanelAction::BrowserSearchClicked => {
            // Intercepted by app.rs — opens text input for browser search
            DispatchResult::handled()
        }
        PanelAction::RemoveEffect(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
                if let Some(effects) = effects_ref {
                    if let Some(fx) = effects.get(*fx_idx) {
                        let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectReorder(from_idx, to_idx) => {
            let tab = ui.inspector.last_effect_tab();
            {
                let target = resolve_effect_target(tab, *active_layer);
                let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::structural()
        }

        // ── Generator params ───────────────────────────────────────
        PanelAction::GenTypeClicked => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::GenParamSnapshot(param_idx) => {
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(gp) = &layer.gen_params {
                            *drag_snapshot = Some(gp.get_param_base(*param_idx));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamChanged(param_idx, val) => {
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            gp.set_param_base(*param_idx, *val);
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamCommit(param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(layer_idx) = *active_layer {
                    {
                        if let Some(layer) = project.timeline.layers.get(layer_idx) {
                            if let Some(gp) = &layer.gen_params {
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
                                    let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamToggle(param_idx) => {
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            let cur = gp.get_param_base(*param_idx);
                            gp.set_param_base(*param_idx, if cur > 0.5 { 0.0 } else { 1.0 });
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamRightClick(param_idx, default_val) => {
            // Reset generator param to its default value
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
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
                                let _ = content_tx.try_send(ContentCommand::Record(Box::new(cmd)));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Gen modulation ─────────────────────────────────────────
        PanelAction::GenDriverToggle(pi) => {
            if let Some(layer_idx) = *active_layer {
                {
                    let target = DriverTarget::GeneratorParam { layer_index: layer_idx };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(gp) = &layer.gen_params {
                            let driver_idx = gp.drivers.as_ref()
                                .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32));
                            if let Some(di) = driver_idx {
                                let old = gp.drivers.as_ref().unwrap()[di].enabled;
                                let cmd = ToggleDriverEnabledCommand::new(
                                    target, di, old, !old,
                                );
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
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
                                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvelopeToggle(pi) => {
            // Gen param envelopes live on GeneratorParamState.envelopes.
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
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
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::GenDriverConfig(pi, cfg) => {
            if let Some(layer_idx) = *active_layer {
                {
                    let target = DriverTarget::GeneratorParam { layer_index: layer_idx };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(gp) = &layer.gen_params {
                            if let Some(di) = gp.drivers.as_ref()
                                .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32))
                            {
                                let driver = &gp.drivers.as_ref().unwrap()[di];
                                match cfg {
                                    DriverConfigAction::BeatDiv(idx) => {
                                        if let Some(new_div) = BeatDivision::from_button_index(*idx) {
                                            let cmd = ChangeDriverBeatDivCommand::new(
                                                target, di, driver.beat_division, new_div,
                                            );
                                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                        }
                                    }
                                    DriverConfigAction::Wave(idx) => {
                                        if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                            let cmd = ChangeDriverWaveformCommand::new(
                                                target, di, driver.waveform, new_wave,
                                            );
                                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                        }
                                    }
                                    DriverConfigAction::Dot => {
                                        if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                            let cmd = ChangeDriverBeatDivCommand::new(
                                                target, di, driver.beat_division, new_div,
                                            );
                                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                        }
                                    }
                                    DriverConfigAction::Triplet => {
                                        if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                            let cmd = ChangeDriverBeatDivCommand::new(
                                                target, di, driver.beat_division, new_div,
                                            );
                                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                        }
                                    }
                                    DriverConfigAction::Reverse => {
                                        let cmd = ToggleDriverReversedCommand::new(
                                            target, di, driver.reversed, !driver.reversed,
                                        );
                                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::GenEnvParamChanged(pi, param, val) => {
            // Live ADSR mutation during drag.
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            if let Some(envs) = &mut gp.envelopes {
                                if let Some(env) = envs.iter_mut()
                                    .find(|e| e.param_index == *pi as i32)
                                {
                                    match param {
                                        manifold_ui::EnvelopeParam::Attack => env.attack_beats = *val,
                                        manifold_ui::EnvelopeParam::Decay => env.decay_beats = *val,
                                        manifold_ui::EnvelopeParam::Sustain => env.sustain_level = *val,
                                        manifold_ui::EnvelopeParam::Release => env.release_beats = *val,
                                    }
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenTrimChanged(pi, min, max) => {
            // Live trim mutation during drag.
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            if let Some(drivers) = &mut gp.drivers {
                                if let Some(driver) = drivers.iter_mut()
                                    .find(|d| d.param_index == *pi as i32)
                                {
                                    driver.trim_min = *min;
                                    driver.trim_max = *max;
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenTargetChanged(pi, norm) => {
            // Live target normalized mutation during drag.
            if let Some(layer_idx) = *active_layer {
                {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            if let Some(envs) = &mut gp.envelopes {
                                if let Some(env) = envs.iter_mut()
                                    .find(|e| e.param_index == *pi as i32)
                                {
                                    env.target_normalized = *norm;
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // File operations intercepted in app.rs action loop — should not reach here.
        // Export stubs remain until export pipeline is ported.
        PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs => {
            log::warn!("File action {:?} reached ui_bridge (should be intercepted in app.rs)", action);
            DispatchResult::handled()
        }
        PanelAction::ExportVideo
        | PanelAction::ExportXml => {
            log::info!("Export action: {:?} (not yet wired)", action);
            DispatchResult::handled()
        }

        // ── Dropdown results (context-routed from UIRoot) ────────────
        PanelAction::SetMidiNote(layer_idx, note) => {
            {
                if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                    let old_note = layer.midi_note;
                    let cmd = manifold_editing::commands::settings::ChangeLayerMidiNoteCommand::new(
                        *layer_idx, old_note, *note,
                    );
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiChannel(layer_idx, channel) => {
            {
                if let Some(layer) = project.timeline.layers.get_mut(*layer_idx) {
                    layer.midi_channel = *channel;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetResolution(preset_idx) => {
            use manifold_core::types::ResolutionPreset;
            {
                let old = project.settings.resolution_preset;
                if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                    let cmd = manifold_editing::commands::settings::ChangeResolutionCommand::new(old, new);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                }
            }
            DispatchResult::resolution()
        }
        PanelAction::SetDisplayResolution(w, h) => {
            // Direct mutation — no undo, matches Unity display resolution selection.
            {
                project.settings.output_width = *w;
                project.settings.output_height = *h;
            }
            DispatchResult::resolution()
        }
        PanelAction::AddEffect(tab, effect_type_idx) => {
            use manifold_core::types::EffectType;
            let Some(effect_type) = EffectType::from_index(*effect_type_idx) else {
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
                    // Clip-level effects need active clip tracking (future)
                    log::debug!("Add effect to clip (clip selection not yet implemented)");
                    return DispatchResult::handled();
                }
            };
            {
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
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::SetGenType(layer_idx, gen_type_idx) => {
            {
                if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                    let old_type = layer.gen_params.as_ref()
                        .map(|gp| gp.generator_type)
                        .unwrap_or(GeneratorType::None);
                    if let Some(new_type) = GeneratorType::from_index(*gen_type_idx) {
                        let old_params = layer.gen_params.as_ref()
                            .map(|gp| gp.param_values.clone())
                            .unwrap_or_default();
                        let old_drivers = layer.gen_params.as_ref()
                            .and_then(|gp| gp.drivers.clone());
                        let old_envelopes = layer.gen_params.as_ref()
                            .and_then(|gp| gp.envelopes.clone());
                        let cmd = manifold_editing::commands::settings::ChangeGeneratorTypeCommand::new(
                            *layer_idx, old_type, new_type, old_params, old_drivers, old_envelopes,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }

        // Right-click actions (intercepted by UIRoot for dropdown; should not reach dispatch)
        PanelAction::ClipRightClicked(_) | PanelAction::TrackRightClicked(_, _) => {
            DispatchResult::handled()
        }

        // ── Context menu actions ──────────────────────────────────
        PanelAction::ContextSplitAtPlayhead(clip_id) => {
            let beat = content_state.current_beat;
            {
                let spb = 60.0 / project.settings.bpm;
                if let Some(cmd) = EditingService::split_clip_at_beat(project, clip_id, beat, spb) {
                    { let mut cmd = cmd; cmd.execute(project); let _ = content_tx.try_send(ContentCommand::Record(cmd)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextDeleteClip(clip_id) => {
            {
                let commands = EditingService::delete_clips(project, &[clip_id.clone()], None, 0.0);
                if !commands.is_empty() {
                    for mut c in commands { c.execute(project); let _ = content_tx.try_send(ContentCommand::Record(c)); }
                }
            }
            selection.selected_clip_ids.remove(clip_id);
            DispatchResult::structural()
        }
        PanelAction::ContextDuplicateClip(clip_id) => {
            {
                // Calculate region from the single clip for proper offset
                let mut region = manifold_core::selection::SelectionRegion::default();
                if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    region.start_beat = clip.start_beat;
                    region.end_beat = clip.start_beat + clip.duration_beats;
                    region.is_active = true;
                }
                let spb = 60.0 / project.settings.bpm.max(1.0);
                let commands = EditingService::duplicate_clips(project, &[clip_id.clone()], &region, spb);
                if !commands.is_empty() {
                    for mut c in commands { c.execute(project); let _ = content_tx.try_send(ContentCommand::Record(c)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextPasteAtTrack(beat, _layer) => {
            let _snapped = ui.viewport.snap_to_grid(*beat);
            {
                let _spb = 60.0 / project.settings.bpm;
                // TODO: browser paste not yet wired
                let result = manifold_editing::service::PasteResult { commands: Vec::new(), pasted_clip_ids: Vec::new(), skip_reason: None, skipped_count: 0 };
                if !result.commands.is_empty() {
                    for mut c in result.commands { c.execute(project); let _ = content_tx.try_send(ContentCommand::Record(c)); }
                    selection.selected_clip_ids.clear();
                    for id in result.pasted_clip_ids {
                        selection.selected_clip_ids.insert(id);
                    }
                    selection.primary_selected_clip_id = selection.selected_clip_ids.iter().next().cloned();
                    selection.selection_version += 1;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddVideoLayer(after_layer) => {
            {
                let idx = after_layer + 1;
                let name = format!("Layer {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorType::None,
                    idx,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddGeneratorLayer(after_layer) => {
            {
                let idx = after_layer + 1;
                let name = format!("Gen {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Generator,
                    GeneratorType::Plasma,
                    idx,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
            }
            DispatchResult::structural()
        }

        PanelAction::ContextDeleteLayer(layer_idx) => {
            {
                let idx = *layer_idx;
                if project.timeline.layers.len() > 1 && idx < project.timeline.layers.len() {
                    let layer = project.timeline.layers[idx].clone();
                    let cmd = DeleteLayerCommand::new(layer, idx);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Record(boxed)); }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::LayerHeaderRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown — should not reach dispatch
            DispatchResult::handled()
        }

        // Context menu items — not yet wired to subsystems
        PanelAction::ContextPasteAtLayer(layer_idx) => {
            // TODO: Wire to EditingService.Paste when clipboard is ported
            log::warn!("Paste at layer {} — not yet implemented", layer_idx);
            DispatchResult::handled()
        }
        PanelAction::ContextImportMidi(layer_idx) => {
            // TODO: Wire to MIDI import when subsystem is ported
            log::warn!("Import MIDI to layer {} — not yet implemented", layer_idx);
            DispatchResult::handled()
        }
        PanelAction::ContextGroupSelectedLayers => {
            // TODO: Wire to EditingService.GroupSelectedLayers
            log::warn!("Group selected layers — not yet implemented");
            DispatchResult::handled()
        }
        PanelAction::ContextUngroup(layer_idx) => {
            // TODO: Wire to EditingService.Ungroup
            log::warn!("Ungroup layer {} — not yet implemented", layer_idx);
            DispatchResult::handled()
        }

        // ── Waveform lane ─────────────────────────────────────────
        PanelAction::ImportAudioClicked => {
            // Open native file dialog for audio import.
            // Port of Unity WorkspaceController → percussionImportController.OnImportPercussionMap.
            let last_dir = dialog_path_memory::get_last_directory(
                DialogContext::PercussionImport, user_prefs,
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import Audio for Percussion Analysis")
                .add_filter("Audio Files", &["wav", "mp3", "m4a", "aac", "flac", "ogg", "aif", "aiff", "wma", "json"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::PercussionImport, &path_str, user_prefs,
                );
                // Send percussion import request to content thread
                let _ = content_tx.try_send(ContentCommand::MutateProject(Box::new(move |_p| {
                    log::info!("[Percussion] Import requested for: {}", path_str);
                })));
            }
            DispatchResult::handled()
        }
        PanelAction::RemoveAudioClicked => {
            log::info!("Remove audio clicked");
            // Send to content thread
            let _ = content_tx.try_send(ContentCommand::ResetAudio);
            ui.waveform_lane.clear_audio();
            ui.stem_lanes.clear_all_stems();
            ui.layout.waveform_lane_visible = true;
            DispatchResult::handled()
        }
        PanelAction::WaveformScrub(screen_x, _screen_y) => {
            let beat = ui.viewport.pixel_to_beat(*screen_x).max(0.0);
            let _ = content_tx.try_send(ContentCommand::SeekToBeat(beat));
            DispatchResult::handled()
        }
        PanelAction::WaveformDragDelta(delta_beats) => {
            // Move waveform start beat by delta
            {
                if let Some(state) = project.percussion_import.as_mut() {
                    state.audio_start_beat = (state.audio_start_beat + *delta_beats).max(0.0);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::WaveformDragEnd(_total_delta) => {
            // Drag finished — start beat already updated incrementally
            DispatchResult::handled()
        }
        PanelAction::ExpandStemsToggled(expanded) => {
            ui.waveform_lane.set_expanded_state(*expanded);
            ui.stem_lanes.set_expanded(*expanded);
            ui.layout.stem_lanes_expanded = *expanded;
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeDrums => {
            // Re-analyze runs on content thread
            log::info!("[Percussion] Re-analyze drums requested");
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeBass => {
            log::info!("[Percussion] Re-analyze bass requested");
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeSynth => {
            log::info!("[Percussion] Re-analyze synth requested");
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeVocal => {
            log::info!("[Percussion] Re-analyze vocal requested");
            DispatchResult::handled()
        }
        PanelAction::ReImportStems => {
            log::info!("[Percussion] Re-import stems requested");
            DispatchResult::handled()
        }
        PanelAction::StemMuteToggled(stem_index) => {
            // Toggle mute for stem
            log::info!("Stem {} mute toggled — stem audio not yet wired", stem_index);
            DispatchResult::handled()
        }
        PanelAction::StemSoloToggled(stem_index) => {
            // Toggle solo for stem
            log::info!("Stem {} solo toggled — stem audio not yet wired", stem_index);
            DispatchResult::handled()
        }

        // Generic dropdown fallback (should not normally fire)
        PanelAction::DropdownSelected(index) => {
            log::debug!("Dropdown selected: {} (no context)", index);
            DispatchResult::handled()
        }
    }
}

/// Update the selection region to encompass all currently selected clips.
/// Called after Ctrl+Click multi-select, paste, and duplicate.
/// From Unity InteractionOverlay.UpdateRegionFromClipSelection.
fn update_region_from_clip_selection(selection: &mut SelectionState, project: &Project) {
    if selection.selected_clip_ids.len() < 2 {
        // Single or no clips — no region needed
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
            selection.set_region_from_clip_bounds(min_beat, max_beat, min_layer, max_layer);
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
        selection.set_region_from_clip_bounds(min_beat, max_beat, min_layer, max_layer);
    }
}

/// Shift+Click region selection with correct anchor precedence.
/// Port of Unity EditingService.SelectRegionTo (lines 216-262).
/// Variant for call sites that have engine access instead of TimelineEditingHost trait.
/// Anchor priority: insert cursor > existing region > primary selected clip > fallback.
fn select_region_to_with_project(
    target_beat: f32,
    target_layer: usize,
    selection: &mut SelectionState,
    project: &Project,
) {
    let layer_count = project.timeline.layers.len();
    if layer_count == 0 { return; }

    // Determine anchor — Unity priority: insert cursor > region > primary clip > fallback
    let anchor: Option<(f32, usize)> = if selection.has_insert_cursor() {
        Some((
            selection.insert_cursor_beat.unwrap_or(0.0),
            selection.insert_cursor_layer_index.unwrap_or(0),
        ))
    } else if selection.has_region() {
        let r = selection.get_region();
        Some((r.start_beat, r.start_layer_index as usize))
    } else if let Some(ref clip_id) = selection.primary_selected_clip_id.clone() {
        // Look up primary clip via project data
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
            selection.set_region(min_beat, max_beat, min_layer, max_layer);
        }
        None => {
            // No anchor — set insert cursor at target (Unity line 247-248)
            selection.set_insert_cursor(target_beat, target_layer);
        }
    }
}

/// Handle undo (called from keyboard shortcut). Sends to content thread.
pub fn undo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    let _ = content_tx.try_send(crate::content_command::ContentCommand::Undo);
}

/// Handle redo (called from keyboard shortcut). Sends to content thread.
pub fn redo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    let _ = content_tx.try_send(crate::content_command::ContentCommand::Redo);
}

// ── Effect tab routing helpers ───────────────────────────────────

use manifold_core::project::Project;

/// Build an EffectTarget for the given tab.
fn resolve_effect_target(tab: InspectorTab, active_layer: Option<usize>) -> EffectTarget {
    match tab {
        InspectorTab::Master => EffectTarget::Master,
        InspectorTab::Layer => EffectTarget::Layer {
            layer_index: active_layer.unwrap_or(0),
        },
        InspectorTab::Clip => EffectTarget::Layer {
            layer_index: active_layer.unwrap_or(0),
        }, // Clip effects use layer for undo target
    }
}

/// Get a read-only reference to the effects list and the EffectTarget
/// for the given inspector tab. Returns (effects_slice, target).
fn resolve_effects_read<'a>(
    tab: InspectorTab,
    project: &'a Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) -> (Option<&'a [EffectInstance]>, EffectTarget) {
    match tab {
        InspectorTab::Master => (
            Some(&project.settings.master_effects),
            EffectTarget::Master,
        ),
        InspectorTab::Layer => {
            let target = EffectTarget::Layer { layer_index: active_layer.unwrap_or(0) };
            let effects = active_layer
                .and_then(|idx| project.timeline.layers.get(idx))
                .and_then(|l| l.effects.as_deref());
            (effects, target)
        }
        InspectorTab::Clip => {
            let target = EffectTarget::Layer { layer_index: active_layer.unwrap_or(0) };
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
fn resolve_effects_ref<'a>(
    tab: InspectorTab,
    project: &'a Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) -> Option<&'a [EffectInstance]> {
    resolve_effects_read(tab, project, active_layer, selection).0
}

/// Get a mutable reference to the effects list and EffectTarget.
fn resolve_effects_mut<'a>(
    tab: InspectorTab,
    project: &'a mut Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) -> (Option<&'a mut Vec<EffectInstance>>, EffectTarget) {
    match tab {
        InspectorTab::Master => {
            let effects = &mut project.settings.master_effects;
            (Some(effects), EffectTarget::Master)
        }
        InspectorTab::Layer => {
            let target = EffectTarget::Layer { layer_index: active_layer.unwrap_or(0) };
            let effects = active_layer
                .and_then(move |idx| project.timeline.layers.get_mut(idx))
                .map(|l| l.effects_mut());
            (effects, target)
        }
        InspectorTab::Clip => {
            let target = EffectTarget::Layer { layer_index: active_layer.unwrap_or(0) };
            let clip_id = selection.primary_selected_clip_id.clone();
            let effects = clip_id.and_then(|cid| {
                project.timeline.find_clip_by_id_mut(&cid)
                    .map(|c| &mut c.effects)
            });
            (effects, target)
        }
    }
}

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
const PAUSED_YELLOW: Color32 = Color32::new(209, 166, 38, 255);

/// Check auto-scroll during playback and return true if viewport scroll changed.
/// Must run BEFORE build() so the rebuild includes the new scroll position.
/// From Unity ViewportManager.UpdatePlayheadPosition (lines 327-357).
pub fn check_auto_scroll(ui: &mut UIRoot, content_state: &crate::content_state::ContentState, project: &Project) -> bool {
    if !content_state.is_playing {
        return false;
    }

    let playhead_beat = content_state.current_beat;
    let ppb = ui.viewport.pixels_per_beat();
    let viewport_w = ui.viewport.tracks_rect().width;
    if viewport_w <= 0.0 || ppb <= 0.0 {
        return false;
    }

    let scroll_x_beats = ui.viewport.scroll_x_beats();
    let playhead_px = (playhead_beat - scroll_x_beats) * ppb; // pixel offset from viewport left

    // Content expansion: if playhead approaches end of content, grow it.
    // From Unity ViewportManager.UpdatePlayheadPosition (lines 314-324).
    let content_beats = project.timeline.duration_beats();
    let content_w_px = content_beats * ppb;
    let playhead_abs_px = playhead_beat * ppb;
    if playhead_abs_px > content_w_px - 50.0 {
        // Content would need to grow — handled by sync_project_data setting clips
        // which automatically extends the viewport range. No explicit action needed here
        // since the viewport always shows scroll_x..scroll_x + viewport_w.
    }

    // Right edge margin: 50px. When playhead approaches right, scroll to 25% from left.
    let right_margin_px = 50.0;
    if playhead_px > viewport_w - right_margin_px {
        // Scroll so playhead is at 25% from left (75% ahead)
        let target_scroll_beat = playhead_beat - (viewport_w * 0.25) / ppb;
        ui.viewport.set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    // Left edge margin: 20px. When playhead goes behind left edge, scroll back.
    let left_margin_px = 20.0;
    if playhead_px < left_margin_px {
        let target_scroll_beat = playhead_beat - left_margin_px / ppb;
        ui.viewport.set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    false
}

/// Push engine state into UI panels (called once per frame, AFTER build).
/// Syncs all data-model state into tree nodes so the renderer shows current values.
pub fn push_state(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
    active_layer: Option<usize>,
    selection: &SelectionState,
    is_dirty: bool,
    project_path: Option<&std::path::Path>,
) {
    let tree = &mut ui.tree;

    // Transport state — three visual states matching Unity TransportPanel
    let state = if content_state.is_playing { manifold_core::types::PlaybackState::Playing } else { manifold_core::types::PlaybackState::Stopped };
    let (play_text, play_color) = match state {
        manifold_core::types::PlaybackState::Playing => ("PAUSE", PLAY_ACTIVE),
        manifold_core::types::PlaybackState::Paused => ("PLAY", PAUSED_YELLOW),
        manifold_core::types::PlaybackState::Stopped => ("PLAY", PLAY_GREEN),
    };
    ui.transport.set_play_state(tree, play_text, play_color);

    // Time display + BPM
    let beat = content_state.current_beat;
    let time = content_state.current_time;

    {
        let bpm = project.settings.bpm;

        // Unity FormatTime: "{minutes:D2}:{seconds:D2}.{tenths}"
        // Time first, then bar.beat.sixteenth — matches Unity exactly
        let mins = (time / 60.0).floor() as i32;
        let secs = (time % 60.0).floor() as i32;
        let tenths = ((time * 10.0) % 10.0).floor() as i32;
        let time_str = format!("{:02}:{:02}.{}", mins, secs, tenths);

        // Beat display uses time_signature_numerator (not hardcoded 4)
        let bpb = (project.settings.time_signature_numerator.max(1)) as f32;
        let bar = (beat / bpb).floor() as i32 + 1;
        let beat_in_bar = (beat % bpb).floor() as i32 + 1;
        let sixteenth = ((beat % 1.0) * 4.0).floor() as i32 + 1;
        let display = format!("{}  |  {}.{}.{}", time_str, bar, beat_in_bar, sixteenth);

        ui.header.set_time_display(tree, &display);
        ui.transport.set_bpm_text(tree, &format!("{:.1}", bpm));

        // Clock authority display — "SRC:INT"/"SRC:LNK"/"SRC:CLK"/"SRC:OSC"
        let auth = project.settings.clock_authority;
        let auth_color = match auth {
            manifold_core::types::ClockAuthority::Internal => color::BUTTON_INACTIVE_C32,
            manifold_core::types::ClockAuthority::Link => color::LINK_ORANGE,
            manifold_core::types::ClockAuthority::MidiClock => color::MIDI_PURPLE,
            manifold_core::types::ClockAuthority::Osc => color::ABLETON_LINK_BLUE,
        };
        ui.transport.set_clock_authority(tree, auth.transport_label(), auth_color);

        // Sync source status (default inactive until sync controllers exist)
        ui.transport.set_link_state(tree, false, color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);
        ui.transport.set_clk_state(tree, false, "Select...", color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);
        ui.transport.set_sync_state(tree, false, color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);

        // Record state — disabled when OSC is clock authority (Unity invariant)
        let rec_allowed = auth != manifold_core::types::ClockAuthority::Osc;
        ui.transport.set_record_state(tree, content_state.is_recording && rec_allowed, rec_allowed);

        // BPM reset: enabled when recorded tempo lane exists or recorded BPM differs
        let can_reset = !project.recording_provenance.recorded_tempo_lane.is_empty()
            || (project.recording_provenance.has_recorded_project_bpm
                && (bpm - project.recording_provenance.recorded_project_bpm).abs() >= 0.0001);
        ui.transport.set_bpm_reset_active(tree, can_reset);

        // BPM clear: enabled when tempo map has >1 point
        let can_clear = project.tempo_map.points.len() > 1;
        ui.transport.set_bpm_clear_active(tree, can_clear);

        // Save button — "SAVE" clean, "SAVE *" dirty with warm brown tint
        ui.transport.set_save_text(tree, if is_dirty { "SAVE *" } else { "SAVE" });

        // Export state
        let has_export_range = project.timeline.export_in_beat < project.timeline.export_out_beat;
        if has_export_range {
            let in_b = project.timeline.export_in_beat;
            let out_b = project.timeline.export_out_beat;
            let export_label = if out_b > 0.0 {
                format!("IN: {:.1} OUT: {:.1}", in_b, out_b)
            } else {
                format!("IN: {:.1}", in_b)
            };
            ui.transport.set_export_label(tree, &export_label);
        } else {
            ui.transport.set_export_label(tree, "");
        }
        ui.transport.set_export_active(tree, false); // No active export in Rust port yet
        ui.transport.set_hdr_active(tree, project.settings.export_hdr);

        // Export range markers on viewport
        ui.viewport.set_export_range(project.timeline.export_in_beat, project.timeline.export_out_beat);

        // Header — project name + dirty bullet
        let project_name = project_path
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled");
        let header_name = if is_dirty {
            format!("{} \u{2022}", project_name)
        } else {
            project_name.to_string()
        };
        ui.header.set_project_name(tree, &header_name);
        let ppb = ui.viewport.pixels_per_beat();
        ui.header.set_zoom_label(tree, &format!("{:.0} px/beat", ppb));

        // Footer — quantize mode, resolution, FPS
        ui.footer.set_quantize_text(tree, project.settings.quantize_mode.display_name());
        // Show preset label if dimensions match, otherwise show "WxH" (Unity: UpdateFooterResolutionText)
        let (preset_w, preset_h) = project.settings.resolution_preset.dimensions();
        let res_label = if preset_w == project.settings.output_width && preset_h == project.settings.output_height {
            project.settings.resolution_preset.display_name().to_string()
        } else {
            format!("{}x{}", project.settings.output_width, project.settings.output_height)
        };
        ui.footer.set_resolution_text(tree, &res_label);
        ui.footer.set_fps_text(tree, &format!("{:.0} FPS", project.settings.frame_rate));
    }

    // Footer stats
    {
        let layers = project.timeline.layers.len();
        let clips: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let info = format!("Layers: {} | Clips: {}", layers, clips);
        ui.footer.set_selection_info(tree, &info);
    }

    // Playhead + playing state
    let playhead_beat = content_state.current_beat;
    ui.viewport.set_playhead(playhead_beat);
    ui.viewport.set_playing(content_state.is_playing);

    // Selection → viewport
    ui.viewport.set_selected_clip_ids(
        selection.selected_clip_ids.iter().cloned().collect()
    );
    if let Some(beat) = selection.insert_cursor_beat {
        ui.viewport.set_insert_cursor(beat);
    }

    // Region → viewport (sync from UIState so clearing via set_insert_cursor propagates)
    if selection.has_region() {
        let r = selection.get_region();
        ui.viewport.set_selection_region(Some(
            manifold_ui::panels::viewport::SelectionRegion {
                start_beat: r.start_beat,
                end_beat: r.end_beat,
                start_layer: r.start_layer_index.max(0) as usize,
                end_layer: r.end_layer_index.max(0) as usize,
            }
        ));
    } else {
        ui.viewport.set_selection_region(None);
    }

    // Layer highlighting via UIState.is_layer_active (unified check across 4 paths):
    // explicit layer selection, clip selection, insert cursor, region.
    {
        let active_flags: Vec<bool> = project.timeline.layers.iter().enumerate()
            .map(|(i, l)| selection.is_layer_active(i, &l.layer_id))
            .collect();
        ui.layer_headers.set_active_layers(&active_flags);
    }
    // Also set single active_layer for backward compat (inspector routing)
    ui.layer_headers.set_active_layer(active_layer);
    {
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            ui.layer_headers.set_mute_state(tree, i, layer.is_muted);
            ui.layer_headers.set_solo_state(tree, i, layer.is_solo);
            ui.layer_headers.set_blend_mode_text(tree, i, layer.default_blend_mode.display_name());

            // MIDI note/channel labels
            let note_text = if layer.midi_note >= 0 {
                format!("{}", layer.midi_note)
            } else {
                "—".into()
            };
            ui.layer_headers.set_midi_note_text(tree, i, &note_text);

            let ch_text = if layer.midi_channel >= 0 {
                format!("Ch {}", layer.midi_channel + 1)
            } else {
                "Any".into()
            };
            ui.layer_headers.set_midi_channel_text(tree, i, &ch_text);

            // Layer info text (clip count)
            let clip_count = layer.clips.len();
            let info = if clip_count == 1 { "1 clip".into() } else { format!("{} clips", clip_count) };
            ui.layer_headers.set_info_text(tree, i, &info);
        }
    }

    // Sync active layer opacity to inspector chrome
    if let Some(idx) = active_layer {
        {
            if let Some(layer) = project.timeline.layers.get(idx) {
                ui.inspector.layer_chrome_mut().sync_opacity(tree, layer.opacity);
                ui.inspector.layer_chrome_mut().sync_name(tree, &layer.name);
            }
            // Master opacity
            ui.inspector.master_chrome_mut().sync_opacity(tree, project.settings.master_opacity);
        }
    }

    // Sync clip chrome from primary selected clip
    if let Some(clip_id) = &selection.primary_selected_clip_id {
        {
            // Linear search (no mut needed for read-only)
            let clip = project.timeline.layers.iter()
                .flat_map(|l| l.clips.iter())
                .find(|c| c.id == *clip_id);
            if let Some(clip) = clip {
                let is_video = !clip.video_clip_id.is_empty();
                let is_gen = clip.generator_type != GeneratorType::None;
                let chrome = ui.inspector.clip_chrome_mut();
                let mode_changed = chrome.set_mode(true, is_video, is_gen, clip.is_looping);
                if is_video {
                    let name = clip.video_clip_id.clone();
                    chrome.sync_name(tree, &name);
                    chrome.sync_source_name(tree, &clip.video_clip_id);
                    chrome.sync_slip(tree, clip.in_point);
                    chrome.sync_loop_enabled(tree, clip.is_looping);
                    chrome.sync_loop_duration(tree, clip.loop_duration_beats);
                    if clip.recorded_bpm > 0.0 {
                        chrome.sync_bpm(tree, &format!("{:.1}", clip.recorded_bpm));
                    } else {
                        chrome.sync_bpm(tree, "Auto");
                    }
                    // Slip range = source duration - clip duration in seconds
                    let spb = 60.0 / Some(&*project).map_or(120.0, |p| p.settings.bpm);
                    let clip_dur_s = clip.duration_beats * spb;
                    chrome.set_slip_range(clip_dur_s.max(1.0));
                    chrome.set_loop_range(clip.duration_beats.max(1.0));
                } else if is_gen {
                    chrome.sync_name(tree, &format!("{}", clip.generator_type.display_name()));
                    chrome.sync_gen_type(tree, clip.generator_type.display_name());
                }
                if mode_changed {
                    // Rebuild needed — mark as structural
                }
            }
        }
    } else {
        // No clip selected — hide clip chrome content
        let chrome = ui.inspector.clip_chrome_mut();
        chrome.set_mode(false, false, false, false);
    }

    // Sync effect card values (master, layer, clip)
    {
        // Master effects
        for (i, effect) in project.settings.master_effects.iter().enumerate() {
            if let Some(card) = ui.inspector.master_effect_mut(i) {
                card.sync_effect_name(tree, effect.effect_type.display_name());
                card.sync_enabled(tree, effect.enabled);
                card.sync_values(tree, &effect.param_values);
            }
        }

        // Layer effects
        if let Some(idx) = active_layer {
            if let Some(layer) = project.timeline.layers.get(idx) {
                if let Some(effects) = &layer.effects {
                    for (i, effect) in effects.iter().enumerate() {
                        if let Some(card) = ui.inspector.layer_effect_mut(i) {
                            card.sync_effect_name(tree, effect.effect_type.display_name());
                            card.sync_enabled(tree, effect.enabled);
                            card.sync_values(tree, &effect.param_values);
                        }
                    }
                }
            }
        }

        // Clip effects
        if let Some(clip_id) = &selection.primary_selected_clip_id {
            let clip = project.timeline.layers.iter()
                .flat_map(|l| l.clips.iter())
                .find(|c| c.id == *clip_id);
            if let Some(clip) = clip {
                for (i, effect) in clip.effects.iter().enumerate() {
                    if let Some(card) = ui.inspector.clip_effect_mut(i) {
                        card.sync_effect_name(tree, effect.effect_type.display_name());
                        card.sync_enabled(tree, effect.enabled);
                        card.sync_values(tree, &effect.param_values);
                    }
                }
            }
        }

        // Generator params (stored on layer, not clip)
        if let Some(idx) = active_layer {
            if let Some(layer) = project.timeline.layers.get(idx) {
                if let Some(gp_state) = &layer.gen_params {
                    if let Some(gp) = ui.inspector.gen_params_mut() {
                        gp.sync_gen_type_name(tree, gp_state.generator_type.display_name());
                        gp.sync_values(tree, &gp_state.param_values);
                    }
                }
            }
        }
    }

}

/// Sync structural project data (layers, tracks) into UI panels.
/// Call once at init and whenever the project structure changes.
/// Triggers a full UI rebuild afterward.
pub fn sync_project_data(ui: &mut UIRoot, project: &Project, active_layer: Option<usize>) {
    {
        // Rebuild CoordinateMapper Y-layout FIRST so layer headers and viewport share
        // the same Y offsets. Unity: LayerHeaderPanel reads from CoordinateMapper.
        ui.viewport.rebuild_mapper_layout(&project.timeline.layers);

        // Layer data → LayerHeaderPanel (Y from mapper — matches viewport exactly)
        let layers: Vec<LayerInfo> = project.timeline.layers.iter().enumerate().map(|(i, layer)| {
            let y = ui.viewport.mapper().get_layer_y_offset(i);
            let track_h = ui.viewport.mapper().get_layer_height(i);
            LayerInfo {
                name: layer.name.clone(),
                layer_id: layer.layer_id.clone(),
                is_collapsed: layer.is_collapsed,
                is_group: layer.is_group(),
                is_generator: layer.layer_type == LayerType::Generator,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                parent_layer_id: layer.parent_layer_id.clone(),
                blend_mode: format!("{:?}", layer.default_blend_mode),
                generator_type: layer.gen_params.as_ref()
                    .map(|g| format!("{:?}", g.generator_type)),
                clip_count: layer.clips.len(),
                video_folder_path: layer.video_folder_path.clone(),
                source_clip_count: 0,
                midi_note: layer.midi_note,
                midi_channel: layer.midi_channel,
                y_offset: y,
                height: track_h,
                is_selected: active_layer == Some(i),
            }
        }).collect();
        ui.layer_headers.set_active_layer(active_layer);
        ui.layer_headers.set_layers(layers);

        // Track data → TimelineViewportPanel
        // From Unity ViewportManager.BuildTrack (lines 548-663):
        // - is_muted includes parent group mute (children of muted groups are dimmed)
        // - is_group set correctly for group layers
        // - accent_color set for child layers
        let tracks: Vec<TrackInfo> = project.timeline.layers.iter().enumerate().map(|(_i, layer)| {
            // Check if muted individually or by parent group
            let parent_muted = layer.parent_layer_id.as_ref().map_or(false, |pid| {
                project.timeline.layers.iter().any(|l| l.layer_id == *pid && l.is_muted)
            });
            let is_muted = layer.is_muted || parent_muted;

            // Variable track heights matching Unity CoordinateMapper.RebuildYLayout
            let height = if layer.parent_layer_id.is_some() {
                // Child of group: check parent collapsed
                let parent_collapsed = layer.parent_layer_id.as_ref().map_or(false, |pid| {
                    project.timeline.layers.iter().any(|l| l.layer_id == *pid && l.is_collapsed)
                });
                if parent_collapsed { 0.0 } else { color::TRACK_HEIGHT }
            } else if layer.is_group() && layer.is_collapsed {
                color::COLLAPSED_GROUP_TRACK_HEIGHT
            } else if !layer.is_group() && layer.is_collapsed {
                if layer.layer_type == manifold_core::types::LayerType::Generator {
                    color::COLLAPSED_GEN_TRACK_HEIGHT
                } else {
                    color::COLLAPSED_TRACK_HEIGHT
                }
            } else {
                color::TRACK_HEIGHT
            };

            // Accent color for child layers (group visual)
            let accent_color = if layer.parent_layer_id.is_some() {
                Some(color::DEFAULT_GROUP_ACCENT)
            } else {
                None
            };

            // Child layer indices for collapsed group preview
            let child_layer_indices = if layer.is_group() {
                let layer_id = &layer.layer_id;
                project.timeline.layers.iter().enumerate()
                    .filter(|(_, l)| l.parent_layer_id.as_ref() == Some(layer_id))
                    .map(|(j, _)| j)
                    .collect()
            } else {
                Vec::new()
            };

            TrackInfo {
                height,
                is_muted,
                is_group: layer.is_group(),
                is_collapsed: layer.is_collapsed,
                accent_color,
                child_layer_indices,
            }
        }).collect();
        ui.viewport.set_tracks(tracks);

        // (CoordinateMapper Y-layout already rebuilt above, before layer headers)

        // Clip data → TimelineViewportPanel
        let mut viewport_clips = Vec::new();
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                let is_gen = layer.layer_type == LayerType::Generator;
                let name = if is_gen {
                    layer.gen_params.as_ref()
                        .map(|gp| gp.generator_type.display_name().to_string())
                        .unwrap_or_else(|| "Gen".to_string())
                } else if !clip.video_clip_id.is_empty() {
                    clip.video_clip_id.clone()
                } else {
                    "Clip".to_string()
                };
                use manifold_ui::panels::viewport::ViewportClip;
                viewport_clips.push(ViewportClip {
                    clip_id: clip.id.clone(),
                    layer_index: i,
                    start_beat: clip.start_beat,
                    duration_beats: clip.duration_beats,
                    name,
                    color: if is_gen {
                        manifold_ui::color::CLIP_GEN_NORMAL
                    } else {
                        manifold_ui::color::CLIP_NORMAL
                    },
                    is_muted: clip.is_muted,
                    is_locked: false,
                    is_generator: is_gen,
                });
            }
        }
        ui.viewport.set_clips(viewport_clips);

        // Beats per bar
        ui.viewport.set_beats_per_bar(project.settings.time_signature_numerator as u32);
    }
}

/// Lightweight per-frame clip position sync.
/// Refreshes viewport.clips_by_layer from the live project model so that
/// drag mutations (clip move, trim) are visible in the bitmap renderer.
/// Does NOT touch tracks, bitmap renderers, or layer headers — only clip data.
/// The bitmap fingerprint will detect if positions actually changed and skip
/// repaint when nothing moved (cheap no-op outside of drag).
pub fn sync_clip_positions(ui: &mut UIRoot, project: &Project) {
    use manifold_ui::panels::viewport::ViewportClip;
    let mut viewport_clips = Vec::new();
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        let is_gen = layer.layer_type == LayerType::Generator;
        for clip in &layer.clips {
            let name = if is_gen {
                layer.gen_params.as_ref()
                    .map(|gp| gp.generator_type.display_name().to_string())
                    .unwrap_or_else(|| "Gen".to_string())
            } else if !clip.video_clip_id.is_empty() {
                clip.video_clip_id.clone()
            } else {
                "Clip".to_string()
            };
            viewport_clips.push(ViewportClip {
                clip_id: clip.id.clone(),
                layer_index: i,
                start_beat: clip.start_beat,
                duration_beats: clip.duration_beats,
                name,
                color: if is_gen {
                    manifold_ui::color::CLIP_GEN_NORMAL
                } else {
                    manifold_ui::color::CLIP_NORMAL
                },
                is_muted: clip.is_muted,
                is_locked: false,
                is_generator: is_gen,
            });
        }
    }
    ui.viewport.set_clips(viewport_clips);
}

/// Sync inspector content for the active selection.
/// Called when the active layer changes or after structural mutations.
pub fn sync_inspector_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) {

    // Master effects → inspector (master has no envelopes)
    let master_configs = effects_to_configs(&project.settings.master_effects, &[]);
    ui.inspector.configure_master_effects(&master_configs);

    // Active layer effects + gen params → inspector
    if let Some(idx) = active_layer {
        if let Some(layer) = project.timeline.layers.get(idx) {
            // Layer effects — envelopes live on the layer
            let envs = layer.envelopes.as_deref().unwrap_or(&[]);
            let layer_effects = layer.effects.as_ref()
                .map(|e| effects_to_configs(e, envs))
                .unwrap_or_default();
            ui.inspector.configure_layer_effects(&layer_effects);

            // Generator params
            let gen_config = layer.gen_params.as_ref()
                .filter(|gp| gp.generator_type != GeneratorType::None)
                .map(|gp| gen_params_to_config(gp));
            ui.inspector.configure_gen_params(gen_config.as_ref());
        } else {
            ui.inspector.configure_layer_effects(&[]);
            ui.inspector.configure_gen_params(None);
        }
    } else {
        ui.inspector.configure_layer_effects(&[]);
        ui.inspector.configure_gen_params(None);
    }

    // Clip effects → inspector
    if let Some(clip_id) = &selection.primary_selected_clip_id {
        let clip = project.timeline.layers.iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *clip_id);
        if let Some(clip) = clip {
            let clip_envs = clip.envelopes.as_deref().unwrap_or(&[]);
            let clip_configs = effects_to_configs(&clip.effects, clip_envs);
            ui.inspector.configure_clip_effects(&clip_configs);
        } else {
            ui.inspector.configure_clip_effects(&[]);
        }
    } else {
        ui.inspector.configure_clip_effects(&[]);
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Convert a slice of `EffectInstance` into `EffectCardConfig` for the UI.
/// Build EffectCardConfig from EffectInstance + envelopes.
/// Unity: EffectCardState.SyncFromDataModel — populates all data-derived visual state.
fn effects_to_configs(effects: &[EffectInstance], envelopes: &[ParamEnvelope]) -> Vec<EffectCardConfig> {
    effects.iter().enumerate().map(|(i, fx)| {
        let reg_def = manifold_core::effect_definition_registry::get(fx.effect_type);
        let n = reg_def.param_count;
        let params: Vec<EffectParamInfo> = reg_def.param_defs.iter().map(|pd| {
            EffectParamInfo {
                name: pd.name.clone(),
                min: pd.min,
                max: pd.max,
                default: pd.default_value,
                whole_numbers: pd.whole_numbers,
                value_labels: pd.value_labels.clone(),
            }
        }).collect();

        // Per-param driver state (Unity: SyncFromDataModel driver loop)
        let mut has_drv = false;
        let mut driver_active = vec![false; n];
        let mut trim_min = vec![0.0f32; n];
        let mut trim_max = vec![1.0f32; n];
        let mut driver_beat_div_idx = vec![-1i32; n];
        let mut driver_waveform_idx = vec![-1i32; n];
        let mut driver_reversed = vec![false; n];
        let mut driver_dotted = vec![false; n];
        let mut driver_triplet = vec![false; n];
        if let Some(ref drivers) = fx.drivers {
            for d in drivers {
                let pi = d.param_index as usize;
                if pi < n && d.enabled {
                    has_drv = true;
                    driver_active[pi] = true;
                    trim_min[pi] = d.trim_min;
                    trim_max[pi] = d.trim_max;
                    // Driver visual state for button highlighting
                    driver_beat_div_idx[pi] = beat_div_to_button_index(d.beat_division.base_division());
                    driver_waveform_idx[pi] = d.waveform as i32;
                    driver_reversed[pi] = d.reversed;
                    driver_dotted[pi] = d.beat_division.is_dotted();
                    driver_triplet[pi] = d.beat_division.is_triplet();
                }
            }
        }

        // Per-param envelope state (Unity: SyncFromDataModel envelope loop)
        let mut has_env = false;
        let mut envelope_active = vec![false; n];
        let mut target_norm = vec![1.0f32; n];
        let mut env_attack = vec![0.0f32; n];
        let mut env_decay = vec![0.0f32; n];
        let mut env_sustain = vec![0.0f32; n];
        let mut env_release = vec![0.0f32; n];
        for env in envelopes {
            if env.target_effect_type == fx.effect_type && env.enabled {
                let pi = env.param_index as usize;
                if pi < n {
                    has_env = true;
                    envelope_active[pi] = true;
                    target_norm[pi] = env.target_normalized;
                    env_attack[pi] = env.attack_beats;
                    env_decay[pi] = env.decay_beats;
                    env_sustain[pi] = env.sustain_level;
                    env_release[pi] = env.release_beats;
                }
            }
        }

        EffectCardConfig {
            effect_index: i,
            name: fx.effect_type.display_name().to_string(),
            enabled: fx.enabled,
            collapsed: fx.collapsed,
            supports_envelopes: true,
            params,
            has_drv,
            has_env,
            driver_active,
            envelope_active,
            trim_min,
            trim_max,
            target_norm,
            env_attack,
            env_decay,
            env_sustain,
            env_release,
            driver_beat_div_idx,
            driver_waveform_idx,
            driver_reversed,
            driver_dotted,
            driver_triplet,
        }
    }).collect()
}

/// Map a base BeatDivision to its button index (0-10).
/// Reverse of BeatDivision::from_button_index.
fn beat_div_to_button_index(div: BeatDivision) -> i32 {
    match div {
        BeatDivision::ThirtySecond => -1, // No button for 1/32
        BeatDivision::Sixteenth => 0,
        BeatDivision::Eighth | BeatDivision::EighthDotted | BeatDivision::EighthTriplet => 1,
        BeatDivision::Quarter | BeatDivision::QuarterDotted | BeatDivision::QuarterTriplet => 2,
        BeatDivision::Half | BeatDivision::HalfDotted | BeatDivision::HalfTriplet => 3,
        BeatDivision::Whole | BeatDivision::WholeDotted | BeatDivision::WholeTriplet => 4,
        BeatDivision::TwoWhole | BeatDivision::TwoWholeDotted => 5,
        BeatDivision::FourWhole => 6,
        BeatDivision::EightWhole => 7,
        BeatDivision::SixteenWhole => 8,
        BeatDivision::ThirtyTwoWhole => 9,
    }
}

/// Convert a `GeneratorParamState` into `GenParamConfig` for the UI.
fn gen_params_to_config(gp: &manifold_core::generator::GeneratorParamState) -> GenParamConfig {
    let reg_def = manifold_core::generator_definition_registry::get(gp.generator_type);
    let n = reg_def.param_defs.len();
    let params: Vec<GenParamInfo> = reg_def.param_defs.iter().map(|pd| {
        GenParamInfo {
            name: pd.name.clone(),
            min: pd.min,
            max: pd.max,
            default: pd.default_value,
            whole_numbers: pd.whole_numbers,
            is_toggle: pd.is_toggle,
            value_labels: pd.value_labels.clone(),
        }
    }).collect();

    // Per-param driver state
    let mut driver_active = vec![false; n];
    let mut trim_min = vec![0.0f32; n];
    let mut trim_max = vec![1.0f32; n];
    let mut driver_beat_div_idx = vec![-1i32; n];
    let mut driver_waveform_idx = vec![-1i32; n];
    let mut driver_reversed = vec![false; n];
    let mut driver_dotted = vec![false; n];
    let mut driver_triplet = vec![false; n];
    if let Some(ref drivers) = gp.drivers {
        for d in drivers {
            if d.enabled {
                let pi = d.param_index as usize;
                if pi < n {
                    driver_active[pi] = true;
                    trim_min[pi] = d.trim_min;
                    trim_max[pi] = d.trim_max;
                    driver_beat_div_idx[pi] = beat_div_to_button_index(d.beat_division.base_division());
                    driver_waveform_idx[pi] = d.waveform as i32;
                    driver_reversed[pi] = d.reversed;
                    driver_dotted[pi] = d.beat_division.is_dotted();
                    driver_triplet[pi] = d.beat_division.is_triplet();
                }
            }
        }
    }

    // Per-param envelope state
    let mut envelope_active = vec![false; n];
    let mut target_norm = vec![1.0f32; n];
    let mut env_attack = vec![0.0f32; n];
    let mut env_decay = vec![0.0f32; n];
    let mut env_sustain = vec![0.0f32; n];
    let mut env_release = vec![0.0f32; n];
    if let Some(ref envelopes) = gp.envelopes {
        for env in envelopes {
            if env.enabled {
                let pi = env.param_index as usize;
                if pi < n {
                    envelope_active[pi] = true;
                    target_norm[pi] = env.target_normalized;
                    env_attack[pi] = env.attack_beats;
                    env_decay[pi] = env.decay_beats;
                    env_sustain[pi] = env.sustain_level;
                    env_release[pi] = env.release_beats;
                }
            }
        }
    }

    GenParamConfig {
        gen_type_name: gp.generator_type.display_name().to_string(),
        params,
        driver_active,
        envelope_active,
        trim_min,
        trim_max,
        target_norm,
        env_attack,
        env_decay,
        env_sustain,
        env_release,
        driver_beat_div_idx,
        driver_waveform_idx,
        driver_reversed,
        driver_dotted,
        driver_triplet,
    }
}
