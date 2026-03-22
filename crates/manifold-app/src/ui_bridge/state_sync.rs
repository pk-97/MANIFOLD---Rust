//! State synchronization: push_state, sync_project_data, sync_clip_positions,
//! sync_inspector_data, check_auto_scroll.
use manifold_core::effects::{EffectInstance, ParamEnvelope};
use manifold_core::project::Project;
use manifold_core::types::{GeneratorType, LayerType, BeatDivision};
use manifold_ui::node::Color32;
use manifold_ui::color;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::TrackInfo;
use manifold_ui::panels::effect_card::{EffectCardConfig, EffectParamInfo};
use manifold_ui::panels::gen_param::{GenParamConfig, GenParamInfo};

use crate::app::SelectionState;
use crate::ui_root::UIRoot;

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
        let export_active = project.timeline.export_in_beat > 0.0
            || project.timeline.export_out_beat > 0.0;
        ui.transport.set_export_active(tree, export_active);
        ui.transport.set_hdr_active(tree, project.settings.export_hdr);
        let perc_active = project.percussion_import.is_some();
        ui.transport.set_perc_active(tree, perc_active);

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
        let active_flags: Vec<bool> = project.timeline.layers.iter()
            .map(|l| selection.is_layer_active(&l.layer_id))
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
                "\u{2014}".into()
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
                    let spb = 60.0 / Some(project).map_or(120.0, |p| p.settings.bpm);
                    let clip_dur_s = clip.duration_beats * spb;
                    chrome.set_slip_range(clip_dur_s.max(1.0));
                    chrome.set_loop_range(clip.duration_beats.max(1.0));
                } else if is_gen {
                    chrome.sync_name(tree, clip.generator_type.display_name());
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
        if let Some(idx) = active_layer
            && let Some(layer) = project.timeline.layers.get(idx)
                && let Some(effects) = &layer.effects {
                    for (i, effect) in effects.iter().enumerate() {
                        if let Some(card) = ui.inspector.layer_effect_mut(i) {
                            card.sync_effect_name(tree, effect.effect_type.display_name());
                            card.sync_enabled(tree, effect.enabled);
                            card.sync_values(tree, &effect.param_values);
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
        if let Some(idx) = active_layer
            && let Some(layer) = project.timeline.layers.get(idx)
                && let Some(gp_state) = &layer.gen_params
                    && let Some(gp) = ui.inspector.gen_params_mut() {
                        gp.sync_gen_type_name(tree, gp_state.generator_type.display_name());
                        gp.sync_values(tree, &gp_state.param_values);
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
                layer_id: layer.layer_id.to_string(),
                is_collapsed: layer.is_collapsed,
                is_group: layer.is_group(),
                is_generator: layer.layer_type == LayerType::Generator,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                parent_layer_id: layer.parent_layer_id.as_ref().map(|id| id.to_string()),
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
        let tracks: Vec<TrackInfo> = project.timeline.layers.iter().map(|layer| {
            // Check if muted individually or by parent group
            let parent_muted = layer.parent_layer_id.as_ref().is_some_and(|pid| {
                project.timeline.layers.iter().any(|l| l.layer_id == *pid && l.is_muted)
            });
            let is_muted = layer.is_muted || parent_muted;

            // Variable track heights matching Unity CoordinateMapper.RebuildYLayout
            let height = if layer.parent_layer_id.is_some() {
                // Child of group: check parent collapsed
                let parent_collapsed = layer.parent_layer_id.as_ref().is_some_and(|pid| {
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
        ui.viewport.layer_ids = project.timeline.layers.iter()
            .map(|l| l.layer_id.clone()).collect();

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
                .map(gen_params_to_config);
            let layer_id = layer.layer_id.clone();
            ui.inspector.configure_gen_params(gen_config.as_ref(), Some(layer_id));
        } else {
            ui.inspector.configure_layer_effects(&[]);
            ui.inspector.configure_gen_params(None, None);
        }
    } else {
        ui.inspector.configure_layer_effects(&[]);
        ui.inspector.configure_gen_params(None, None);
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
            effect_id: fx.id.clone(),
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
