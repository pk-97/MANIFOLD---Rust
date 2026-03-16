//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

use manifold_core::types::{
    BlendMode, GeneratorType, LayerType, PlaybackState,
    BeatDivision, DriverWaveform,
};
use manifold_core::effects::{EffectInstance, ParameterDriver, ParamEnvelope};
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
    ChangeQuantizeModeCommand, ChangeLayerBlendModeCommand,
};
use manifold_editing::commands::effects::{
    ToggleEffectCommand, ChangeEffectParamCommand, RemoveEffectCommand,
};
use manifold_editing::commands::effect_target::{EffectTarget, DriverTarget};
use manifold_editing::commands::drivers::{
    AddDriverCommand, ToggleDriverEnabledCommand,
    ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand,
    ToggleDriverReversedCommand,
};
use manifold_editing::commands::clip::{
    MoveClipCommand, TrimClipCommand, SlipClipCommand, ChangeClipLoopCommand,
};
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::service::EditingService;
use manifold_playback::engine::PlaybackEngine;
use manifold_ui::{PanelAction, InspectorTab, DriverConfigAction};
use manifold_ui::node::Color32;
use manifold_ui::color;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::{TrackInfo, HitRegion};
use manifold_ui::panels::effect_card::{EffectCardConfig, EffectParamInfo};
use manifold_ui::panels::gen_param::{GenParamConfig, GenParamInfo};

use crate::app::{SelectionState, ClipDragState, ClipDragMode, ClipDragSnapshot};
use crate::ui_root::UIRoot;

/// Result of dispatching a panel action.
pub struct DispatchResult {
    /// True if the action was handled.
    pub handled: bool,
    /// True if the action changed project structure (needs sync_project_data).
    pub structural_change: bool,
}

impl DispatchResult {
    fn handled() -> Self { Self { handled: true, structural_change: false } }
    fn structural() -> Self { Self { handled: true, structural_change: true } }
    fn unhandled() -> Self { Self { handled: false, structural_change: false } }
}

/// Dispatch a panel action to the engine/editing service.
pub fn dispatch(
    action: &PanelAction,
    engine: &mut PlaybackEngine,
    editing: &mut EditingService,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    clip_drag: &mut ClipDragState,
    active_layer: &mut Option<usize>,
    drag_snapshot: &mut Option<f32>,
) -> DispatchResult {
    match action {
        // ── Transport ──────────────────────────────────────────────
        PanelAction::PlayPause => {
            if engine.is_playing() {
                engine.set_state(PlaybackState::Paused);
            } else {
                engine.set_state(PlaybackState::Playing);
            }
            DispatchResult::handled()
        }
        PanelAction::Stop => {
            engine.set_state(PlaybackState::Stopped);
            engine.seek_to(0.0);
            DispatchResult::handled()
        }
        PanelAction::Record => {
            log::info!("Record toggled (MIDI recording not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ResetBpm => {
            log::info!("Reset BPM (tempo lane restore not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ClearBpm => {
            if let Some(project) = engine.project_mut() {
                let old_points = project.tempo_map.clone_points();
                let bpm = project.settings.bpm;
                let cmd = manifold_editing::commands::settings::ClearTempoMapCommand::new(old_points, bpm);
                editing.execute(Box::new(cmd), project);
            }
            DispatchResult::handled()
        }
        PanelAction::BpmFieldClicked => {
            log::debug!("BPM field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::Seek(beat) => {
            if let Some(p) = engine.project() {
                let time = *beat * (60.0 / p.settings.bpm);
                engine.seek_to(time);
            }
            DispatchResult::handled()
        }
        PanelAction::SetInsertCursor(beat) => {
            // Legacy path — when no layer context available
            selection.insert_cursor_beat = Some(*beat);
            DispatchResult::handled()
        }

        // ── Viewport clip interaction ─────────────────────────────
        PanelAction::ClipClicked(clip_id, modifiers) => {
            if modifiers.command || modifiers.ctrl {
                selection.toggle(clip_id.clone());
            } else {
                selection.select_single(clip_id.clone());
            }
            // Find the clip's layer to set active layer
            if let Some(project) = engine.project() {
                for (i, layer) in project.timeline.layers.iter().enumerate() {
                    if layer.clips.iter().any(|c| c.id == *clip_id) {
                        *active_layer = Some(i);
                        break;
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDoubleClicked(_clip_id) => {
            // Future: open clip properties or enter clip editing mode
            DispatchResult::handled()
        }
        PanelAction::TrackClicked(beat, layer, _modifiers) => {
            selection.clear();
            selection.set_insert_cursor(*beat, *layer);
            *active_layer = Some(*layer);
            DispatchResult::handled()
        }
        PanelAction::TrackDoubleClicked(beat, layer) => {
            let snapped = ui.viewport.snap_to_grid(*beat);
            if let Some(project) = engine.project_mut() {
                let cmd = EditingService::create_clip_at_position(project, snapped, *layer, 4.0);
                editing.execute(cmd, project);
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
                            editing.execute(cmd, project);
                        }
                        // Select the newly created clip
                        selection.select_single(new_clip_clone.id);
                    }
                }
            }
            *active_layer = Some(*layer);
            DispatchResult::structural()
        }
        PanelAction::ClipDragStarted(clip_id, region, anchor_beat) => {
            // Select the clip if not already selected
            if !selection.selected_clip_ids.contains(clip_id) {
                selection.select_single(clip_id.clone());
            }

            match region {
                HitRegion::Body => {
                    // Snapshot all selected clips for move
                    let mut snapshots = Vec::new();
                    let sel_ids: Vec<String> = selection.selected_clip_ids.iter().cloned().collect();
                    if let Some(project) = engine.project_mut() {
                        for sel_id in &sel_ids {
                            if let Some(clip) = project.timeline.find_clip_by_id(sel_id) {
                                snapshots.push(ClipDragSnapshot {
                                    clip_id: sel_id.clone(),
                                    original_start_beat: clip.start_beat,
                                    original_layer_index: clip.layer_index,
                                });
                            }
                        }
                    }
                    clip_drag.mode = ClipDragMode::Move;
                    clip_drag.anchor_clip_id = clip_id.clone();
                    clip_drag.anchor_beat = *anchor_beat;
                    clip_drag.snapshots = snapshots;
                }
                HitRegion::TrimLeft | HitRegion::TrimRight => {
                    if let Some(project) = engine.project_mut() {
                        if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                            clip_drag.trim_old_start = clip.start_beat;
                            clip_drag.trim_old_duration = clip.duration_beats;
                            clip_drag.trim_old_in_point = clip.in_point;
                        }
                    }
                    clip_drag.mode = if *region == HitRegion::TrimLeft {
                        ClipDragMode::TrimLeft
                    } else {
                        ClipDragMode::TrimRight
                    };
                    clip_drag.anchor_clip_id = clip_id.clone();
                    clip_drag.anchor_beat = *anchor_beat;
                    clip_drag.snapshots.clear();
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipDragMoved(current_beat, _target_layer) => {
            // Collect IDs being dragged for magnetic snap ignore list
            let drag_ids: Vec<String> = clip_drag.snapshots.iter().map(|s| s.clip_id.clone()).collect();
            let anchor_layer = engine.project()
                .and_then(|p| {
                    p.timeline.layers.iter()
                        .enumerate()
                        .find_map(|(li, layer)| {
                            layer.clips.iter()
                                .find(|c| c.id == clip_drag.anchor_clip_id)
                                .map(|_| li)
                        })
                })
                .unwrap_or(0);

            match clip_drag.mode {
                ClipDragMode::Move => {
                    let snapped = ui.viewport.magnetic_snap(*current_beat, anchor_layer, &drag_ids);
                    let anchor_snapped = ui.viewport.magnetic_snap(clip_drag.anchor_beat, anchor_layer, &drag_ids);
                    let delta = snapped - anchor_snapped;
                    if let Some(project) = engine.project_mut() {
                        for snap in &clip_drag.snapshots {
                            let new_start = (snap.original_start_beat + delta).max(0.0);
                            if let Some(clip) = project.timeline.find_clip_by_id_mut(&snap.clip_id) {
                                clip.start_beat = new_start;
                            }
                        }
                    }
                }
                ClipDragMode::TrimLeft => {
                    let snapped = ui.viewport.magnetic_snap(*current_beat, anchor_layer, &drag_ids);
                    if let Some(project) = engine.project_mut() {
                        let old_end = clip_drag.trim_old_start + clip_drag.trim_old_duration;
                        let new_start = snapped.max(0.0).min(old_end - 0.25);
                        let new_duration = old_end - new_start;
                        // Adjust in_point for video clips
                        let spb = 60.0 / project.settings.bpm;
                        let in_point_delta = (new_start - clip_drag.trim_old_start) * spb;
                        let new_in_point = (clip_drag.trim_old_in_point + in_point_delta).max(0.0);

                        if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_drag.anchor_clip_id) {
                            clip.start_beat = new_start;
                            clip.duration_beats = new_duration;
                            clip.in_point = new_in_point;
                        }
                    }
                }
                ClipDragMode::TrimRight => {
                    let snapped = ui.viewport.magnetic_snap(*current_beat, anchor_layer, &drag_ids);
                    if let Some(project) = engine.project_mut() {
                        let new_duration = (snapped - clip_drag.trim_old_start).max(0.25);
                        if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_drag.anchor_clip_id) {
                            clip.duration_beats = new_duration;
                        }
                    }
                }
                _ => {}
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDragEnded => {
            match clip_drag.mode {
                ClipDragMode::Move => {
                    // Collect current positions first, then create commands
                    let mut moves: Vec<(String, f32, f32, i32, i32)> = Vec::new();
                    let drag_ids: std::collections::HashSet<String> =
                        clip_drag.snapshots.iter().map(|s| s.clip_id.clone()).collect();
                    if let Some(project) = engine.project_mut() {
                        for snap in &clip_drag.snapshots {
                            if let Some(clip) = project.timeline.find_clip_by_id(&snap.clip_id) {
                                let new_start = clip.start_beat;
                                let new_layer = clip.layer_index;
                                if (new_start - snap.original_start_beat).abs() > f32::EPSILON
                                    || new_layer != snap.original_layer_index
                                {
                                    moves.push((
                                        snap.clip_id.clone(),
                                        snap.original_start_beat,
                                        new_start,
                                        snap.original_layer_index,
                                        new_layer,
                                    ));
                                }
                            }
                        }
                        // Enforce non-overlap for each moved clip
                        let spb = 60.0 / project.settings.bpm;
                        for snap in &clip_drag.snapshots {
                            if let Some(clip) = project.timeline.find_clip_by_id(&snap.clip_id).cloned() {
                                let layer_idx = clip.layer_index as usize;
                                let overlap_cmds = EditingService::enforce_non_overlap(
                                    project, &clip, layer_idx, &drag_ids, spb,
                                );
                                for cmd in overlap_cmds {
                                    editing.execute(cmd, project);
                                }
                            }
                        }
                    }
                    for (id, old_start, new_start, old_layer, new_layer) in moves {
                        let cmd = MoveClipCommand::new(id, old_start, new_start, old_layer, new_layer);
                        editing.record(Box::new(cmd));
                    }
                }
                ClipDragMode::TrimLeft | ClipDragMode::TrimRight => {
                    if let Some(project) = engine.project_mut() {
                        if let Some(clip) = project.timeline.find_clip_by_id(&clip_drag.anchor_clip_id) {
                            let changed = (clip.start_beat - clip_drag.trim_old_start).abs() > f32::EPSILON
                                || (clip.duration_beats - clip_drag.trim_old_duration).abs() > f32::EPSILON;
                            if changed {
                                let cmd = TrimClipCommand::new(
                                    clip_drag.anchor_clip_id.clone(),
                                    clip_drag.trim_old_start,
                                    clip.start_beat,
                                    clip_drag.trim_old_duration,
                                    clip.duration_beats,
                                    clip_drag.trim_old_in_point,
                                    clip.in_point,
                                );
                                editing.record(Box::new(cmd));
                            }
                        }
                    }
                }
                _ => {}
            }
            clip_drag.mode = ClipDragMode::None;
            clip_drag.snapshots.clear();
            DispatchResult::structural()
        }
        PanelAction::RegionDragStarted(beat, layer) => {
            clip_drag.mode = ClipDragMode::RegionSelect;
            clip_drag.region_anchor_beat = *beat;
            clip_drag.region_anchor_layer = *layer;
            selection.clear();
            DispatchResult::handled()
        }
        PanelAction::RegionDragMoved(beat, layer) => {
            if clip_drag.mode == ClipDragMode::RegionSelect {
                let min_beat = clip_drag.region_anchor_beat.min(*beat);
                let max_beat = clip_drag.region_anchor_beat.max(*beat);
                let min_layer = clip_drag.region_anchor_layer.min(*layer);
                let max_layer = clip_drag.region_anchor_layer.max(*layer);

                // Set visual region on viewport
                ui.viewport.set_selection_region(Some(
                    manifold_ui::panels::viewport::SelectionRegion {
                        start_beat: min_beat,
                        end_beat: max_beat,
                        start_layer: min_layer,
                        end_layer: max_layer,
                    }
                ));

                // Select clips within region
                if let Some(project) = engine.project() {
                    let region = manifold_core::selection::SelectionRegion {
                        start_beat: min_beat,
                        end_beat: max_beat,
                        start_layer_index: min_layer as i32,
                        end_layer_index: max_layer as i32,
                        is_active: true,
                    };
                    let clips_in_region = EditingService::get_clips_in_region(project, &region);
                    selection.selected_clip_ids.clear();
                    for (_, clip_id) in clips_in_region {
                        selection.selected_clip_ids.insert(clip_id);
                    }
                    selection.primary_clip_id = selection.selected_clip_ids.iter().next().cloned();
                    selection.version += 1;
                }
            }
            DispatchResult::handled()
        }
        PanelAction::RegionDragEnded => {
            if clip_drag.mode == ClipDragMode::RegionSelect {
                clip_drag.mode = ClipDragMode::None;
                // Clear visual region but keep selection
                ui.viewport.set_selection_region(None);
            }
            DispatchResult::handled()
        }
        PanelAction::ViewportHoverChanged(_clip_id) => {
            // Hover state is already tracked on viewport panel
            DispatchResult::handled()
        }

        // ── Clock/Sync ─────────────────────────────────────────────
        PanelAction::CycleClockAuthority => {
            if let Some(project) = engine.project_mut() {
                project.settings.clock_authority = project.settings.clock_authority.next();
                log::info!("Clock authority → {}", project.settings.clock_authority.display_name());
            }
            DispatchResult::handled()
        }
        PanelAction::ToggleLink => {
            log::info!("Toggle Link (not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ToggleMidiClock => {
            log::info!("Toggle MIDI Clock (not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::SelectClkDevice => {
            log::info!("Select clock device (not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ToggleSyncOutput => {
            log::info!("Toggle sync output (not yet implemented)");
            DispatchResult::handled()
        }

        // ── Export/Header/Footer ───────────────────────────────────
        PanelAction::ToggleHdr => {
            if let Some(project) = engine.project_mut() {
                project.settings.export_hdr = !project.settings.export_hdr;
                log::info!("HDR export → {}", project.settings.export_hdr);
            }
            DispatchResult::handled()
        }
        PanelAction::TogglePercussion => {
            log::info!("Toggle percussion (not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ToggleMonitor => {
            log::info!("Toggle monitor (not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::CycleQuantize => {
            if let Some(project) = engine.project_mut() {
                let old = project.settings.quantize_mode;
                let new = old.next();
                let cmd = ChangeQuantizeModeCommand::new(old, new);
                editing.execute(Box::new(cmd), project);
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
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_muted = !layer.is_muted;
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ToggleSolo(idx) => {
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_solo = !layer.is_solo;
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerClicked(idx) => {
            *active_layer = Some(*idx);
            DispatchResult::handled()
        }
        PanelAction::LayerDoubleClicked(_idx) => {
            log::debug!("Layer double-clicked (rename not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ChevronClicked(idx) => {
            if let Some(project) = engine.project_mut() {
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
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get(*idx) {
                    let old_mode = layer.default_blend_mode;
                    if let Some(new_mode) = BlendMode::ALL.iter().find(|m| format!("{:?}", m) == *mode_str) {
                        let cmd = ChangeLayerBlendModeCommand::new(*idx, old_mode, *new_mode);
                        editing.execute(Box::new(cmd), project);
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ExpandLayer(idx) => {
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_collapsed = false;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::CollapseLayer(idx) => {
            if let Some(project) = engine.project_mut() {
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
            let beat = engine.current_beat();
            if let Some(project) = engine.project_mut() {
                let cmd = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                editing.execute(cmd, project);
            }
            DispatchResult::structural()
        }
        PanelAction::AddGenClipClicked(idx) => {
            let beat = engine.current_beat();
            if let Some(project) = engine.project_mut() {
                let cmd = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                editing.execute(cmd, project);
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
            log::debug!("Layer drag started");
            DispatchResult::handled()
        }
        PanelAction::LayerDragMoved(_from, _to) => {
            DispatchResult::handled()
        }
        PanelAction::LayerDragEnded(_from, _to) => {
            log::debug!("Layer drag ended (reorder not yet implemented via drag)");
            DispatchResult::handled()
        }

        // ── Layer management ───────────────────────────────────────
        PanelAction::AddLayerClicked => {
            if let Some(project) = engine.project_mut() {
                let count = project.timeline.layers.len();
                let name = format!("Layer {}", count + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorType::None,
                    count,
                    None,
                );
                editing.execute(Box::new(cmd), project);
            }
            DispatchResult::structural()
        }
        PanelAction::DeleteLayerClicked(idx) => {
            if let Some(project) = engine.project_mut() {
                if project.timeline.layers.len() > 1 {
                    if let Some(layer) = project.timeline.layers.get(*idx) {
                        let layer_clone = layer.clone();
                        let cmd = DeleteLayerCommand::new(layer_clone, *idx);
                        editing.execute(Box::new(cmd), project);
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
            if let Some(project) = engine.project() {
                *drag_snapshot = Some(project.settings.master_opacity);
            }
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityChanged(val) => {
            if let Some(project) = engine.project_mut() {
                project.settings.master_opacity = *val;
            }
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(project) = engine.project_mut() {
                    let new_val = project.settings.master_opacity;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                        editing.record(Box::new(cmd));
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::MasterCollapseToggle | PanelAction::MasterExitPathClicked => {
            DispatchResult::handled()
        }
        PanelAction::MasterOpacityRightClick => {
            // Reset master opacity to 1.0
            if let Some(project) = engine.project_mut() {
                let old = project.settings.master_opacity;
                if (old - 1.0).abs() > f32::EPSILON {
                    project.settings.master_opacity = 1.0;
                    let cmd = ChangeMasterOpacityCommand::new(old, 1.0);
                    editing.record(Box::new(cmd));
                }
            }
            DispatchResult::handled()
        }

        // ── Layer chrome ───────────────────────────────────────────
        PanelAction::LayerOpacitySnapshot => {
            if let Some(idx) = *active_layer {
                if let Some(project) = engine.project() {
                    if let Some(layer) = project.timeline.layers.get(idx) {
                        *drag_snapshot = Some(layer.opacity);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityChanged(val) => {
            if let Some(idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
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
                    if let Some(project) = engine.project_mut() {
                        if let Some(layer) = project.timeline.layers.get(idx) {
                            let new_val = layer.opacity;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = ChangeLayerOpacityCommand::new(idx, old_val, new_val);
                                editing.record(Box::new(cmd));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::LayerChromeCollapseToggle => {
            DispatchResult::handled()
        }
        PanelAction::LayerOpacityRightClick => {
            // Reset layer opacity to 1.0
            if let Some(idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(idx) {
                        let old = layer.opacity;
                        if (old - 1.0).abs() > f32::EPSILON {
                            layer.opacity = 1.0;
                            let cmd = ChangeLayerOpacityCommand::new(idx, old, 1.0);
                            editing.record(Box::new(cmd));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Clip chrome ────────────────────────────────────────────
        PanelAction::ClipChromeCollapseToggle => {
            // Handled by inspector rebuild (toggle state on clip_chrome panel).
            DispatchResult::handled()
        }
        PanelAction::ClipBpmClicked => {
            log::debug!("Clip BPM clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &selection.primary_clip_id {
                let clip_id = clip_id.clone();
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                        let old_loop = clip.is_looping;
                        let old_dur = clip.loop_duration_beats;
                        let cmd = ChangeClipLoopCommand::new(
                            clip_id, old_loop, !old_loop, old_dur, old_dur,
                        );
                        editing.execute(Box::new(cmd), project);
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipSlipSnapshot => {
            if let Some(clip_id) = &selection.primary_clip_id {
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                        *drag_snapshot = Some(clip.in_point);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipChanged(val) => {
            if let Some(clip_id) = &selection.primary_clip_id {
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                        clip.in_point = val.max(0.0);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipSlipCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(clip_id) = &selection.primary_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(project) = engine.project_mut() {
                        if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                            let new_val = clip.in_point;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = SlipClipCommand::new(clip_id, old_val, new_val);
                                editing.record(Box::new(cmd));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            if let Some(clip_id) = &selection.primary_clip_id {
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                        *drag_snapshot = Some(clip.loop_duration_beats);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopChanged(val) => {
            if let Some(clip_id) = &selection.primary_clip_id {
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                        clip.loop_duration_beats = val.max(0.0);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopCommit => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(clip_id) = &selection.primary_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(project) = engine.project_mut() {
                        if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                            let new_val = clip.loop_duration_beats;
                            let is_looping = clip.is_looping;
                            if (old_val - new_val).abs() > f32::EPSILON {
                                let cmd = ChangeClipLoopCommand::new(
                                    clip_id, is_looping, is_looping, old_val, new_val,
                                );
                                editing.record(Box::new(cmd));
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        PanelAction::ClipSlipRightClick => {
            // Reset clip slip (in_point) to 0.0
            if let Some(clip_id) = &selection.primary_clip_id {
                let clip_id = clip_id.clone();
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                        let old = clip.in_point;
                        if old.abs() > f32::EPSILON {
                            clip.in_point = 0.0;
                            let cmd = SlipClipCommand::new(clip_id, old, 0.0);
                            editing.record(Box::new(cmd));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClipLoopRightClick => {
            // Reset clip loop duration to clip's full duration
            if let Some(clip_id) = &selection.primary_clip_id {
                let clip_id = clip_id.clone();
                if let Some(project) = engine.project_mut() {
                    if let Some(clip) = project.timeline.find_clip_by_id_mut(&clip_id) {
                        let old_dur = clip.loop_duration_beats;
                        let full_dur = clip.duration_beats;
                        let is_looping = clip.is_looping;
                        if (old_dur - full_dur).abs() > f32::EPSILON {
                            clip.loop_duration_beats = full_dur;
                            let cmd = ChangeClipLoopCommand::new(
                                clip_id, is_looping, is_looping, old_dur, full_dur,
                            );
                            editing.record(Box::new(cmd));
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
            if let Some(project) = engine.project_mut() {
                let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
                if let Some(effects) = effects_ref {
                    if let Some(fx) = effects.get(*fx_idx) {
                        let old = fx.enabled;
                        let cmd = ToggleEffectCommand::new(target, *fx_idx, old, !old);
                        editing.execute(Box::new(cmd), project);
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(_) | PanelAction::EffectCardClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::EffectParamRightClick(fx_idx, param_idx, default_val) => {
            let tab = ui.inspector.last_effect_tab();
            if let Some(project) = engine.project_mut() {
                let (effects_mut, target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*fx_idx) {
                        let old = fx.param_values.get(*param_idx).copied().unwrap_or(0.0);
                        if (old - *default_val).abs() > f32::EPSILON {
                            while fx.param_values.len() <= *param_idx {
                                fx.param_values.push(0.0);
                            }
                            fx.param_values[*param_idx] = *default_val;
                            let cmd = ChangeEffectParamCommand::new(
                                target, *fx_idx, *param_idx, old, *default_val,
                            );
                            editing.record(Box::new(cmd));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamSnapshot(fx_idx, param_idx) => {
            let tab = ui.inspector.last_effect_tab();
            if let Some(project) = engine.project() {
                let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                    *drag_snapshot = Some(
                        fx.param_values.get(*param_idx).copied().unwrap_or(0.0)
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamChanged(fx_idx, param_idx, val) => {
            let tab = ui.inspector.last_effect_tab();
            if let Some(project) = engine.project_mut() {
                let (effects_mut, _target) = resolve_effects_mut(tab, project, *active_layer, selection);
                if let Some(effects) = effects_mut {
                    if let Some(fx) = effects.get_mut(*fx_idx) {
                        while fx.param_values.len() <= *param_idx {
                            fx.param_values.push(0.0);
                        }
                        fx.param_values[*param_idx] = *val;
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamCommit(fx_idx, param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                let tab = ui.inspector.last_effect_tab();
                if let Some(project) = engine.project() {
                    let effects = resolve_effects_ref(tab, project, *active_layer, selection);
                    if let Some(fx) = effects.and_then(|e| e.get(*fx_idx)) {
                        let new_val = fx.param_values.get(*param_idx)
                            .copied().unwrap_or(0.0);
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let target = resolve_effect_target(tab, *active_layer);
                            let cmd = ChangeEffectParamCommand::new(
                                target, *fx_idx, *param_idx, old_val, new_val,
                            );
                            editing.record(Box::new(cmd));
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::EffectDriverToggle(ei, pi) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    let target = DriverTarget::Effect {
                        effect_target: EffectTarget::Layer { layer_index: layer_idx },
                        effect_index: *ei,
                    };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(effects) = &layer.effects {
                            if let Some(fx) = effects.get(*ei) {
                                let driver_idx = fx.drivers.as_ref()
                                    .and_then(|ds| ds.iter().position(|d| d.param_index == *pi as i32));
                                if let Some(di) = driver_idx {
                                    let old = fx.drivers.as_ref().unwrap()[di].enabled;
                                    let cmd = ToggleDriverEnabledCommand::new(
                                        target, di, old, !old,
                                    );
                                    editing.execute(Box::new(cmd), project);
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
                                    };
                                    let cmd = AddDriverCommand::new(target, driver);
                                    editing.execute(Box::new(cmd), project);
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvelopeToggle(ei, pi) => {
            // Envelopes live on the layer, not on the effect instance.
            // Toggle enabled if one exists for this effect+param, otherwise add.
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        let effect_type = layer.effects.as_ref()
                            .and_then(|fx| fx.get(*ei))
                            .map(|fx| fx.effect_type);
                        if let Some(et) = effect_type {
                            let envs = layer.envelopes_mut();
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
                                });
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectDriverConfig(ei, pi, cfg) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    let target = DriverTarget::Effect {
                        effect_target: EffectTarget::Layer { layer_index: layer_idx },
                        effect_index: *ei,
                    };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(effects) = &layer.effects {
                            if let Some(fx) = effects.get(*ei) {
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
                                                editing.execute(Box::new(cmd), project);
                                            }
                                        }
                                        DriverConfigAction::Wave(idx) => {
                                            if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                                let cmd = ChangeDriverWaveformCommand::new(
                                                    target, di, driver.waveform, new_wave,
                                                );
                                                editing.execute(Box::new(cmd), project);
                                            }
                                        }
                                        DriverConfigAction::Dot => {
                                            if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                                let cmd = ChangeDriverBeatDivCommand::new(
                                                    target, di, driver.beat_division, new_div,
                                                );
                                                editing.execute(Box::new(cmd), project);
                                            }
                                        }
                                        DriverConfigAction::Triplet => {
                                            if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                                let cmd = ChangeDriverBeatDivCommand::new(
                                                    target, di, driver.beat_division, new_div,
                                                );
                                                editing.execute(Box::new(cmd), project);
                                            }
                                        }
                                        DriverConfigAction::Reverse => {
                                            let cmd = ToggleDriverReversedCommand::new(
                                                target, di, driver.reversed, !driver.reversed,
                                            );
                                            editing.execute(Box::new(cmd), project);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::EffectEnvParamChanged(ei, pi, param, val) => {
            // Live ADSR mutation during drag (no undo — commit-less slider).
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        let effect_type = layer.effects.as_ref()
                            .and_then(|fx| fx.get(*ei))
                            .map(|fx| fx.effect_type);
                        if let Some(et) = effect_type {
                            let envs = layer.envelopes_mut();
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
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTrimChanged(ei, pi, min, max) => {
            // Live trim mutation during drag.
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(effects) = &mut layer.effects {
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
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectTargetChanged(ei, pi, norm) => {
            // Live target normalized mutation during drag.
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        let effect_type = layer.effects.as_ref()
                            .and_then(|fx| fx.get(*ei))
                            .map(|fx| fx.effect_type);
                        if let Some(et) = effect_type {
                            let envs = layer.envelopes_mut();
                            if let Some(env) = envs.iter_mut().find(|e|
                                e.target_effect_type == et && e.param_index == *pi as i32
                            ) {
                                env.target_normalized = *norm;
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::RemoveEffect(fx_idx) => {
            let tab = ui.inspector.last_effect_tab();
            if let Some(project) = engine.project_mut() {
                let (effects_ref, target) = resolve_effects_read(tab, project, *active_layer, selection);
                if let Some(effects) = effects_ref {
                    if let Some(fx) = effects.get(*fx_idx) {
                        let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                        editing.execute(Box::new(cmd), project);
                    }
                }
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
                if let Some(project) = engine.project() {
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(gp) = &layer.gen_params {
                            *drag_snapshot = Some(
                                gp.param_values.get(*param_idx).copied().unwrap_or(0.0)
                            );
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamChanged(param_idx, val) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            while gp.param_values.len() <= *param_idx {
                                gp.param_values.push(0.0);
                            }
                            gp.param_values[*param_idx] = *val;
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamCommit(param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(layer_idx) = *active_layer {
                    if let Some(project) = engine.project() {
                        if let Some(layer) = project.timeline.layers.get(layer_idx) {
                            if let Some(gp) = &layer.gen_params {
                                let new_val = gp.param_values.get(*param_idx)
                                    .copied().unwrap_or(0.0);
                                if (old_val - new_val).abs() > f32::EPSILON {
                                    let mut old_params = gp.param_values.clone();
                                    if *param_idx < old_params.len() {
                                        old_params[*param_idx] = old_val;
                                    }
                                    let new_params = gp.param_values.clone();
                                    let cmd = ChangeGeneratorParamsCommand::new(
                                        layer_idx, old_params, new_params,
                                    );
                                    editing.record(Box::new(cmd));
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
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            while gp.param_values.len() <= *param_idx {
                                gp.param_values.push(0.0);
                            }
                            let cur = gp.param_values[*param_idx];
                            gp.param_values[*param_idx] = if cur > 0.5 { 0.0 } else { 1.0 };
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::GenParamRightClick(param_idx, default_val) => {
            // Reset generator param to its default value
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        if let Some(gp) = &mut layer.gen_params {
                            let old = gp.param_values.get(*param_idx).copied().unwrap_or(0.0);
                            if (old - *default_val).abs() > f32::EPSILON {
                                while gp.param_values.len() <= *param_idx {
                                    gp.param_values.push(0.0);
                                }
                                let old_params = gp.param_values.clone();
                                gp.param_values[*param_idx] = *default_val;
                                let new_params = gp.param_values.clone();
                                let cmd = ChangeGeneratorParamsCommand::new(
                                    layer_idx, old_params, new_params,
                                );
                                editing.record(Box::new(cmd));
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
                if let Some(project) = engine.project_mut() {
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
                                editing.execute(Box::new(cmd), project);
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
                                };
                                let cmd = AddDriverCommand::new(target, driver);
                                editing.execute(Box::new(cmd), project);
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
                if let Some(project) = engine.project_mut() {
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
                if let Some(project) = engine.project_mut() {
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
                                            editing.execute(Box::new(cmd), project);
                                        }
                                    }
                                    DriverConfigAction::Wave(idx) => {
                                        if let Some(new_wave) = DriverWaveform::from_index(*idx) {
                                            let cmd = ChangeDriverWaveformCommand::new(
                                                target, di, driver.waveform, new_wave,
                                            );
                                            editing.execute(Box::new(cmd), project);
                                        }
                                    }
                                    DriverConfigAction::Dot => {
                                        if let Some(new_div) = driver.beat_division.toggle_dotted() {
                                            let cmd = ChangeDriverBeatDivCommand::new(
                                                target, di, driver.beat_division, new_div,
                                            );
                                            editing.execute(Box::new(cmd), project);
                                        }
                                    }
                                    DriverConfigAction::Triplet => {
                                        if let Some(new_div) = driver.beat_division.toggle_triplet() {
                                            let cmd = ChangeDriverBeatDivCommand::new(
                                                target, di, driver.beat_division, new_div,
                                            );
                                            editing.execute(Box::new(cmd), project);
                                        }
                                    }
                                    DriverConfigAction::Reverse => {
                                        let cmd = ToggleDriverReversedCommand::new(
                                            target, di, driver.reversed, !driver.reversed,
                                        );
                                        editing.execute(Box::new(cmd), project);
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
                if let Some(project) = engine.project_mut() {
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
                if let Some(project) = engine.project_mut() {
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
                if let Some(project) = engine.project_mut() {
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

        // ── File operations (stubs — no I/O yet) ───────────────────
        PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs
        | PanelAction::ExportVideo
        | PanelAction::ExportXml => {
            log::info!("File action: {:?} (not yet wired)", action);
            DispatchResult::handled()
        }

        // ── Dropdown results (context-routed from UIRoot) ────────────
        PanelAction::SetMidiNote(layer_idx, note) => {
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                    let old_note = layer.midi_note;
                    let cmd = manifold_editing::commands::settings::ChangeLayerMidiNoteCommand::new(
                        *layer_idx, old_note, *note,
                    );
                    editing.execute(Box::new(cmd), project);
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiChannel(layer_idx, channel) => {
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get_mut(*layer_idx) {
                    layer.midi_channel = *channel;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetResolution(preset_idx) => {
            use manifold_core::types::ResolutionPreset;
            if let Some(project) = engine.project_mut() {
                let old = project.settings.resolution_preset;
                if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                    let cmd = manifold_editing::commands::settings::ChangeResolutionCommand::new(old, new);
                    editing.execute(Box::new(cmd), project);
                }
            }
            DispatchResult::handled()
        }
        PanelAction::AddEffect(tab, effect_type_idx) => {
            use manifold_core::types::EffectType;
            let Some(effect_type) = EffectType::from_index(*effect_type_idx) else {
                return DispatchResult::handled();
            };
            let defaults: Vec<f32> = effect_type.param_defs().iter()
                .map(|&(_, _, _, default, _)| default)
                .collect();
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
            if let Some(project) = engine.project_mut() {
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
                editing.execute(Box::new(cmd), project);
            }
            DispatchResult::structural()
        }
        PanelAction::SetGenType(layer_idx, gen_type_idx) => {
            if let Some(project) = engine.project_mut() {
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
                        editing.execute(Box::new(cmd), project);
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
            let beat = engine.current_beat();
            if let Some(project) = engine.project_mut() {
                let spb = 60.0 / project.settings.bpm;
                if let Some(cmd) = EditingService::split_clip_at_beat(project, clip_id, beat, spb) {
                    editing.execute(cmd, project);
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextDeleteClip(clip_id) => {
            if let Some(project) = engine.project_mut() {
                let commands = EditingService::delete_clips(project, &[clip_id.clone()], None, 0.0);
                if !commands.is_empty() {
                    editing.execute_batch(commands, "Delete clip".into(), project);
                }
            }
            selection.selected_clip_ids.remove(clip_id);
            DispatchResult::structural()
        }
        PanelAction::ContextDuplicateClip(clip_id) => {
            if let Some(project) = engine.project_mut() {
                // Calculate region from the single clip for proper offset
                let mut region = manifold_core::selection::SelectionRegion::default();
                if let Some(clip) = project.timeline.find_clip_by_id(clip_id) {
                    region.start_beat = clip.start_beat;
                    region.end_beat = clip.start_beat + clip.duration_beats;
                    region.is_active = true;
                }
                let commands = EditingService::duplicate_clips(project, &[clip_id.clone()], &region);
                if !commands.is_empty() {
                    editing.execute_batch(commands, "Duplicate clip".into(), project);
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextPasteAtTrack(beat, layer) => {
            let snapped = ui.viewport.snap_to_grid(*beat);
            if let Some(project) = engine.project_mut() {
                let spb = 60.0 / project.settings.bpm;
                let result = editing.paste_clips(project, snapped, *layer as i32, spb);
                if !result.commands.is_empty() {
                    editing.execute_batch(result.commands, "Paste clips".into(), project);
                    selection.selected_clip_ids.clear();
                    for id in result.pasted_clip_ids {
                        selection.selected_clip_ids.insert(id);
                    }
                    selection.primary_clip_id = selection.selected_clip_ids.iter().next().cloned();
                    selection.version += 1;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddVideoLayer(after_layer) => {
            if let Some(project) = engine.project_mut() {
                let idx = after_layer + 1;
                let name = format!("Layer {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorType::None,
                    idx,
                    None,
                );
                editing.execute(Box::new(cmd), project);
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddGeneratorLayer(after_layer) => {
            if let Some(project) = engine.project_mut() {
                let idx = after_layer + 1;
                let name = format!("Gen {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Generator,
                    GeneratorType::Plasma,
                    idx,
                    None,
                );
                editing.execute(Box::new(cmd), project);
            }
            DispatchResult::structural()
        }

        PanelAction::ContextDeleteLayer(layer_idx) => {
            if let Some(project) = engine.project_mut() {
                let idx = *layer_idx;
                if project.timeline.layers.len() > 1 && idx < project.timeline.layers.len() {
                    let layer = project.timeline.layers[idx].clone();
                    let cmd = DeleteLayerCommand::new(layer, idx);
                    editing.execute(Box::new(cmd), project);
                }
            }
            DispatchResult::structural()
        }

        PanelAction::LayerHeaderRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown — should not reach dispatch
            DispatchResult::handled()
        }

        // Generic dropdown fallback (should not normally fire)
        PanelAction::DropdownSelected(index) => {
            log::debug!("Dropdown selected: {} (no context)", index);
            DispatchResult::handled()
        }
    }
}

/// Handle undo (called from keyboard shortcut).
pub fn undo(engine: &mut PlaybackEngine, editing: &mut EditingService) -> bool {
    if let Some(project) = engine.project_mut() {
        editing.undo(project)
    } else {
        false
    }
}

/// Handle redo (called from keyboard shortcut).
pub fn redo(engine: &mut PlaybackEngine, editing: &mut EditingService) -> bool {
    if let Some(project) = engine.project_mut() {
        editing.redo(project)
    } else {
        false
    }
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
            let effects = selection.primary_clip_id.as_ref().and_then(|cid| {
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
            let clip_id = selection.primary_clip_id.clone();
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

/// Check auto-scroll and return true if viewport scroll changed (needs rebuild).
/// Must run BEFORE build() so the rebuild includes the new scroll position.
pub fn check_auto_scroll(ui: &mut UIRoot, engine: &PlaybackEngine) -> bool {
    let playhead_beat = engine.current_beat();
    let mut scroll_changed = false;
    if engine.is_playing() {
        let ppb = ui.viewport.pixels_per_beat();
        let tracks_w = ui.viewport.viewport_rect().width;
        let scroll_x = ui.viewport.scroll_x_beats();
        let visible_end_beat = scroll_x + tracks_w / ppb;

        if playhead_beat > visible_end_beat {
            ui.viewport.set_scroll(
                playhead_beat - tracks_w * 0.1 / ppb,
                ui.viewport.scroll_y_px(),
            );
            scroll_changed = true;
        } else if playhead_beat < scroll_x {
            ui.viewport.set_scroll(
                (playhead_beat - tracks_w * 0.2 / ppb).max(0.0),
                ui.viewport.scroll_y_px(),
            );
            scroll_changed = true;
        }
    }
    scroll_changed
}

/// Push engine state into UI panels (called once per frame, AFTER build).
/// Syncs all data-model state into tree nodes so the renderer shows current values.
pub fn push_state(ui: &mut UIRoot, engine: &PlaybackEngine, active_layer: Option<usize>, selection: &SelectionState) {
    let tree = &mut ui.tree;

    // Transport state
    let is_playing = engine.is_playing();
    let (play_text, play_color) = if is_playing {
        ("PLAY", PLAY_ACTIVE)
    } else {
        ("PLAY", PLAY_GREEN)
    };
    ui.transport.set_play_state(tree, play_text, play_color);

    // Time display + BPM
    let beat = engine.current_beat();
    let time = engine.current_time();

    if let Some(project) = engine.project() {
        let bpm = project.settings.bpm;
        let bar = (beat / 4.0).floor() as i32 + 1;
        let beat_in_bar = (beat % 4.0).floor() as i32 + 1;
        let sub = ((beat % 1.0) * 4.0).floor() as i32 + 1;
        let beat_text = format!("{:02}.{}.{}", bar, beat_in_bar, sub);

        let mins = (time / 60.0).floor() as i32;
        let secs = time % 60.0;
        let display = format!("{} | {:02}:{:05.2}", beat_text, mins, secs);

        ui.header.set_time_display(tree, &display);
        ui.transport.set_bpm_text(tree, &format!("{:.1}", bpm));

        // Clock authority display
        let auth = project.settings.clock_authority;
        let auth_color = match auth {
            manifold_core::types::ClockAuthority::Internal => color::BUTTON_INACTIVE_C32,
            manifold_core::types::ClockAuthority::Link => color::LINK_ORANGE,
            manifold_core::types::ClockAuthority::MidiClock => color::MIDI_PURPLE,
            manifold_core::types::ClockAuthority::Osc => color::SYNC_ACTIVE,
        };
        ui.transport.set_clock_authority(tree, auth.display_name(), auth_color);

        // Sync source status (default inactive — no actual connections yet)
        ui.transport.set_link_state(tree, false, color::STATUS_DOT_INACTIVE, "—", color::TEXT_DIMMED_C32);
        ui.transport.set_clk_state(tree, false, "—", color::STATUS_DOT_INACTIVE, "—", color::TEXT_DIMMED_C32);
        ui.transport.set_sync_state(tree, false, color::STATUS_DOT_INACTIVE, "—", color::TEXT_DIMMED_C32);

        // Record state
        ui.transport.set_record_state(tree, engine.is_recording(), true);

        // BPM tap/reset buttons (inactive until tapped)
        ui.transport.set_bpm_reset_active(tree, false);
        ui.transport.set_bpm_clear_active(tree, false);

        // Save button — no dirty tracking yet, show clean state
        ui.transport.set_save_text(tree, "Save");

        // Export state
        let has_export_range = project.timeline.export_in_beat < project.timeline.export_out_beat;
        if has_export_range {
            let export_label = format!("Export {:.1}-{:.1}", project.timeline.export_in_beat, project.timeline.export_out_beat);
            ui.transport.set_export_label(tree, &export_label);
        } else {
            ui.transport.set_export_label(tree, "Export");
        }
        ui.transport.set_export_active(tree, false); // No active export in Rust port yet
        ui.transport.set_hdr_active(tree, project.settings.export_hdr);

        // Export range markers on viewport
        ui.viewport.set_export_range(project.timeline.export_in_beat, project.timeline.export_out_beat);

        // Header — project name + zoom label
        ui.header.set_project_name(tree, "Untitled"); // No project file path yet
        let ppb = ui.viewport.pixels_per_beat();
        let zoom_pct = (ppb / color::ZOOM_LEVELS[color::DEFAULT_ZOOM_INDEX]) * 100.0;
        ui.header.set_zoom_label(tree, &format!("{:.0}%", zoom_pct));

        // Footer — quantize mode, resolution, FPS
        ui.footer.set_quantize_text(tree, project.settings.quantize_mode.display_name());
        ui.footer.set_resolution_text(tree, project.settings.resolution_preset.display_name());
        ui.footer.set_fps_text(tree, &format!("{:.0} FPS", project.settings.frame_rate));
    }

    // Footer stats
    if let Some(project) = engine.project() {
        let layers = project.timeline.layers.len();
        let clips: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let info = format!("Layers: {} | Clips: {}", layers, clips);
        ui.footer.set_selection_info(tree, &info);
    }

    // Playhead + playing state
    let playhead_beat = engine.current_beat();
    ui.viewport.set_playhead(playhead_beat);
    ui.viewport.set_playing(engine.is_playing());

    // Selection → viewport
    ui.viewport.set_selected_clip_ids(
        selection.selected_clip_ids.iter().cloned().collect()
    );
    if let Some(beat) = selection.insert_cursor_beat {
        ui.viewport.set_insert_cursor(beat);
    }

    // Layer mute/solo state sync + active layer highlighting + labels
    ui.layer_headers.set_active_layer(active_layer);
    if let Some(project) = engine.project() {
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
        if let Some(project) = engine.project() {
            if let Some(layer) = project.timeline.layers.get(idx) {
                ui.inspector.layer_chrome_mut().sync_opacity(tree, layer.opacity);
                ui.inspector.layer_chrome_mut().sync_name(tree, &layer.name);
            }
            // Master opacity
            ui.inspector.master_chrome_mut().sync_opacity(tree, project.settings.master_opacity);
        }
    }

    // Sync clip chrome from primary selected clip
    if let Some(clip_id) = &selection.primary_clip_id {
        if let Some(project) = engine.project() {
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
                    let spb = 60.0 / engine.project().map_or(120.0, |p| p.settings.bpm);
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
    if let Some(project) = engine.project() {
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
        if let Some(clip_id) = &selection.primary_clip_id {
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
pub fn sync_project_data(ui: &mut UIRoot, engine: &PlaybackEngine, active_layer: Option<usize>) {
    if let Some(project) = engine.project() {
        // Layer data → LayerHeaderPanel
        let mut cumulative_y: f32 = 0.0;
        let layers: Vec<LayerInfo> = project.timeline.layers.iter().enumerate().map(|(i, layer)| {
            let track_h = if layer.is_collapsed { 48.0 } else { 140.0 };
            let y = cumulative_y;
            cumulative_y += track_h;
            LayerInfo {
                name: layer.name.clone(),
                layer_id: layer.layer_id.clone(),
                is_collapsed: layer.is_collapsed,
                is_group: false,
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
        let tracks: Vec<TrackInfo> = project.timeline.layers.iter().map(|layer| {
            TrackInfo {
                height: if layer.is_collapsed { 48.0 } else { 140.0 },
                is_muted: layer.is_muted,
                is_group: false,
                accent_color: None,
            }
        }).collect();
        ui.viewport.set_tracks(tracks);

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

    // Rebuild UI tree with the new data
    ui.build();
}

/// Sync inspector content for the active selection.
/// Called when the active layer changes or after structural mutations.
pub fn sync_inspector_data(
    ui: &mut UIRoot,
    engine: &PlaybackEngine,
    active_layer: Option<usize>,
) {
    let Some(project) = engine.project() else { return };

    // Master effects → inspector
    let master_configs = effects_to_configs(&project.settings.master_effects);
    ui.inspector.configure_master_effects(&master_configs);

    // Active layer effects + gen params → inspector
    if let Some(idx) = active_layer {
        if let Some(layer) = project.timeline.layers.get(idx) {
            // Layer effects
            let layer_effects = layer.effects.as_ref()
                .map(|e| effects_to_configs(e))
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

    // Rebuild to reflect new inspector content
    ui.build();
}

// ── Helpers ──────────────────────────────────────────────────────

/// Convert a slice of `EffectInstance` into `EffectCardConfig` for the UI.
fn effects_to_configs(effects: &[EffectInstance]) -> Vec<EffectCardConfig> {
    effects.iter().enumerate().map(|(i, fx)| {
        let defs = fx.effect_type.param_defs();
        let params: Vec<EffectParamInfo> = defs.iter().map(|&(name, min, max, default, whole)| {
            EffectParamInfo {
                name: name.to_string(),
                min,
                max,
                default,
                whole_numbers: whole,
            }
        }).collect();

        EffectCardConfig {
            effect_index: i,
            name: fx.effect_type.display_name().to_string(),
            enabled: fx.enabled,
            supports_envelopes: true,
            params,
        }
    }).collect()
}

/// Convert a `GeneratorParamState` into `GenParamConfig` for the UI.
fn gen_params_to_config(gp: &manifold_core::generator::GeneratorParamState) -> GenParamConfig {
    let defs = gp.generator_type.param_defs();
    let params: Vec<GenParamInfo> = defs.iter().map(|&(name, min, max, default, whole, toggle)| {
        GenParamInfo {
            name: name.to_string(),
            min,
            max,
            default,
            whole_numbers: whole,
            is_toggle: toggle,
        }
    }).collect();

    GenParamConfig {
        gen_type_name: gp.generator_type.display_name().to_string(),
        params,
    }
}
