//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

use manifold_core::types::{LayerType, PlaybackState};
use manifold_playback::engine::PlaybackEngine;
use manifold_ui::PanelAction;
use manifold_ui::node::Color32;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::TrackInfo;

use crate::ui_root::UIRoot;

/// Dispatch a panel action to the engine. Returns true if handled.
pub fn dispatch(action: &PanelAction, engine: &mut PlaybackEngine) -> bool {
    match action {
        // ── Transport ──────────────────────────────────────────────
        PanelAction::PlayPause => {
            if engine.is_playing() {
                engine.set_state(PlaybackState::Paused);
            } else {
                engine.set_state(PlaybackState::Playing);
            }
            true
        }
        PanelAction::Stop => {
            engine.set_state(PlaybackState::Stopped);
            engine.seek_to(0.0);
            true
        }
        PanelAction::Seek(beat) => {
            let project = engine.project();
            if let Some(p) = project {
                let time = *beat * (60.0 / p.settings.bpm);
                engine.seek_to(time);
            }
            true
        }

        // ── Zoom ───────────────────────────────────────────────────
        PanelAction::ZoomIn | PanelAction::ZoomOut => {
            // Zoom is UI-only state, handled in UIRoot.
            true
        }

        // ── File operations (stubs — no EditingService yet) ────────
        PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs
        | PanelAction::ExportVideo
        | PanelAction::ExportXml => {
            log::info!("File action: {:?} (not yet wired)", action);
            true
        }

        // ── All other actions are logged but not yet wired ─────────
        _ => {
            log::debug!("Unhandled panel action: {:?}", action);
            false
        }
    }
}

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);

/// Push engine state into UI panels (called once per frame).
pub fn push_state(ui: &mut UIRoot, engine: &PlaybackEngine) {
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

    // Playhead + playing state (lightweight, every frame)
    ui.viewport.set_playhead(engine.current_beat());
    ui.viewport.set_playing(engine.is_playing());
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
