//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

use manifold_core::types::{EffectType, GeneratorType, LayerType, PlaybackState};
use manifold_core::effects::EffectInstance;
use manifold_editing::commands::settings::{
    ChangeMasterOpacityCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand,
};
use manifold_editing::commands::effects::{
    ToggleEffectCommand, ChangeEffectParamCommand,
};
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::service::EditingService;
use manifold_playback::engine::PlaybackEngine;
use manifold_ui::PanelAction;
use manifold_ui::node::Color32;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::TrackInfo;
use manifold_ui::panels::effect_card::{EffectCardConfig, EffectParamInfo};
use manifold_ui::panels::gen_param::{GenParamConfig, GenParamInfo};

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
        PanelAction::Seek(beat) => {
            if let Some(p) = engine.project() {
                let time = *beat * (60.0 / p.settings.bpm);
                engine.seek_to(time);
            }
            DispatchResult::handled()
        }

        // ── Zoom ───────────────────────────────────────────────────
        PanelAction::ZoomIn | PanelAction::ZoomOut => {
            // Zoom is UI-only state, handled in UIRoot.
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
        PanelAction::ChevronClicked(idx) => {
            if let Some(project) = engine.project_mut() {
                if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                    layer.is_collapsed = !layer.is_collapsed;
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
            // UI-only state (collapse) or unimplemented
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

        // ── Effect operations ──────────────────────────────────────
        PanelAction::EffectToggle(fx_idx) => {
            // Toggle on the active layer's effects (for now, layer scope)
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
        PanelAction::EffectCollapseToggle(_) | PanelAction::EffectCardClicked(_)
        | PanelAction::EffectParamRightClick(_, _) => {
            // UI-only state
            DispatchResult::handled()
        }

        // ── Generator params ───────────────────────────────────────
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
                                    let mut new_params = gp.param_values.clone();
                                    // Restore old value in old_params
                                    if *param_idx < old_params.len() {
                                        old_params[*param_idx] = old_val;
                                    }
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
        PanelAction::GenParamRightClick(_) | PanelAction::GenTypeClicked => {
            // Unimplemented (would need dropdown)
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

        // ── All other actions ──────────────────────────────────────
        _ => {
            log::debug!("Unhandled panel action: {:?}", action);
            DispatchResult::unhandled()
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
pub fn push_state(ui: &mut UIRoot, engine: &PlaybackEngine, active_layer: Option<usize>) {
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
