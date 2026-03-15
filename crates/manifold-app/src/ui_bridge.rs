//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

use manifold_core::types::{
    BlendMode, GeneratorType, LayerType, PlaybackState,
};
use manifold_core::effects::EffectInstance;
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
    ChangeQuantizeModeCommand, ChangeLayerBlendModeCommand,
};
use manifold_editing::commands::effects::{
    ToggleEffectCommand, ChangeEffectParamCommand, RemoveEffectCommand,
};
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::clip::{MoveClipCommand, TrimClipCommand};
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::service::EditingService;
use manifold_playback::engine::PlaybackEngine;
use manifold_ui::{PanelAction, InspectorTab};
use manifold_ui::node::Color32;
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
                // Select the newly created clip
                if let Some(new_layer) = project.timeline.layers.get(*layer) {
                    if let Some(new_clip) = new_layer.clips.last() {
                        selection.select_single(new_clip.id.clone());
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
            let snapped = ui.viewport.snap_to_grid(*current_beat);
            match clip_drag.mode {
                ClipDragMode::Move => {
                    let delta = snapped - ui.viewport.snap_to_grid(clip_drag.anchor_beat);
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
        PanelAction::ZoomIn | PanelAction::ZoomOut => {
            // Zoom is UI-only state, handled in UIRoot.
            DispatchResult::handled()
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
        PanelAction::MasterCollapseToggle | PanelAction::MasterExitPathClicked
        | PanelAction::MasterOpacityRightClick => {
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

        // ── Clip chrome ────────────────────────────────────────────
        PanelAction::ClipChromeCollapseToggle => {
            DispatchResult::handled()
        }
        PanelAction::ClipBpmClicked => {
            log::debug!("Clip BPM clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            log::debug!("Clip loop toggle (clip selection not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::ClipSlipSnapshot => {
            DispatchResult::handled()
        }
        PanelAction::ClipSlipChanged(_val) => {
            DispatchResult::handled()
        }
        PanelAction::ClipSlipCommit => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopSnapshot => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopChanged(_val) => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopCommit => {
            DispatchResult::handled()
        }

        // ── Effect operations ──────────────────────────────────────
        PanelAction::EffectToggle(fx_idx) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    let target = EffectTarget::Layer { layer_index: layer_idx };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(effects) = &layer.effects {
                            if let Some(fx) = effects.get(*fx_idx) {
                                let old = fx.enabled;
                                let cmd = ToggleEffectCommand::new(
                                    target, *fx_idx, old, !old,
                                );
                                editing.execute(Box::new(cmd), project);
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectCollapseToggle(_) | PanelAction::EffectCardClicked(_)
        | PanelAction::EffectParamRightClick(_, _) => {
            DispatchResult::handled()
        }
        PanelAction::EffectParamSnapshot(fx_idx, param_idx) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project() {
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(effects) = &layer.effects {
                            if let Some(fx) = effects.get(*fx_idx) {
                                *drag_snapshot = Some(
                                    fx.param_values.get(*param_idx).copied().unwrap_or(0.0)
                                );
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamChanged(fx_idx, param_idx, val) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(layer_idx) {
                        let effects = layer.effects_mut();
                        if let Some(fx) = effects.get_mut(*fx_idx) {
                            while fx.param_values.len() <= *param_idx {
                                fx.param_values.push(0.0);
                            }
                            fx.param_values[*param_idx] = *val;
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        PanelAction::EffectParamCommit(fx_idx, param_idx) => {
            if let Some(old_val) = drag_snapshot.take() {
                if let Some(layer_idx) = *active_layer {
                    if let Some(project) = engine.project() {
                        if let Some(layer) = project.timeline.layers.get(layer_idx) {
                            if let Some(effects) = &layer.effects {
                                if let Some(fx) = effects.get(*fx_idx) {
                                    let new_val = fx.param_values.get(*param_idx)
                                        .copied().unwrap_or(0.0);
                                    if (old_val - new_val).abs() > f32::EPSILON {
                                        let target = EffectTarget::Layer { layer_index: layer_idx };
                                        let cmd = ChangeEffectParamCommand::new(
                                            target, *fx_idx, *param_idx, old_val, new_val,
                                        );
                                        editing.record(Box::new(cmd));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        PanelAction::EffectDriverToggle(_ei, _pi) => {
            log::debug!("Effect driver toggle (modulation not yet wired)");
            DispatchResult::handled()
        }
        PanelAction::EffectEnvelopeToggle(_ei, _pi) => {
            log::debug!("Effect envelope toggle (modulation not yet wired)");
            DispatchResult::handled()
        }
        PanelAction::EffectDriverConfig(_ei, _pi, _cfg) => {
            log::debug!("Effect driver config (modulation not yet wired)");
            DispatchResult::handled()
        }
        PanelAction::EffectEnvParamChanged(_ei, _pi, _param, _val) => {
            DispatchResult::handled()
        }
        PanelAction::EffectTrimChanged(_ei, _pi, _min, _max) => {
            DispatchResult::handled()
        }
        PanelAction::EffectTargetChanged(_ei, _pi, _norm) => {
            DispatchResult::handled()
        }

        // ── Effect management ──────────────────────────────────────
        PanelAction::AddEffectClicked(_tab) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::RemoveEffect(fx_idx) => {
            if let Some(layer_idx) = *active_layer {
                if let Some(project) = engine.project_mut() {
                    let target = EffectTarget::Layer { layer_index: layer_idx };
                    if let Some(layer) = project.timeline.layers.get(layer_idx) {
                        if let Some(effects) = &layer.effects {
                            if let Some(fx) = effects.get(*fx_idx) {
                                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                                editing.execute(Box::new(cmd), project);
                            }
                        }
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
        PanelAction::GenParamRightClick(_) => {
            DispatchResult::handled()
        }

        // ── Gen modulation ─────────────────────────────────────────
        PanelAction::GenDriverToggle(_pi) => {
            log::debug!("Gen driver toggle (modulation not yet wired)");
            DispatchResult::handled()
        }
        PanelAction::GenEnvelopeToggle(_pi) => {
            log::debug!("Gen envelope toggle (modulation not yet wired)");
            DispatchResult::handled()
        }
        PanelAction::GenDriverConfig(_pi, _cfg) => {
            DispatchResult::handled()
        }
        PanelAction::GenEnvParamChanged(_pi, _param, _val) => {
            DispatchResult::handled()
        }
        PanelAction::GenTrimChanged(_pi, _min, _max) => {
            DispatchResult::handled()
        }
        PanelAction::GenTargetChanged(_pi, _norm) => {
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

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);

/// Push engine state into UI panels (called once per frame).
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
    }

    // Footer stats
    if let Some(project) = engine.project() {
        let layers = project.timeline.layers.len();
        let clips: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let info = format!("Layers: {} | Clips: {}", layers, clips);
        ui.footer.set_selection_info(tree, &info);
    }

    // Playhead + playing state
    ui.viewport.set_playhead(engine.current_beat());
    ui.viewport.set_playing(engine.is_playing());

    // Selection → viewport
    ui.viewport.set_selected_clip_ids(
        selection.selected_clip_ids.iter().cloned().collect()
    );
    if let Some(beat) = selection.insert_cursor_beat {
        ui.viewport.set_insert_cursor(beat);
    }

    // Layer mute/solo state sync
    if let Some(project) = engine.project() {
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            ui.layer_headers.set_mute_state(tree, i, layer.is_muted);
            ui.layer_headers.set_solo_state(tree, i, layer.is_solo);
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
}

/// Sync structural project data (layers, tracks) into UI panels.
/// Call once at init and whenever the project structure changes.
/// Triggers a full UI rebuild afterward.
pub fn sync_project_data(ui: &mut UIRoot, engine: &PlaybackEngine) {
    if let Some(project) = engine.project() {
        // Layer data → LayerHeaderPanel
        let layers: Vec<LayerInfo> = project.timeline.layers.iter().enumerate().map(|(i, layer)| {
            let track_h = if layer.is_collapsed { 48.0 } else { 140.0 };
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
                y_offset: i as f32 * track_h,
                height: track_h,
                is_selected: false,
            }
        }).collect();
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
