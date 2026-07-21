//! State synchronization: push_state, sync_project_data, sync_clip_positions,
//! sync_inspector_data, check_auto_scroll.
use manifold_core::PresetTypeId;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
use manifold_core::types::{BeatDivision, LayerType};
use manifold_core::Beats;
use manifold_ui::color;
use manifold_ui::node::Color32;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::param_card::{ParamCardKind, ParamCardStringInfo, RowMod};
use manifold_ui::panels::param_slider_shared::{AbletonMappingDisplay, AudioCardState, AudioRowState};
use manifold_ui::param_surface::{ParamRow, ParamSurface, RowMapping, RowSpec, RowValue};
use manifold_ui::panels::viewport::TrackInfo;

use crate::app::SelectionState;
use crate::ui_root::UIRoot;

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
const PAUSED_YELLOW: Color32 = Color32::new(209, 166, 38, 255);

/// Cached transport display strings — avoids per-frame `format!` allocations
/// when beat/time/bpm haven't changed (which is most frames when paused).
pub struct TransportDisplayCache {
    // Time display: "MM:SS.T  |  bar.beat.sixteenth"
    prev_mins: i32,
    prev_secs: i32,
    prev_tenths: i32,
    prev_bar: i32,
    prev_beat_in_bar: i32,
    prev_sixteenth: i32,
    cached_display: String,
    // BPM display: "120.0"
    prev_bpm_tenths: i32, // bpm * 10, rounded
    cached_bpm: String,
    // Link peers display: "1 peer" / "N peers"
    prev_link_peers: i32,
    cached_link_peers: String,
}

impl TransportDisplayCache {
    pub fn new() -> Self {
        Self {
            prev_mins: -1,
            prev_secs: -1,
            prev_tenths: -1,
            prev_bar: -1,
            prev_beat_in_bar: -1,
            prev_sixteenth: -1,
            cached_display: String::new(),
            prev_bpm_tenths: -1,
            cached_bpm: String::new(),
            prev_link_peers: -1,
            cached_link_peers: String::new(),
        }
    }

    /// Returns the formatted display string, only reformatting when values change.
    fn time_display(
        &mut self,
        mins: i32,
        secs: i32,
        tenths: i32,
        bar: i32,
        beat_in_bar: i32,
        sixteenth: i32,
    ) -> &str {
        if mins != self.prev_mins
            || secs != self.prev_secs
            || tenths != self.prev_tenths
            || bar != self.prev_bar
            || beat_in_bar != self.prev_beat_in_bar
            || sixteenth != self.prev_sixteenth
        {
            self.prev_mins = mins;
            self.prev_secs = secs;
            self.prev_tenths = tenths;
            self.prev_bar = bar;
            self.prev_beat_in_bar = beat_in_bar;
            self.prev_sixteenth = sixteenth;
            self.cached_display = format!(
                "{:02}:{:02}.{}  |  {}.{}.{}",
                mins, secs, tenths, bar, beat_in_bar, sixteenth,
            );
        }
        &self.cached_display
    }

    /// Returns the formatted BPM string, only reformatting when value changes.
    fn bpm_display(&mut self, bpm: f32) -> &str {
        let bpm_tenths = (bpm * 10.0).round() as i32;
        if bpm_tenths != self.prev_bpm_tenths {
            self.prev_bpm_tenths = bpm_tenths;
            self.cached_bpm = format!("{:.1}", bpm);
        }
        &self.cached_bpm
    }

    /// Returns the formatted Link peers string, only reformatting when count changes.
    pub fn link_peers_display(&mut self, peers: u32) -> &str {
        if peers as i32 != self.prev_link_peers {
            self.prev_link_peers = peers as i32;
            self.cached_link_peers = match peers {
                0 => String::new(),
                1 => "1 peer".to_string(),
                n => format!("{n} peers"),
            };
        }
        &self.cached_link_peers
    }
}

/// Check auto-scroll during playback and return true if viewport scroll changed.
/// Must run BEFORE build() so the rebuild includes the new scroll position.
/// From Unity ViewportManager.UpdatePlayheadPosition (lines 327-357).
/// BUG-159: playhead-follow yields to an active or just-finished user scroll
/// gesture (wheel, trackpad pan, scrollbar drag) instead of fighting it —
/// Ableton's feel. Re-engage is automatic: once this grace window elapses
/// with no further user gesture, the next `check_auto_scroll` call resumes
/// following on its own, no separate "re-engage" event needed.
const USER_SCROLL_GRACE: std::time::Duration = std::time::Duration::from_millis(800);

pub fn check_auto_scroll(
    ui: &mut UIRoot,
    content_state: &crate::content_state::ContentState,
    project: &Project,
) -> bool {
    if !content_state.is_playing {
        return false;
    }
    // BUG-159: a user scroll gesture (in progress, or within the grace
    // window) owns the viewport — auto-follow must not overwrite it.
    if ui.viewport.scrollbar_h_dragging() || ui.viewport.user_scroll_x_recent(USER_SCROLL_GRACE) {
        return false;
    }

    let playhead_beat = content_state.current_beat.as_f32();
    let ppb = ui.viewport.pixels_per_beat();
    let viewport_w = ui.viewport.tracks_rect().width;
    if viewport_w <= 0.0 || ppb <= 0.0 {
        return false;
    }

    let scroll_x_beats = ui.viewport.scroll_x_beats().as_f32();
    let playhead_px = (playhead_beat - scroll_x_beats) * ppb; // pixel offset from viewport left

    // Content expansion: if playhead approaches end of content, grow it.
    // From Unity ViewportManager.UpdatePlayheadPosition (lines 314-324).
    let content_beats = project.timeline.duration_beats();
    let content_w_px = content_beats.as_f32() * ppb;
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
        ui.viewport
            .set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    // Left edge margin: 20px. When playhead goes behind left edge, scroll back.
    let left_margin_px = 20.0;
    if playhead_px < left_margin_px {
        let target_scroll_beat = playhead_beat - left_margin_px / ppb;
        ui.viewport
            .set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    false
}

#[cfg(test)]
mod bug159_auto_scroll_yield_tests {
    use super::*;
    use manifold_core::Beats;

    fn playing_state(beat: f32) -> crate::content_state::ContentState {
        crate::content_state::ContentState {
            current_beat: Beats::from_f32(beat),
            is_playing: true,
            ..Default::default()
        }
    }

    /// A UIRoot laid out through the real production path (one `build()`
    /// pass, same as every live frame) so `viewport.tracks_rect()` is a real
    /// nonzero rect — `check_auto_scroll`'s edge margins (50px right, 20px
    /// left) need that to be reachable at all.
    fn wide_ui_root() -> UIRoot {
        let mut ui = UIRoot::new();
        ui.build();
        ui.viewport.set_zoom(20.0); // pixels-per-beat, so a few hundred beats span the viewport
        ui
    }

    #[test]
    fn auto_scroll_moves_when_no_user_gesture_is_active() {
        let mut ui = wide_ui_root();
        let project = Project::default();
        // Push the playhead far enough right to cross the right-edge margin.
        let state = playing_state(500.0);
        let moved = check_auto_scroll(&mut ui, &state, &project);
        assert!(moved, "auto-scroll must engage with no competing user gesture");
    }

    #[test]
    fn auto_scroll_yields_to_a_recent_user_scroll_gesture() {
        let mut ui = wide_ui_root();
        ui.viewport.note_user_scroll_x();
        let project = Project::default();
        let state = playing_state(500.0);
        let moved = check_auto_scroll(&mut ui, &state, &project);
        assert!(
            !moved,
            "auto-scroll must yield while a user scroll gesture is recent — \
             BUG-159's violent snap-back is exactly this check missing"
        );
    }
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
    transport_cache: &mut TransportDisplayCache,
) {
    let tree = &mut ui.tree;

    // Transport state — three visual states matching Unity TransportPanel
    let state = if content_state.is_playing {
        manifold_core::types::PlaybackState::Playing
    } else {
        manifold_core::types::PlaybackState::Stopped
    };
    let (play_text, play_color) = match state {
        manifold_core::types::PlaybackState::Playing => ("PAUSE", PLAY_ACTIVE),
        manifold_core::types::PlaybackState::Paused => ("PLAY", PAUSED_YELLOW),
        manifold_core::types::PlaybackState::Stopped => ("PLAY", PLAY_GREEN),
    };
    ui.transport.set_play_state(play_text, play_color);

    // Time display + BPM
    let beat = content_state.current_beat.as_f32();
    let time = content_state.current_time;

    {
        // When clock authority is Internal, use project.settings.bpm (the local
        // project) — it's updated immediately by handle_text_input_commit, so
        // the BPM field reflects user input without waiting for the content thread
        // round-trip. When an external source is active (Link, MIDI Clock, OSC),
        // use content_state.bpm which carries the live external tempo.
        let bpm = if content_state.clock_authority == manifold_core::types::ClockAuthority::Internal
        {
            project.settings.bpm.0
        } else {
            content_state.bpm as f32
        };

        // Unity FormatTime: "{minutes:D2}:{seconds:D2}.{tenths}"
        // Time first, then bar.beat.sixteenth — matches Unity exactly
        let t = time.0;
        let mins = (t / 60.0).floor() as i32;
        let secs = (t % 60.0).floor() as i32;
        let tenths = ((t * 10.0) % 10.0).floor() as i32;
        // Beat display uses time_signature_numerator (not hardcoded 4)
        let bpb = (project.settings.time_signature_numerator.max(1)) as f32;
        let bar = (beat / bpb).floor() as i32 + 1;
        let beat_in_bar = (beat % bpb).floor() as i32 + 1;
        let sixteenth = ((beat % 1.0) * 4.0).floor() as i32 + 1;

        let display = transport_cache.time_display(mins, secs, tenths, bar, beat_in_bar, sixteenth);
        ui.header.set_time_display(display);
        let bpm_str = transport_cache.bpm_display(bpm);
        ui.transport.set_bpm_text(bpm_str);

        // Clock authority display — "SRC:INT"/"SRC:LNK"/"SRC:CLK"/"SRC:OSC"
        // Use content_state (authoritative, auto-determined each content frame)
        let auth = content_state.clock_authority;
        let auth_color = match auth {
            manifold_core::types::ClockAuthority::Internal => color::BUTTON_INACTIVE_C32,
            manifold_core::types::ClockAuthority::Link => color::LINK_ORANGE,
            manifold_core::types::ClockAuthority::MidiClock => color::MIDI_PURPLE,
            manifold_core::types::ClockAuthority::Osc => color::ABLETON_LINK_BLUE,
        };
        ui.transport
            .set_clock_authority(auth.transport_label(), auth_color);

        // Cache MIDI device names for dropdown
        if ui.midi_device_names[..] != content_state.midi_device_names[..] {
            ui.midi_device_names.clear();
            ui.midi_device_names
                .extend_from_slice(&content_state.midi_device_names);
        }

        // D17 "export-complete green sweep" (`UI_CRAFT_AND_MOTION_PLAN.md` P2).
        // `export_finished` was written by the content thread but never read —
        // see the `FIXME(dead-code-audit)` on `ExportFinishedEvent` in
        // `content_state.rs`. `content_state` here is a cached snapshot
        // re-pushed every UI frame, not an edge-triggered event, so key on the
        // event's own identity to fire the toast exactly once per real export.
        if let Some(ev) = &content_state.export_finished {
            let key = format!("{}|{}|{}", ev.success, ev.message, ev.output_path);
            if ui.last_export_toast_key.as_deref() != Some(key.as_str()) {
                if ev.success {
                    ui.toast.show_with_accent(ev.message.clone(), color::GREEN_BASE);
                } else {
                    ui.toast.show_with_accent(ev.message.clone(), color::RED_BASE);
                }
                ui.last_export_toast_key = Some(key);
            }
        }

        // D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — real command
        // label instead of the generic "Undo"/"Redo" the M::Undo/M::Redo menu
        // handlers used to show directly (`app_render.rs`). Same re-fire guard
        // as the export toast above; keyed on `data_version` (bumped by every
        // undo/redo) rather than the description, so undoing the same command
        // twice in a row (rare, but possible via redo-then-undo) still fires.
        if let Some(ev) = &content_state.undo_redo_event {
            let key = content_state.data_version;
            if ui.last_undo_redo_toast_key != Some(key) {
                let verb = if ev.is_redo { "Redo" } else { "Undo" };
                ui.toast.show(format!("{verb}: {}", ev.description));
                ui.last_undo_redo_toast_key = Some(key);
            }
        }

        // Cache Ableton session for parameter mapping dropdown
        if let Some(session) = &content_state.ableton_session {
            ui.ableton_session = Some(std::sync::Arc::clone(session));
            // If the picker is open, refresh it with the updated session data.
            if ui.ableton_picker.is_open() {
                ui.ableton_picker
                    .update_session(crate::ui_root::build_picker_session(session));
                ui.overlay_dirty = true;
            }
        }

        // Sync source status — driven by content_state from transport controller
        // Link
        if !content_state.link_enabled {
            ui.transport.set_link_state(
                false,
                color::STATUS_DOT_INACTIVE,
                "Off",
                color::TEXT_DIMMED_C32,
            );
        } else if content_state.link_peers > 0 {
            let status = transport_cache.link_peers_display(content_state.link_peers as u32);
            ui.transport.set_link_state(
                true,
                color::STATUS_DOT_GREEN,
                status,
                color::TEXT_WHITE_C32,
            );
        } else {
            ui.transport.set_link_state(
                true,
                color::STATUS_DOT_YELLOW,
                "Listening",
                color::TEXT_DIMMED_C32,
            );
        }

        // MIDI Clock
        if !content_state.midi_clock_enabled {
            let device_text = if content_state.midi_clock_device_name.is_empty() {
                "Select..."
            } else {
                &content_state.midi_clock_device_name
            };
            ui.transport.set_clk_state(
                false,
                device_text,
                color::STATUS_DOT_INACTIVE,
                "Off",
                color::TEXT_DIMMED_C32,
            );
        } else if content_state.midi_clock_receiving {
            let device_text = if content_state.midi_clock_device_name.is_empty() {
                "MIDI"
            } else {
                &content_state.midi_clock_device_name
            };
            let position: &str = if content_state.midi_clock_position_display.is_empty() {
                "Receiving"
            } else {
                &content_state.midi_clock_position_display
            };
            ui.transport.set_clk_state(
                true,
                device_text,
                color::STATUS_DOT_GREEN,
                position,
                color::TEXT_WHITE_C32,
            );
        } else {
            let device_text = if content_state.midi_clock_device_name.is_empty() {
                "MIDI"
            } else {
                &content_state.midi_clock_device_name
            };
            ui.transport.set_clk_state(
                true,
                device_text,
                color::STATUS_DOT_YELLOW,
                "Waiting",
                color::TEXT_DIMMED_C32,
            );
        }

        // OSC Sync output — show AbletonOSC transport or legacy M4L sender state.
        {
            use manifold_core::types::OscSyncMode;
            let sync_enabled = match content_state.osc_sync_mode {
                OscSyncMode::AbletonOsc => content_state.ableton_transport_enabled,
                OscSyncMode::M4L => content_state.osc_sender_enabled,
            };
            if !sync_enabled {
                ui.transport.set_sync_state(
                    false,
                    color::STATUS_DOT_INACTIVE,
                    "Off",
                    color::TEXT_DIMMED_C32,
                );
            } else if content_state.osc_sync_mode == OscSyncMode::AbletonOsc {
                // Closed-loop sync health (ABLETON_TRANSPORT_SYNC_DESIGN
                // D9/D10): amber while a command awaits its ack or while
                // running position on OSC alone (MIDI clock absent), red
                // after a command exhausted its retries.
                use manifold_playback::transport_sync::TransportSyncStatus;
                let (dot, text, text_color) = match content_state.ableton_sync_status {
                    TransportSyncStatus::Locked => {
                        (color::STATUS_DOT_GREEN, "ABL", color::TEXT_WHITE_C32)
                    }
                    TransportSyncStatus::Confirming => {
                        (color::STATUS_DOT_YELLOW, "ABL…", color::TEXT_WHITE_C32)
                    }
                    TransportSyncStatus::DegradedOscOnly => {
                        (color::STATUS_DOT_YELLOW, "ABL no CLK", color::TEXT_WHITE_C32)
                    }
                    TransportSyncStatus::Warning => {
                        (color::STATUS_BAD, "ABL desync", color::TEXT_WHITE_C32)
                    }
                };
                ui.transport.set_sync_state(true, dot, text, text_color);
            } else {
                ui.transport.set_sync_state(
                    true,
                    color::STATUS_DOT_GREEN,
                    "M4L",
                    color::TEXT_WHITE_C32,
                );
            }
        }

        // Record state — disabled when OSC is clock authority (Unity invariant)
        let rec_allowed = auth != manifold_core::types::ClockAuthority::Osc;
        ui.transport
            .set_record_state(content_state.is_recording && rec_allowed, rec_allowed);

        // BPM reset: enabled only when a recorded tempo lane exists (tempo
        // automation from a recording session). Audio-import-detected BPM is
        // not a "recorded" value — importing audio just sets the project BPM.
        let can_reset = !project.recording_provenance.recorded_tempo_lane.is_empty();
        ui.transport.set_bpm_reset_active(can_reset);

        // BPM clear: enabled when tempo map has >1 point
        let can_clear = project.tempo_map.point_count() > 1;
        ui.transport.set_bpm_clear_active(can_clear);

        // Automation globals (P4, docs/AUTOMATION_LANES_DESIGN.md §4/§5/§7):
        // ARM mirrors the runtime arm flag; BACK lights red exactly when any
        // lane override latch is active (Live's Back to Arrangement).
        ui.transport.set_automation_state(
            content_state.automation_armed,
            !content_state.automation_latched_params.is_empty(),
        );
        // LANES button (view-only toggle, no content-thread state behind it).
        ui.transport
            .set_automation_mode_visible(selection.automation_mode_visible);

        // Save dirty state is shown by the "•" in the window/header project name
        // (set above); the transport SAVE button moved to the File menu. HDR /
        // Percussion / render config moved to the Settings popup (fed below).

        // Export range markers on viewport
        ui.viewport.set_export_range(
            project.timeline.export_in_beat,
            project.timeline.export_out_beat,
            project.timeline.export_range_enabled,
        );

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
        ui.header.set_project_name(&header_name);
        let ppb = ui.viewport.pixels_per_beat();
        ui.header.set_zoom_label(&format!("{:.0} px/beat", ppb));

        // Footer — quantize mode + FPS. (Resolution / render scale / tonemap
        // moved to the Settings popup; the footer keeps the live FPS readout.)
        ui.footer
            .set_quantize_text(project.settings.quantize_mode.display_name());
        ui.footer
            .set_fps_text(&format!("{:.0} FPS", project.settings.frame_rate));

        // Show preset label if dimensions match, otherwise show "WxH" (Unity: UpdateFooterResolutionText)
        let (preset_w, preset_h) = project.settings.resolution_preset.dimensions();
        let res_label = if preset_w == project.settings.output_width
            && preset_h == project.settings.output_height
        {
            project
                .settings
                .resolution_preset
                .display_name()
                .to_string()
        } else {
            format!(
                "{}x{}",
                project.settings.output_width, project.settings.output_height
            )
        };

        // Settings popup hosts the render config — feed the same state so its
        // segmented controls highlight the active option.
        ui.settings_popup.set_resolution_text(&res_label);
        ui.settings_popup
            .set_render_scale(project.settings.render_scale);
        ui.settings_popup
            .set_tonemap_curve(crate::ui_translate::tonemap_curve_to_ui(
                project.settings.tonemap_curve,
            ));
        ui.settings_popup.set_hdr(project.settings.export_hdr);
    }

    // Footer stats
    {
        let layers = project.timeline.layers.len();
        let clips: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let info = format!("Layers: {} | Clips: {}", layers, clips);
        ui.footer.set_selection_info(&info);
    }

    // Playhead + playing state
    let playhead_beat = content_state.current_beat.as_f32();
    ui.viewport.set_playhead(Beats::from_f32(playhead_beat));
    ui.viewport.set_playing(content_state.is_playing);

    // Selection → viewport (version-gated to avoid per-frame Vec allocation).
    // The panel's `selected_clip_ids` is a pure render cache fed from the enum
    // here — never an independent authority.
    ui.viewport.sync_selection(
        selection.selection_version,
        || selection.get_selected_clip_ids(),
        || selection.selected_marker_ids.iter().cloned().collect(),
    );
    if let Some(beat) = selection.insert_cursor_beat {
        ui.viewport.set_insert_cursor(beat);
    }

    // Region → viewport (sync from UIState so clearing via set_insert_cursor
    // propagates). Only a `TimeRange` selection has a region; a `Clips`
    // selection pushes `None`, so no band draws behind a clip selection (D1).
    if let Some(r) = selection.current_region() {
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        let (start_layer, end_layer) = r.layer_index_range(&ui_layers).unwrap_or((0, 0));
        ui.viewport
            .set_selection_region(Some(manifold_ui::panels::viewport::SelectionRegion {
                start_beat: r.start_beat,
                end_beat: r.end_beat,
                start_layer,
                end_layer,
            }));
    } else {
        ui.viewport.set_selection_region(None);
    }

    // Layer highlighting via UIState.is_layer_active (unified check across 4 paths):
    // explicit layer selection, clip selection, insert cursor, region.
    {
        let active_flags: Vec<bool> = project
            .timeline
            .layers
            .iter()
            .map(|l| selection.is_layer_active(&l.layer_id))
            .collect();
        ui.layer_headers.set_active_layers(&active_flags);
    }
    // Also set single active_layer for backward compat (inspector routing)
    let active_layer_id = active_layer
        .and_then(|i| project.timeline.layers.get(i))
        .map(|l| l.layer_id.clone());
    ui.layer_headers.set_active_layer(active_layer_id);
    // §19 timeline echo: the focused lane lifts in the viewport body too (track
    // index == layer index, the `tracks` vec is built 1:1 from project layers).
    ui.viewport.set_active_track_index(active_layer);
    {
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            ui.layer_headers.set_mute_state(tree, i, layer.is_muted);
            ui.layer_headers.set_solo_state(tree, i, layer.is_solo);
            ui.layer_headers.set_led_state(tree, i, layer.blit_to_led);
            ui.layer_headers
                .set_blend_mode_text(tree, i, layer.default_blend_mode.display_name());

            // MIDI note/channel/device labels + trigger-mode toggle
            use manifold_core::types::MidiTriggerMode;
            let all_notes = matches!(layer.midi_trigger_mode, MidiTriggerMode::AllNotes);
            let note_text = if all_notes {
                "\u{2014}".into()
            } else {
                manifold_core::midi::note_number_to_name(layer.midi_note)
            };
            ui.layer_headers.set_midi_note_text(tree, i, &note_text);

            let ch_text = if layer.midi_channel >= 0 {
                format!("{}", layer.midi_channel + 1)
            } else {
                "All".into()
            };
            ui.layer_headers.set_midi_channel_text(tree, i, &ch_text);

            let dev_text = match layer.midi_device.as_deref() {
                None | Some("") => "All",
                Some(name) => name,
            };
            ui.layer_headers.set_midi_device_text(tree, i, dev_text);

            let mode_text = if all_notes { "All" } else { "Note" };
            ui.layer_headers.set_midi_mode_text(tree, i, mode_text);

            // Layer info text (clip count)
            let clip_count = layer.clips.len();
            let info = if clip_count == 1 {
                "1 clip".into()
            } else {
                format!("{} clips", clip_count)
            };
            ui.layer_headers.set_info_text(tree, i, &info);
        }
    }

    // Macro slider values + labels/mapping counts for context menus
    let macro_vals: Vec<f32> = project
        .settings
        .macro_bank
        .slots
        .iter()
        .map(|s| s.value)
        .collect();
    // Display labels include [ABL] suffix — used for slider display only.
    // Raw slot.label is stored separately in ui.macro_labels for dropdown menus.
    let macro_display_labels: Vec<String> = project
        .settings
        .macro_bank
        .slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let base = if slot.label.is_empty() {
                format!("M{}", i + 1)
            } else {
                slot.label.clone()
            };
            if let Some(mapping) = &slot.ableton_mapping {
                use manifold_core::ableton_mapping::AbletonMappingStatus;
                let suffix = match mapping.status {
                    AbletonMappingStatus::Active => "[ABL]",
                    AbletonMappingStatus::Dormant => "[ABL-]",
                    AbletonMappingStatus::Ambiguous => "[ABL?]",
                };
                format!("{base} {suffix}")
            } else {
                base
            }
        })
        .collect();
    // Set Ableton display data + trim ranges before sync so build can use them.
    let macro_abl_displays: Vec<Option<AbletonMappingDisplay>> = project
        .settings
        .macro_bank
        .slots
        .iter()
        .map(|slot| {
            slot.ableton_mapping
                .as_ref()
                .map(|m| AbletonMappingDisplay {
                    macro_name: m.address.macro_name.clone(),
                    track_name: m.address.track_name.clone(),
                    device_name: m.address.device_name.clone(),
                    status: crate::ui_translate::ableton_mapping_status_to_ui(m.status),
                    inverted: m.inverted,
                })
        })
        .collect();
    ui.inspector
        .macros_panel_mut()
        .set_ableton_displays(&macro_abl_displays);
    let macro_abl_ranges: Vec<Option<(f32, f32)>> = project
        .settings
        .macro_bank
        .slots
        .iter()
        .map(|slot| {
            slot.ableton_mapping
                .as_ref()
                .map(|m| (m.range_min, m.range_max))
        })
        .collect();
    ui.inspector
        .macros_panel_mut()
        .set_ableton_ranges(&macro_abl_ranges);
    ui.inspector
        .macros_panel_mut()
        .sync_values(tree, &macro_vals, &macro_display_labels);
    for (i, slot) in project.settings.macro_bank.slots.iter().enumerate() {
        if i < manifold_core::MACRO_COUNT {
            ui.macro_labels[i].clone_from(&slot.label);
            ui.macro_mapping_descs[i] = slot
                .mappings
                .iter()
                .map(|m| describe_macro_mapping(&m.target, project))
                .collect();
            ui.macro_ableton_mapped[i] = slot.ableton_mapping.is_some();
        }
    }

    // Sync active layer opacity to inspector chrome
    if let Some(idx) = active_layer {
        {
            if let Some(layer) = project.timeline.layers.get(idx) {
                ui.inspector
                    .layer_chrome_mut()
                    .sync_opacity(tree, layer.opacity);
                ui.inspector.layer_chrome_mut().sync_name(tree, &layer.name);
            }
            // Master opacity + LED brightness
            ui.inspector
                .master_chrome_mut()
                .sync_opacity(tree, project.settings.master_opacity);
            ui.inspector
                .master_chrome_mut()
                .sync_led_brightness(tree, project.settings.led_brightness);
            ui.inspector
                .master_chrome_mut()
                .sync_led_enabled(tree, content_state.led_enabled);

            // LED exit path label + cached effect names for dropdown
            let exit_label = super::led_exit_path_label(
                project.settings.led_exit_index,
                &project.settings.master_effects,
            );
            ui.inspector
                .master_chrome_mut()
                .sync_exit_path(tree, &exit_label);
        }
    }

    // Cache master effect names for the LED exit path dropdown
    {
        use manifold_core::preset_type_registry;
        let names: Vec<String> = project
            .settings
            .master_effects
            .iter()
            .map(|fx| preset_type_registry::display_name(fx.effect_type()).to_string())
            .collect();
        ui.master_effect_names = names;
    }

    // Sync clip chrome VALUES from primary selected clip.
    // Mode (has_clip, is_video, is_gen, is_looping) is set in sync_inspector_data
    // BEFORE build so the tree layout is correct. Here we only sync text/values
    // into the already-built nodes.
    if let Some(clip_id) = &selection.primary_selected_clip_id {
        let clip = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *clip_id);
        if let Some(clip) = clip {
            let is_video = !clip.video_clip_id.is_empty();
            let is_gen = clip.generator_type != PresetTypeId::NONE;
            let chrome = ui.inspector.clip_chrome_mut();
            if is_video {
                let name = clip.video_clip_id.clone();
                chrome.sync_name(tree, &name);
                chrome.sync_source_name(tree, &clip.video_clip_id);
                chrome.sync_loop_enabled(tree, clip.is_looping);
                chrome.sync_loop_duration(tree, clip.loop_duration_beats);
                if clip.recorded_bpm > 0.0 {
                    chrome.sync_bpm(tree, &format!("{:.1}", clip.recorded_bpm));
                } else {
                    chrome.sync_bpm(tree, "Auto");
                }
                chrome.set_loop_range(clip.duration_beats.max(Beats(1.0)));
            } else if clip.is_audio() {
                let file_name = std::path::Path::new(&clip.audio_file_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Audio".to_string());
                chrome.sync_name(tree, &file_name);
                chrome.sync_source_name(tree, &file_name);
                // Warp on ⇔ a recorded BPM is set; off (0) plays at native speed.
                chrome.sync_warp_enabled(tree, clip.recorded_bpm > 0.0);
                // Clip BPM drives warp; "Auto" (0) means play at native speed.
                if clip.recorded_bpm > 0.0 {
                    chrome.sync_bpm(tree, &format!("{:.1}", clip.recorded_bpm));
                } else {
                    chrome.sync_bpm(tree, "Auto");
                }
                // Detection status + progress (what the pipeline is doing).
                let progress = if content_state.percussion_progress < 0.0 {
                    0.0
                } else {
                    content_state.percussion_progress
                };
                chrome.sync_detect_status(
                    tree,
                    &content_state.percussion_status_message,
                    progress,
                    content_state.percussion_show_progress,
                );
            } else if is_gen {
                chrome.sync_name(
                    tree,
                    manifold_core::preset_type_registry::display_name(&clip.generator_type),
                );
                chrome.sync_gen_type(
                    tree,
                    manifold_core::preset_type_registry::display_name(&clip.generator_type),
                );
            }
        }
    }

    // Sync effect card values (master, layer, clip)
    sync_card_values(ui, project, active_layer);

    // Sync Scene Setup row values (same per-frame value plane, dock rows)
    sync_scene_row_values(ui, project);
}

/// Per-frame VALUE sync for the Scene Setup dock's rows — the scene-row
/// sibling of [`sync_card_values`]: push each built row's CURRENT value from
/// `project` (the layer's generator graph def, instance override or bundled
/// default) onto the already-built panel, so rows track OSC / command /
/// other-window writes between structural syncs instead of freezing. Driven
/// (wire-fed) rows update through the value-label handle the driven branch
/// now keeps; non-driven rows update their card slider. Same drag safety as
/// `sync_card_values`: the actively-dragged field is restored into
/// `local_project` upstream of every call, so this writes the user's own
/// value straight back. No-op while the panel is closed or not Live.
pub fn sync_scene_row_values(ui: &mut UIRoot, project: &Project) {
    if !ui.scene_setup_panel.is_open() {
        return;
    }
    let Some(layer_id) = ui.scene_setup_panel.live_layer_id() else {
        return;
    };
    let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
        return;
    };
    let gen_inst = layer.gen_params();

    // The unified properties card's per-frame value push — real exposed
    // params, resolved the SAME way `sync_card_values` resolves the main
    // generator inspector card's values (`ui_translate::param_slots_to_ui`),
    // just against the SCENE PANEL's own bound layer (`live_layer_id`)
    // rather than the app's `active_layer` (a scene row always lives on the
    // layer its panel is docked to, which can differ from the app's active
    // layer — BUG-292).
    if let Some(gp) = gen_inst {
        let slots = crate::ui_translate::param_slots_to_ui(&gp.params);
        ui.scene_setup_panel.sync_properties_values(&mut ui.tree, &slots);
    }
}

/// P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the REAL section
/// string(s) P1 stamped onto every param whose PRIMARY node is one of
/// `doc_ids` — read directly off `def`'s exposure metadata. Two stamping
/// code paths (creation-time commands vs the load-time migration) produce
/// DIFFERENT section strings for the same node kind (e.g. a scene_object's
/// own section is the bare handle at creation, "{handle} — Object" after
/// migration) — reading the real string is the only way to filter correctly
/// regardless of which path produced it. Dedups, preserves first-seen order.
///
/// BUG-291 (fixed): the original implementation attributed a param by
/// walking `meta.bindings` to each binding's TARGET node and checking that
/// against `doc_ids` — but a fan-out control (the glTF importer's D7 sun
/// macro: the sun's `pos_x/y/z` ALSO binds `envmap.sun_x/y/z` so one slider
/// drives both; similarly env intensity also drives `hdri_gain.gain`) adds
/// an EXTRA `BindingDef` under the SAME `id` targeting the OTHER node. Target-
/// walking misattributed those extra bindings to whichever item owned the
/// fanned-out-to node (World's `envmap` doc id matched the sun's `pos_x`
/// binding's target, so a "Sun" section leaked into World). Attributing by
/// the doc-id PREFIX of the param's OWN `id` instead is fan-out-proof: P1
/// stamps every exposed id as `{primary_node_doc_id}_{param}`
/// (`manifold_core::scene_exposure::stamp_scene_node_exposures_into`,
/// mirrored by the glTF importer's own hand-authored fan-out ids at
/// `gltf_import.rs`'s D7 block) — the prefix names the param's ONE true
/// owner regardless of how many nodes its value also happens to drive, so no
/// binding-target walk (and no node-doc-id cross-reference) is needed at
/// all.
fn sections_for_doc_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    doc_ids: &[u32],
) -> Vec<String> {
    let Some(def) = def else { return Vec::new() };
    let Some(meta) = def.preset_metadata.as_ref() else { return Vec::new() };
    if doc_ids.is_empty() {
        return Vec::new();
    }

    let mut sections: Vec<String> = Vec::new();
    for spec in &meta.params {
        let Some(prefix_doc_id) = spec.id.split('_').next().and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !doc_ids.contains(&prefix_doc_id) {
            continue;
        }
        let Some(section) = spec.section.clone() else {
            continue;
        };
        if !sections.contains(&section) {
            sections.push(section);
        }
    }
    sections
}

#[cfg(test)]
mod sections_for_doc_ids_tests {
    //! BUG-291: reproduces the exact glTF-importer fan-out shape
    //! (`gltf_import.rs`'s D7 sun-coherence block) that leaked a "Sun"
    //! section into World's item. `sections_for_doc_ids` is state_sync's
    //! own private fn — exercised directly (state-level, no pixels), per
    //! `docs/BUG_BACKLOG.md`'s prescribed fix shape.
    use super::*;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata, SkipModeDef,
        EFFECT_GRAPH_VERSION_WITH_METADATA,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_core::NodeId;

    /// World = envmap (doc id 1) [+ atmosphere, omitted — not needed to
    /// reproduce the leak]. Sun = its own light node (doc id 7). The sun's
    /// `pos_x` control fans out to `envmap.sun_x` (D7 "sun coherence") under
    /// the SAME `id` as its own `sun.pos_x` binding — exactly the shape that
    /// made the old target-walking implementation attribute the fanned-out
    /// binding to World (whose doc-id set contains the envmap node the
    /// fan-out targets).
    fn azalea_like_fixture() -> EffectGraphDef {
        let meta = PresetMetadata {
            id: PresetTypeId::new("gltf_import_fixture"),
            display_name: "glTF Import Fixture".to_string(),
            category: "Diagnostic".to_string(),
            osc_prefix: "gltf_import_fixture".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![
                ParamSpecDef {
                    id: "1_intensity".to_string(),
                    name: "Intensity".to_string(),
                    section: Some("Environment".to_string()),
                    ..Default::default()
                },
                ParamSpecDef {
                    id: "7_pos_x".to_string(),
                    name: "Position X".to_string(),
                    section: Some("Sun".to_string()),
                    ..Default::default()
                },
            ],
            bindings: vec![
                // envmap's own intensity binding.
                BindingDef {
                    id: "1_intensity".to_string(),
                    label: String::new(),
                    default_value: 1.0,
                    target: BindingTarget::Node { node_id: NodeId::new("envmap"), param: "intensity".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
                // The sun's own pos_x binding.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node { node_id: NodeId::new("sun"), param: "pos_x".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
                // D7 fan-out: the SAME id, a SECOND binding targeting the
                // envmap's sun-disc param — the leak vector.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node { node_id: NodeId::new("envmap"), param: "sun_x".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
            ],
            skip_mode: SkipModeDef::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn world_sections_exclude_the_fanned_out_sun_section() {
        let def = azalea_like_fixture();
        // World's doc-id set: just the envmap node (doc id 1).
        let sections = sections_for_doc_ids(Some(&def), &[1]);
        assert_eq!(
            sections,
            vec!["Environment".to_string()],
            "World must not pick up \"Sun\" via the sun's fanned-out envmap.sun_x binding"
        );
    }

    #[test]
    fn the_lights_own_item_still_includes_its_section() {
        let def = azalea_like_fixture();
        // Sun's doc-id set: just its own light node (doc id 7).
        let sections = sections_for_doc_ids(Some(&def), &[7]);
        assert_eq!(sections, vec!["Sun".to_string()]);
    }
}

/// Push per-frame card VALUES (slider fill + readout, enabled toggle, card
/// name) from `project` into the already-configured inspector cards of any
/// window's `ui` — master effects, active layer's effects, generator params.
/// Window-agnostic: `push_state` calls it for the main window every frame,
/// and the graph-editor window's present path calls it on its own
/// `ed.ui_root` with the same `local_project`/`active_layer`, so card sliders
/// track drivers / mappings / envelopes in both windows instead of freezing
/// between structural syncs. No drag guard needed here: the actively-dragged
/// field is restored into `local_project` upstream of every call
/// (`app_render.rs`'s snapshot-drain `drag.apply`), so this writes the user's
/// own value straight back — user-owned in both windows.
pub fn sync_card_values(ui: &mut UIRoot, project: &Project, active_layer: Option<usize>) {
    let tree = &mut ui.tree;
    // Master effects
    for (i, effect) in project.settings.master_effects.iter().enumerate() {
        if let Some(card) = ui.inspector.master_effect_mut(i) {
            card.sync_effect_name(
                tree,
                manifold_core::preset_type_registry::display_name(effect.effect_type()),
            );
            card.sync_enabled(tree, effect.enabled);
            card.sync_values(tree, &crate::ui_translate::param_slots_to_ui(&effect.params));
        }
    }

    // Layer effects
    if let Some(idx) = active_layer
        && let Some(layer) = project.timeline.layers.get(idx)
        && let Some(effects) = &layer.effects
    {
        for (i, effect) in effects.iter().enumerate() {
            if let Some(card) = ui.inspector.layer_effect_mut(i) {
                card.sync_effect_name(
                    tree,
                    manifold_core::preset_type_registry::display_name(effect.effect_type()),
                );
                card.sync_enabled(tree, effect.enabled);
                card.sync_values(tree, &crate::ui_translate::param_slots_to_ui(&effect.params));
            }
        }
    }

    // Generator params (stored on layer, not clip)
    if let Some(idx) = active_layer
        && let Some(layer) = project.timeline.layers.get(idx)
        && let Some(gp_state) = layer.gen_params()
        && let Some(gp) = ui.inspector.gen_params_mut()
    {
        gp.sync_gen_type_name(
            tree,
            manifold_core::preset_type_registry::display_name(gp_state.generator_type()),
        );
        gp.sync_values(tree, &crate::ui_translate::param_slots_to_ui(&gp_state.params));
    }
}

/// Sync structural project data (layers, tracks) into UI panels.
/// Call once at init and whenever the project structure changes.
/// Triggers a full UI rebuild afterward.
pub fn sync_project_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) {
    {
        // Rebuild CoordinateMapper Y-layout FIRST so layer headers and viewport share
        // the same Y offsets. Unity: LayerHeaderPanel reads from CoordinateMapper.
        // `_for_layout` (not the plain `layers_to_ui`) resolves
        // `automation_lane_count` from `selection.automation_mode_visible` — the
        // one flag that grows a track when lanes are visible
        // (`docs/AUTOMATION_LANES_DESIGN.md` §7).
        ui.viewport.rebuild_mapper_layout(&crate::ui_translate::layers_to_ui_for_layout(
            &project.timeline.layers,
            selection.automation_mode_visible,
            &selection.chosen_automation_params,
        ));

        // Layer data → LayerHeaderPanel. Y offset/height are NOT copied here —
        // `LayerInfo` no longer carries them; the header panel queries the
        // mapper directly at draw time (`docs/TIMELINE_LAYOUT_P0_SPEC.md` D1),
        // the exact same values the viewport uses for lanes.
        let layers: Vec<LayerInfo> = project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                LayerInfo {
                    name: layer.name.clone(),
                    layer_id: layer.layer_id.to_string(),
                    is_collapsed: layer.is_collapsed,
                    is_group: layer.is_group(),
                    is_generator: layer.layer_type == LayerType::Generator,
                    is_audio: layer.is_audio(),
                    is_muted: layer.is_muted
                        || layer.parent_layer_id.as_ref().is_some_and(|pid| {
                            project
                                .timeline
                                .layers
                                .iter()
                                .any(|l| l.layer_id == *pid && l.is_muted)
                        }),
                    is_solo: layer.is_solo
                        || layer.parent_layer_id.as_ref().is_some_and(|pid| {
                            project
                                .timeline
                                .layers
                                .iter()
                                .any(|l| l.layer_id == *pid && l.is_solo)
                        }),
                    analysis_only: layer.analysis_only,
                    is_led: layer.blit_to_led,
                    parent_layer_id: layer.parent_layer_id.as_ref().map(|id| id.to_string()),
                    blend_mode: format!("{:?}", layer.default_blend_mode),
                    generator_type: layer.gen_params().map(|g| {
                        manifold_core::preset_type_registry::display_name(g.generator_type())
                            .to_string()
                    }),
                    clip_count: layer.clips.len(),
                    video_folder_path: layer.video_folder_path.clone(),
                    source_clip_count: 0,
                    midi_note: layer.midi_note,
                    midi_channel: layer.midi_channel,
                    midi_device: layer.midi_device.clone(),
                    midi_all_notes: matches!(
                        layer.midi_trigger_mode,
                        manifold_core::types::MidiTriggerMode::AllNotes
                    ),
                    audio_gain_db: layer.audio_gain_db,
                    audio_send_name: project
                        .audio_setup
                        .send_for_layer(&layer.layer_id)
                        .map(|s| s.label.clone()),
                    is_selected: selection.is_layer_active(&layer.layer_id),
                    color: manifold_ui::node::Color32::from_f32(
                        layer.layer_color.r,
                        layer.layer_color.g,
                        layer.layer_color.b,
                        layer.layer_color.a,
                    ),
                }
            })
            .collect();
        let active_layer_id = active_layer
            .and_then(|i| project.timeline.layers.get(i))
            .map(|l| l.layer_id.clone());
        ui.layer_headers.set_active_layer(active_layer_id);
        ui.layer_headers.set_layers(layers);

        // Track data → TimelineViewportPanel
        // From Unity ViewportManager.BuildTrack (lines 548-663):
        // - is_muted includes parent group mute (children of muted groups are dimmed)
        // - is_group set correctly for group layers
        let tracks: Vec<TrackInfo> = project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                // Check if muted individually or by parent group
                let parent_muted = layer.parent_layer_id.as_ref().is_some_and(|pid| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .any(|l| l.layer_id == *pid && l.is_muted)
                });
                let is_muted = layer.is_muted || parent_muted;

                // Track height is owned solely by the CoordinateMapper
                // (rebuilt above, read back by the viewport). No copy here.

                // Child layer indices for collapsed group preview
                let child_layer_indices = if layer.is_group() {
                    let layer_id = &layer.layer_id;
                    project
                        .timeline
                        .layers
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.parent_layer_id.as_ref() == Some(layer_id))
                        .map(|(j, _)| j)
                        .collect()
                } else {
                    Vec::new()
                };

                TrackInfo {
                    is_muted,
                    is_group: layer.is_group(),
                    is_collapsed: layer.is_collapsed,
                    child_layer_indices,
                }
            })
            .collect();
        ui.viewport.set_tracks(tracks);
        ui.viewport.layer_ids = project
            .timeline
            .layers
            .iter()
            .map(|l| l.layer_id.clone())
            .collect();

        // (CoordinateMapper Y-layout already rebuilt above, before layer headers)

        // Clip data → TimelineViewportPanel
        let mut viewport_clips = Vec::new();
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            for (clip_idx, clip) in layer.clips.iter().enumerate() {
                let is_gen = layer.layer_type == LayerType::Generator;
                let name = clip_display_name(layer, clip, clip_idx + 1, &project.video_library);
                use manifold_ui::panels::viewport::ViewportClip;
                let clip_color = clip_base_color(layer, clip, 1.0);
                viewport_clips.push(ViewportClip {
                    clip_id: clip.id.clone(),
                    layer_index: i,
                    start_beat: clip.start_beat,
                    duration_beats: clip.duration_beats,
                    name: name.into(),
                    color: clip_color,
                    is_muted: clip.is_muted,
                    is_locked: false,
                    is_generator: is_gen,
                    is_audio: layer.is_audio(),
                    waveform: if layer.is_audio() {
                        ui.audio_waveforms.renderer(&clip.id)
                    } else {
                        None
                    },
                    in_point_seconds: clip.in_point.0 as f32,
                    waveform_breakpoints: audio_waveform_breakpoints(clip, project),
                });
            }
        }
        ui.viewport.set_clips(viewport_clips);

        // Automation lane data → viewport (P4, `docs/AUTOMATION_LANES_DESIGN.md`
        // §7). Gated on the same flag `layers_to_ui_for_layout` used above, so
        // the Y-layout and the lane list can never disagree about whether
        // lanes are showing this frame.
        let mut viewport_lanes = Vec::new();
        if selection.automation_mode_visible {
            use manifold_ui::panels::viewport::ViewportAutomationLane;
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                if layer.is_collapsed || layer.is_group() {
                    continue;
                }
                let chosen = selection.chosen_automation_params.get(&layer.layer_id);
                for lane in crate::ui_translate::layer_automation_lanes_to_ui(layer, chosen) {
                    viewport_lanes.push(ViewportAutomationLane { layer_index: i, lane });
                }
            }
        }
        ui.viewport.set_automation_lanes(viewport_lanes);

        // Timeline markers → viewport
        ui.viewport
            .set_markers(crate::ui_translate::markers_to_ui(&project.timeline.markers));
        ui.viewport
            .set_selected_marker_ids(selection.selected_marker_ids.iter().cloned().collect());

        // Beats per bar
        ui.viewport
            .set_beats_per_bar(project.settings.time_signature_numerator as u32);
    }
}

/// Display title for a timeline clip in the viewport: **type · instance ·
/// format**. The type is the generator subtype / "Video" / "Audio", the
/// instance is the clip's 1-based position on its layer (left to right), and
/// the format is the container + resolution (video) or container (audio).
/// Generators carry no file format. Shared by both clip-sync paths so they
/// label clips identically.
///
/// Examples: `Text 1` · `Video 2 · MOV 1080p` · `Audio 1 · WAV`.
fn clip_display_name(
    layer: &manifold_core::layer::Layer,
    clip: &manifold_core::clip::TimelineClip,
    instance: usize,
    video_library: &manifold_core::video::VideoLibrary,
) -> String {
    if layer.layer_type == LayerType::Generator {
        let ty = layer
            .gen_params()
            .map(|gp| {
                manifold_core::preset_type_registry::display_name(gp.generator_type()).to_string()
            })
            .unwrap_or_else(|| "Gen".to_string());
        format!("{ty} {instance}")
    } else if clip.is_audio() {
        match file_ext_upper(&clip.audio_file_path) {
            Some(ext) => format!("Audio {instance} \u{b7} {ext}"),
            None => format!("Audio {instance}"),
        }
    } else if !clip.video_clip_id.is_empty() {
        // Container + resolution from the source clip, when the library knows it.
        let fmt = video_library
            .find_clip_by_id(&clip.video_clip_id)
            .map(|vc| {
                let ext = file_ext_upper(&vc.file_name);
                let res = resolution_label(vc.resolution_width, vc.resolution_height);
                match (ext, res) {
                    (Some(e), Some(r)) => format!(" \u{b7} {e} {r}"),
                    (Some(e), None) => format!(" \u{b7} {e}"),
                    (None, Some(r)) => format!(" \u{b7} {r}"),
                    (None, None) => String::new(),
                }
            })
            .unwrap_or_default();
        format!("Video {instance}{fmt}")
    } else {
        format!("Clip {instance}")
    }
}

/// Uppercased file extension (`flowers.mov` → `MOV`), or `None` when the path
/// has no extension.
fn file_ext_upper(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
}

/// A short resolution tag from pixel dimensions — `1080p`, `4K`, etc. Falls
/// back to `W×H` for non-standard sizes, and `None` when dimensions are unset.
fn resolution_label(width: i32, height: i32) -> Option<String> {
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(match height {
        2160 => "4K".to_string(),
        1440 => "1440p".to_string(),
        1080 => "1080p".to_string(),
        720 => "720p".to_string(),
        480 => "480p".to_string(),
        _ => format!("{width}\u{00d7}{height}"),
    })
}

/// Effective base colour for a clip: its per-clip `color_override` if set,
/// otherwise the owning layer's colour. Alpha is supplied by the caller — the
/// two clip-sync paths use different clip alphas. Shared so both resolve the
/// override identically.
fn clip_base_color(
    layer: &manifold_core::layer::Layer,
    clip: &manifold_core::clip::TimelineClip,
    alpha: f32,
) -> manifold_ui::node::Color32 {
    let c = clip.color_override.unwrap_or(layer.layer_color);
    manifold_ui::node::Color32::from_f32(c.r, c.g, c.b, alpha)
}

/// Lightweight per-frame clip position sync.
/// Refreshes viewport.clips_by_layer from the live project model so that
/// drag mutations (clip move, trim) are visible in the bitmap renderer.
/// Does NOT touch tracks, bitmap renderers, or layer headers — only clip data.
/// The bitmap fingerprint will detect if positions actually changed and skip
/// repaint when nothing moved (cheap no-op outside of drag).
///
/// `automation_visible` also refreshes `viewport`'s automation lane geometry
/// from the live project when true (P4 Unit A automation point drag —
/// `InteractionOverlay::handle_automation_drag` mutates the project directly
/// each frame the same way clip move-drag does, and needs the SAME per-frame
/// resync path so a dragged dot's on-screen position updates live instead of
/// waiting for the next structural sync — `docs/AUTOMATION_LANES_DESIGN.md`
/// §7). Cheap: gated the same way the clip refresh already is (mouse-pressed
/// or structural change), and lane counts are tens, not hundreds.
pub fn sync_clip_positions(
    ui: &mut UIRoot,
    project: &Project,
    automation_visible: bool,
    chosen_automation_params: &std::collections::HashMap<
        manifold_core::LayerId,
        (manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId),
    >,
) {
    use manifold_ui::panels::viewport::ViewportClip;
    let mut viewport_clips = Vec::new();
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        let is_gen = layer.layer_type == LayerType::Generator;
        for (clip_idx, clip) in layer.clips.iter().enumerate() {
            let name = clip_display_name(layer, clip, clip_idx + 1, &project.video_library);
            let clip_color = clip_base_color(layer, clip, 0.86);
            viewport_clips.push(ViewportClip {
                clip_id: clip.id.clone(),
                layer_index: i,
                start_beat: clip.start_beat,
                duration_beats: clip.duration_beats,
                name: name.into(),
                color: clip_color,
                is_muted: clip.is_muted,
                is_locked: false,
                is_generator: is_gen,
                is_audio: layer.is_audio(),
                waveform: if layer.is_audio() {
                    ui.audio_waveforms.renderer(&clip.id)
                } else {
                    None
                },
                in_point_seconds: clip.in_point.0 as f32,
                waveform_breakpoints: audio_waveform_breakpoints(clip, project),
            });
        }
    }
    ui.viewport.set_clips(viewport_clips);

    if automation_visible {
        use manifold_ui::panels::viewport::ViewportAutomationLane;
        let mut viewport_lanes = Vec::new();
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            if layer.is_collapsed || layer.is_group() {
                continue;
            }
            let chosen = chosen_automation_params.get(&layer.layer_id);
            for lane in crate::ui_translate::layer_automation_lanes_to_ui(layer, chosen) {
                viewport_lanes.push(ViewportAutomationLane { layer_index: i, lane });
            }
        }
        ui.viewport.set_automation_lanes(viewport_lanes);
    }

    // Only sync markers when marker data has changed (avoids re-pushing on every
    // drag frame). Markers are few (dozens), so building the UI view to compare is cheap.
    let ui_markers = crate::ui_translate::markers_to_ui(&project.timeline.markers);
    if ui.viewport.markers_stale(&ui_markers) {
        ui.viewport.set_markers(ui_markers);
    }
}

/// Piecewise beat→file-seconds breakpoints for an audio clip's waveform,
/// mirroring exactly what `AudioLayerPlayback::update` computes for the
/// voice's expected source position (`crates/manifold-playback/src/
/// audio_layer_playback.rs` ~:251-257): `expected = (now - clip_start) *
/// warp_ratio + in_point`, where `now`/`clip_start` are transport seconds
/// from `engine.beat_to_timeline_time_immut`, i.e.
/// `TempoMapConverter::beat_to_seconds_immut` — the project's piecewise tempo
/// map integration, NOT a constant seconds-per-beat. Audio playback is
/// correct; the waveform painter draws a single linear window, so a varying
/// tempo map made the two disagree.
///
/// Reproducing that beat→seconds function pointwise at the clip's start beat,
/// every tempo-map point strictly inside the clip, and its end beat gives a
/// piecewise-*linear* (in beats, matching the pixel x-axis) mapping: constant
/// between tempo points, a new slope at each one. Each pair is `(x_frac,
/// file_secs)` — `x_frac` beat-linear in `[0, 1]` across the clip, `file_secs`
/// the source-file position playback would be at for that beat. A
/// constant-tempo clip (no tempo-map point strictly inside it) yields exactly
/// 2 breakpoints — the old single linear window, reproduced exactly since
/// `warp_ratio` (unaffected by the tempo map) is unchanged and the only
/// segment IS start→end.
///
/// Empty for non-audio clips or non-positive duration. Baked once here per
/// clip per structural sync (not per frame in the renderer) — plain
/// `Vec<(f32, f32)>` data, no core types, per `manifold-ui`'s layering rule
/// (depends on `manifold-foundation` only).
fn audio_waveform_breakpoints(
    clip: &manifold_core::clip::TimelineClip,
    project: &Project,
) -> Vec<(f32, f32)> {
    if !clip.is_audio() {
        return Vec::new();
    }
    let duration_beats = clip.duration_beats.as_f32();
    if duration_beats <= 0.0 {
        return Vec::new();
    }
    let start_beat = clip.start_beat;
    let end_beat = clip.start_beat + clip.duration_beats;
    let project_bpm = project.settings.bpm;
    let ratio = clip.warp_ratio(project_bpm.0);
    let in_point = clip.in_point.0 as f32;
    let start_secs =
        TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, start_beat, project_bpm).0;

    let file_secs_at = |beat: Beats| -> f32 {
        let secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, beat, project_bpm).0;
        ((secs - start_secs) as f32) * ratio + in_point
    };

    let mut breakpoints = Vec::with_capacity(2 + project.tempo_map.points().len());
    breakpoints.push((0.0, file_secs_at(start_beat)));
    for point in project.tempo_map.points() {
        if point.beat > start_beat && point.beat < end_beat {
            let x_frac = (point.beat - start_beat).as_f32() / duration_beats;
            breakpoints.push((x_frac, file_secs_at(point.beat)));
        }
    }
    breakpoints.push((1.0, file_secs_at(end_beat)));
    breakpoints
}

/// Sync inspector content for the active selection.
/// Called when the active layer changes or after structural mutations.
pub fn sync_inspector_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) {
    // Audio Setup modal — refresh its current device + send list while it's
    // open. Resolving the device through the directory once per sync (only while
    // the modal is up) gives each send row its real channel name, grouped or
    // not, instead of a bare index.
    if ui.audio_setup_panel.is_open() {
        use manifold_core::AudioSourceKind;
        use manifold_ui::panels::audio_setup_panel::AudioSendRow;
        let dir = manifold_audio::directory::system_directory();
        // Tap sources (system / app output) don't live in the input-device list,
        // so resolving them there would always read "missing". Only resolve a
        // hardware device; a tap's liveness is checked separately below.
        let is_tap = project.audio_setup.device.as_ref().is_some_and(|d| d.is_tap());
        let device = match &project.audio_setup.device {
            Some(d) if !d.is_tap() => dir.resolve(d.uid_opt(), Some(&d.name)),
            Some(_) => None, // a tap — no DeviceInfo
            None => dir.list_input_devices().into_iter().find(|d| d.is_default),
        };
        let sends = project
            .audio_setup
            .sends
            .iter()
            .map(|s| {
                // Read-only source view: the full routing lines (capture device +
                // each feeding layer) for the Inputs section. Routing is edited
                // elsewhere — layers from the layer header, channels from the
                // channel control (the row-level "Cap" chip and its click-to-reveal
                // dropdown are gone; this is the one place the detail lives now).
                let ch_label = channel_label(device.as_ref(), is_tap, &s.channels);
                let cap = s.has_capture();
                let layer_name = |lid: &manifold_core::LayerId| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| &l.layer_id == lid)
                        .map(|l| l.name.clone())
                        .unwrap_or_else(|| {
                            manifold_ui::panels::audio_setup_panel::MISSING_LAYER_LABEL.to_string()
                        })
                };
                // Full routing lines for the read-only Inputs section.
                let mut routings: Vec<String> = Vec::new();
                if cap {
                    routings.push(format!("Capture \u{2022} {ch_label}"));
                }
                for lid in s.layers() {
                    routings.push(format!("Layer \u{2022} {}", layer_name(lid)));
                }
                // Consumers section: named audio mods (param gate/continuous
                // cards) plus enabled layer-owned `LayerClipTrigger` configs
                // (P3, D2 — the matrix's per-band route walk is gone; clip
                // triggers are authored on the layer only). Both are purely
                // navigational rows (D3): click selects the owning layer.
                let clip_triggers = project.clip_trigger_consumers(&s.id);
                let has_clip_triggers = !clip_triggers.is_empty();
                let consumers: Vec<manifold_ui::panels::audio_setup_panel::SendConsumerRow> =
                    project
                        .audio_mod_consumers(&s.id)
                        .into_iter()
                        .chain(clip_triggers)
                        .map(|(layer_id, label)| {
                            manifold_ui::panels::audio_setup_panel::SendConsumerRow { label, layer_id }
                        })
                        .collect();
                // Inputs section: audio layers feeding this send (id + name).
                let feeding_layers: Vec<(manifold_core::LayerId, String)> =
                    s.layers().iter().map(|lid| (lid.clone(), layer_name(lid))).collect();

                AudioSendRow {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    channel_label: ch_label,
                    channels: s.channels.clone(),
                    gain_db: s.gain_db,
                    floor_db: s.floor_db,
                    driven_count: project.audio_send_usage_count(&s.id),
                    routings,
                    has_clip_triggers,
                    feeding_layers,
                    consumers,
                }
            })
            .collect();

        // Surface a reliability warning: a chosen source that can't capture right
        // now — a device that won't resolve / reads offline, a tap on an OS that
        // can't tap, an app that isn't running — else a blocked mic permission.
        let status_warning = match &project.audio_setup.device {
            Some(d) => match d.kind {
                AudioSourceKind::InputDevice => match &device {
                    None => Some(format!("\u{26A0} \"{}\" is offline or unplugged", d.name)),
                    Some(info) if !info.is_alive => {
                        Some(format!("\u{26A0} \"{}\" is offline", info.name))
                    }
                    _ => None,
                },
                AudioSourceKind::SystemAudio => (!dir.tap_capabilities().system_audio)
                    .then(|| "\u{26A0} System audio capture needs macOS 14.4+".to_string()),
                AudioSourceKind::App => dir
                    .resolve_app(d.uid_opt().unwrap_or(""))
                    .is_none()
                    .then(|| format!("\u{26A0} \"{}\" isn't running", d.name)),
            },
            None => None,
        }
        .or_else(|| {
            (!manifold_audio::permission::status().is_usable())
                .then(|| "\u{26A0} Microphone access blocked — check System Settings".to_string())
        });

        ui.audio_setup_panel.configure(
            project
                .audio_setup
                .device
                .as_ref()
                .map(crate::ui_translate::audio_device_ref_to_ui),
            sends,
            status_warning,
        );

    }

    // ── Scene Setup panel (SCENE_SETUP_PANEL_DESIGN.md) ──
    // Rebuilt from scratch every sync while the dock is open — no cached/
    // staged copy anywhere (D1: "no rotting, no staleness"). Selection
    // scoping mirrors the inspector-tab rung derivation just below (§1 VERIFY
    // marker, resolved): the selection's own layer, falling back to
    // `active_layer`.
    if ui.scene_setup_panel.is_open() {
        use manifold_renderer::node_graph::scene_vm::{SceneVm, is_param_driven, is_param_exposed};
        use manifold_ui::panels::scene_setup_panel::{
            AtmosphereRowVm, EnvironmentRowVm, ObjectMaterialVm, ObjectRowVm, RowAddr, RowValue,
            SceneSetupState, SceneSetupVm, TransformRowVm,
        };

        let sel_layer_idx = selection
            .selected_layer_id_for_clip
            .as_ref()
            .or(selection.primary_selected_layer_id.as_ref())
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .or(active_layer);
        let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));

        // P2 slice 2a: the scene panel's bound layer's FULL generator
        // `ParamSurface`, filled in below only in the `Live` arm — see
        // `ScenePanel::configure_params`'s doc comment.
        let mut full_params: Option<ParamSurface> = None;
        let state = match layer {
            None => SceneSetupState::NoSelection("Select a layer to set up its scene.".to_string()),
            Some(l) if l.layer_type != LayerType::Generator => SceneSetupState::NoSelection(
                "Select a generator layer to set up its scene.".to_string(),
            ),
            Some(l) => {
                let layer_id = l.layer_id.clone();
                let gen_type = l.generator_type().clone();
                if gen_type.is_none() {
                    SceneSetupState::NoGenerator { layer_id }
                } else {
                    let def = l
                        .generator_graph()
                        .cloned()
                        .or_else(|| manifold_renderer::node_graph::bundled_preset_def(&gen_type).cloned());
                    match def.as_ref().and_then(SceneVm::from_def) {
                        None => SceneSetupState::NoScene { layer_id },
                        Some(vm) => {
                            // Ranges transcribed from each primitive's own
                            // `ParamDef::range` (`bake_environment`'s
                            // intensity [0,4] / fill [0,2]; `atmosphere`'s
                            // fog_density [0,1] / height_falloff [0,2]).
                            // UX-P3a: `exposed` is a free read off the SAME
                            // `def` `SceneVm::from_def` just walked
                            // (`is_param_exposed` — a second independent
                            // O(nodes) pass, node doc ids unique
                            // document-wide) — every row built through `row`/
                            // `scoped_row` gets a correct lit state for free,
                            // not just the rows P3a actually wires a mod
                            // button onto.
                            // Bound-row value override: a row whose inner (node, param) is
                            // covered by a card/user binding LIVES in the
                            // binding's instance slot — the write path edits
                            // that slot, so the displayed value must read it
                            // too, or the panel shows the def's stale import
                            // default.
                            let hoisted_gen_inst = l.gen_params();
                            let display_value = |node_doc_id: u32, param_id: &str, fallback: f32| {
                                hoisted_gen_inst
                                    .and_then(|inst| {
                                        // Instance graph first; a TRACKING
                                        // instance (graph: None — fresh
                                        // imports) resolves via the same
                                        // effective def the VM was built on.
                                        let id = inst
                                            .binding_id_for_node_param(node_doc_id, param_id)
                                            .or_else(|| {
                                                manifold_core::effects::binding_id_for_node_param_in(
                                                    def.as_ref()?,
                                                    node_doc_id,
                                                    param_id,
                                                )
                                            })?;
                                        inst.params
                                            .contains(id.as_str())
                                            .then(|| inst.get_base_param(id.as_str()))
                                    })
                                    .unwrap_or(fallback)
                            };
                            // P3 (scene_vm slimming): `is_param_driven` is the
                            // sole source of every row's driven-state now —
                            // the per-struct `_driven` fields scene_vm used to
                            // transcribe are gone; this wraps the shared
                            // helper against the SAME `def` `display_value`
                            // already closes over.
                            let is_driven = |node_doc_id: u32, param_id: &str| {
                                def.as_ref().is_some_and(|d| is_param_driven(d, node_doc_id, param_id))
                            };
                            let row = |node_doc_id: u32,
                                       param_id: &str,
                                       value: f32,
                                       driven: bool,
                                       min: f32,
                                       max: f32| RowValue {
                                addr: RowAddr::root(node_doc_id, param_id),
                                value: display_value(node_doc_id, param_id, value),
                                min,
                                max,
                                driven,
                                exposed: def
                                    .as_ref()
                                    .is_some_and(|d| is_param_exposed(d, node_doc_id, param_id)),
                            };
                            // Scoped variant for a P2 Objects row living
                            // inside the object's own group (material/
                            // modifier params) — same shape, plus the
                            // `[group_node_id]` scope the graph command
                            // family's `.with_scope` takes.
                            let scoped_row = |scope_path: Vec<u32>,
                                              node_doc_id: u32,
                                              param_id: &str,
                                              value: f32,
                                              driven: bool,
                                              min: f32,
                                              max: f32| RowValue {
                                addr: RowAddr { scope_path, node_doc_id, param_id: param_id.to_string() },
                                value: display_value(node_doc_id, param_id, value),
                                min,
                                max,
                                driven,
                                exposed: def
                                    .as_ref()
                                    .is_some_and(|d| is_param_exposed(d, node_doc_id, param_id)),
                            };
                            // C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md):
                            // moved up from its original C-P1a position
                            // (right before the old Environment/Fog-only
                            // `environment`/`atmosphere` construction below)
                            // so `transform_row`/`material_row` — which build
                            // BEFORE that point in this match arm — can also
                            // wrap their rows in `ModulatedRow` via `mrow`.
                            // Same closure, same definition, just visible
                            // earlier in this block; the Environment/Fog call
                            // sites further down are unchanged.
                            use manifold_ui::panels::scene_setup_panel::ModulatedRow;
                            let gen_inst = l.gen_params();
                            // BUG-249: modulation facts resolve through the
                            // REAL exposed-param binding id, not the row's
                            // synthesized scene id (which the runtime never
                            // evaluates) — see `scene_row_modulation`.
                            let mrow = |node_doc_id: u32, param_key: &str, v: RowValue| ModulatedRow {
                                modulation: Box::new(scene_row_modulation(
                                    gen_inst,
                                    def.as_ref(),
                                    node_doc_id,
                                    param_key,
                                    automation_latched,
                                )),
                                value: v,
                            };
                            let transform_row = |t: &manifold_renderer::node_graph::scene_vm::TransformVm| {
                                // D12 fix: `t`'s own addresses already carry
                                // the correct `scope_path` (empty for a
                                // root/ungrouped atom, `[group_node_id]` for
                                // one living inside an object's group) — use
                                // it directly instead of the old `row()`
                                // (root-only) helper, which silently wrote
                                // to the wrong scope for any grouped
                                // object's transform.
                                let scope = t.pos_addr.0.scope_path.clone();
                                let row = |node_doc_id: u32, param_id: &str, value: f32, driven: bool, min: f32, max: f32| {
                                    scoped_row(scope.clone(), node_doc_id, param_id, value, driven, min, max)
                                };
                                // C-P1b: each cell is now a `ModulatedRow` —
                                // `mrow` synthesizes the SAME
                                // `scene.{node_doc_id}.{param_key}` id the
                                // panel's `build_object_card_row` uses to key
                                // its own id map (D2's "one definition both
                                // sides use"), independent of `scope_path`
                                // (node_doc_id alone is document-wide unique,
                                // so a grouped object's transform still
                                // resolves its modulation facts correctly).
                                Box::new(TransformRowVm {
                                    pos: (
                                        mrow(t.node_doc_id, "pos_x", row(t.node_doc_id, "pos_x", t.pos_value.0, t.pos_driven.0, -100.0, 100.0)),
                                        mrow(t.node_doc_id, "pos_y", row(t.node_doc_id, "pos_y", t.pos_value.1, t.pos_driven.1, -100.0, 100.0)),
                                        mrow(t.node_doc_id, "pos_z", row(t.node_doc_id, "pos_z", t.pos_value.2, t.pos_driven.2, -100.0, 100.0)),
                                    ),
                                    rot: (
                                        mrow(
                                            t.node_doc_id,
                                            "rot_x",
                                            row(
                                                t.node_doc_id,
                                                "rot_x",
                                                t.rot_value.0,
                                                t.rot_driven.0,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                        mrow(
                                            t.node_doc_id,
                                            "rot_y",
                                            row(
                                                t.node_doc_id,
                                                "rot_y",
                                                t.rot_value.1,
                                                t.rot_driven.1,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                        mrow(
                                            t.node_doc_id,
                                            "rot_z",
                                            row(
                                                t.node_doc_id,
                                                "rot_z",
                                                t.rot_value.2,
                                                t.rot_driven.2,
                                                -std::f32::consts::TAU,
                                                std::f32::consts::TAU,
                                            ),
                                        ),
                                    ),
                                    scale: (
                                        mrow(t.node_doc_id, "scale_x", row(t.node_doc_id, "scale_x", t.scale_value.0, t.scale_driven.0, 0.01, 10.0)),
                                        mrow(t.node_doc_id, "scale_y", row(t.node_doc_id, "scale_y", t.scale_value.1, t.scale_driven.1, 0.01, 10.0)),
                                        mrow(t.node_doc_id, "scale_z", row(t.node_doc_id, "scale_z", t.scale_value.2, t.scale_driven.2, 0.01, 10.0)),
                                    ),
                                })
                            };
                            let material_row =
                                |m: &manifold_renderer::node_graph::scene_vm::MaterialVm| match m
                                {
                                    manifold_renderer::node_graph::scene_vm::MaterialVm::Known(row_data) => {
                                        // D12 fix: `row_data.scope_path`
                                        // already carries the correct scope
                                        // (see `transform_row`'s identical
                                        // fix above) — no external
                                        // group_node_id needed, and it's
                                        // correct for an ungrouped object
                                        // too (empty scope).
                                        let scope = row_data.scope_path.clone();
                                        // C-P1b: `ModulatedRow`s, same
                                        // `mrow` synthesis as `transform_row`
                                        // above. Values/driven-state are P3
                                        // manifest reads keyed on the same
                                        // node id — the struct only carries
                                        // identity now.
                                        let color = (
                                            mrow(row_data.node_doc_id, "color_r", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_r",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_r"),
                                                0.0,
                                                1.0,
                                            )),
                                            mrow(row_data.node_doc_id, "color_g", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_g",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_g"),
                                                0.0,
                                                1.0,
                                            )),
                                            mrow(row_data.node_doc_id, "color_b", scoped_row(
                                                scope.clone(),
                                                row_data.node_doc_id,
                                                "color_b",
                                                0.8,
                                                is_driven(row_data.node_doc_id, "color_b"),
                                                0.0,
                                                1.0,
                                            )),
                                        );
                                        if row_data.is_pbr {
                                            ObjectMaterialVm::Pbr {
                                                color,
                                                metallic: mrow(row_data.node_doc_id, "metallic", scoped_row(
                                                    scope.clone(),
                                                    row_data.node_doc_id,
                                                    "metallic",
                                                    0.0,
                                                    is_driven(row_data.node_doc_id, "metallic"),
                                                    0.0,
                                                    1.0,
                                                )),
                                                roughness: mrow(row_data.node_doc_id, "roughness", scoped_row(
                                                    scope,
                                                    row_data.node_doc_id,
                                                    "roughness",
                                                    0.5,
                                                    is_driven(row_data.node_doc_id, "roughness"),
                                                    0.01,
                                                    1.0,
                                                )),
                                            }
                                        } else {
                                            ObjectMaterialVm::Other { color }
                                        }
                                    }
                                    manifold_renderer::node_graph::scene_vm::MaterialVm::None => {
                                        ObjectMaterialVm::None
                                    }
                                };
                            let objects: Vec<ObjectRowVm> = vm
                                .objects
                                .iter()
                                .map(|o| match o {
                                    manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(known) => {
                                        let manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow {
                                            index,
                                            object_node_id,
                                            group_node_id,
                                            name,
                                            visible_addr,
                                            visible_value,
                                            visible_driven,
                                            transform,
                                            material,
                                            modifier_chain,
                                            modifier_chain_parseable,
                                            ..
                                        } = known.as_ref();
                                        // P2 slice 2a: the real P1 section
                                        // string(s) covering this object —
                                        // its scene_object node, transform
                                        // node, material node, and every
                                        // modifier in its stack. Read
                                        // straight off the layer's exposure
                                        // metadata via doc-id cross-reference
                                        // (`sections_for_doc_ids`) — never
                                        // reconstructed from a naming
                                        // convention (creation-time and
                                        // load-migration stamping produce
                                        // different strings for the same
                                        // node kind).
                                        let mut object_doc_ids = vec![*object_node_id];
                                        if let Some(t) = transform {
                                            object_doc_ids.push(t.node_doc_id);
                                        }
                                        if let manifold_renderer::node_graph::scene_vm::MaterialVm::Known(m) =
                                            material
                                        {
                                            object_doc_ids.push(m.node_doc_id);
                                        }
                                        object_doc_ids.extend(modifier_chain.iter().map(|m| m.node_doc_id));
                                        let sections = sections_for_doc_ids(def.as_ref(), &object_doc_ids);
                                        ObjectRowVm::Known(Box::new(
                                            manifold_ui::panels::scene_setup_panel::ObjectKnownRow {
                                                index: *index,
                                                object_node_id: *object_node_id,
                                                group_node_id: *group_node_id,
                                                name: name.clone(),
                                                visible: scoped_row(
                                                    visible_addr.scope_path.clone(),
                                                    visible_addr.node_doc_id,
                                                    &visible_addr.param_id,
                                                    if *visible_value { 1.0 } else { 0.0 },
                                                    *visible_driven,
                                                    0.0,
                                                    1.0,
                                                ),
                                                transform: transform.as_ref().map(&transform_row),
                                                material: material_row(material),
                                                modifiers: modifier_chain
                                                    .iter()
                                                    .enumerate()
                                                    .map(|(i, m)| manifold_ui::panels::scene_setup_panel::ModifierKnownRow {
                                                        index: i,
                                                        node_doc_id: m.node_doc_id,
                                                        display_name: modifier_display_name(&m.type_id),
                                                    })
                                                    .collect(),
                                                modifiers_addable: *modifier_chain_parseable,
                                                sections,
                                            },
                                        ))
                                    }
                                    manifold_renderer::node_graph::scene_vm::SceneObjectVm::Custom { index } => {
                                        ObjectRowVm::Custom { index: *index }
                                    }
                                })
                                .collect();
                            // P3: Lights + Camera. Enum-label arrays
                            // transcribed from `node.light`'s own
                            // `LIGHT_MODES`/`SHADOW_SOFTNESS_LABELS`
                            // constants (`light.rs`) — this crate can't
                            // depend on them directly through the UI DTO
                            // boundary (`manifold-ui` doesn't depend on
                            // `manifold-renderer`), same convention as
                            // `EnvironmentRowVm::mode_is_hdri`.
                            const LIGHT_MODE_LABELS: &[&str] = &["Sun", "Point"];
                            const SHADOW_SOFTNESS_LABELS: &[&str] = &["Hard", "Soft", "VerySoft", "Contact"];
                            const CAST_SHADOWS_LABELS: &[&str] = &["Off", "On"];
                            // C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md):
                            // Light's Mode/Cast Shadows/Shadow Softness rows
                            // are now `ModulatedEnumRow` — same `mrow`
                            // promotion `transform_row`/`material_row` above
                            // already do for their own fields.
                            let enum_row = |node_doc_id: u32,
                                            param_id: &str,
                                            value: u32,
                                            driven: bool,
                                            labels: &'static [&'static str]| {
                                manifold_ui::panels::scene_setup_panel::ModulatedEnumRow {
                                    row: mrow(
                                        node_doc_id,
                                        param_id,
                                        row(node_doc_id, param_id, value as f32, driven, 0.0, (labels.len() - 1) as f32),
                                    ),
                                    labels: labels.to_vec(),
                                }
                            };
                            let lights: Vec<manifold_ui::panels::scene_setup_panel::LightRowVm> = vm
                                .lights
                                .iter()
                                .map(|l| match l {
                                    manifold_renderer::node_graph::scene_vm::SceneLightVm::Known(r) => {
                                        manifold_ui::panels::scene_setup_panel::LightRowVm::Known(Box::new(
                                            manifold_ui::panels::scene_setup_panel::LightKnownRow {
                                                index: r.index,
                                                node_doc_id: r.node_doc_id,
                                                name: r.name.clone(),
                                                mode: enum_row(r.node_doc_id, "mode", 0, is_driven(r.node_doc_id, "mode"), LIGHT_MODE_LABELS),
                                                color: (
                                                    mrow(r.node_doc_id, "color_r", row(r.node_doc_id, "color_r", 1.0, is_driven(r.node_doc_id, "color_r"), 0.0, 1.0)),
                                                    mrow(r.node_doc_id, "color_g", row(r.node_doc_id, "color_g", 1.0, is_driven(r.node_doc_id, "color_g"), 0.0, 1.0)),
                                                    mrow(r.node_doc_id, "color_b", row(r.node_doc_id, "color_b", 1.0, is_driven(r.node_doc_id, "color_b"), 0.0, 1.0)),
                                                ),
                                                intensity: mrow(r.node_doc_id, "intensity", row(
                                                    r.node_doc_id,
                                                    "intensity",
                                                    1.0,
                                                    is_driven(r.node_doc_id, "intensity"),
                                                    0.0,
                                                    10.0,
                                                )),
                                                pos: (
                                                    mrow(r.node_doc_id, "pos_x", row(r.node_doc_id, "pos_x", 0.0, is_driven(r.node_doc_id, "pos_x"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "pos_y", row(r.node_doc_id, "pos_y", 30.0, is_driven(r.node_doc_id, "pos_y"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "pos_z", row(r.node_doc_id, "pos_z", 0.0, is_driven(r.node_doc_id, "pos_z"), -100.0, 100.0)),
                                                ),
                                                aim: (
                                                    mrow(r.node_doc_id, "aim_x", row(r.node_doc_id, "aim_x", 0.0, is_driven(r.node_doc_id, "aim_x"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "aim_y", row(r.node_doc_id, "aim_y", 0.0, is_driven(r.node_doc_id, "aim_y"), -100.0, 100.0)),
                                                    mrow(r.node_doc_id, "aim_z", row(r.node_doc_id, "aim_z", 0.0, is_driven(r.node_doc_id, "aim_z"), -100.0, 100.0)),
                                                ),
                                                cast_shadows: enum_row(
                                                    r.node_doc_id,
                                                    "cast_shadows",
                                                    1,
                                                    is_driven(r.node_doc_id, "cast_shadows"),
                                                    CAST_SHADOWS_LABELS,
                                                ),
                                                shadow_softness: enum_row(
                                                    r.node_doc_id,
                                                    "shadow_softness",
                                                    1,
                                                    is_driven(r.node_doc_id, "shadow_softness"),
                                                    SHADOW_SOFTNESS_LABELS,
                                                ),
                                                light_size: mrow(r.node_doc_id, "light_size", row(
                                                    r.node_doc_id,
                                                    "light_size",
                                                    1.0,
                                                    is_driven(r.node_doc_id, "light_size"),
                                                    0.0,
                                                    20.0,
                                                )),
                                                // P2 slice 2a: see
                                                // `ObjectKnownRow::sections`'s
                                                // doc comment.
                                                sections: sections_for_doc_ids(def.as_ref(), &[r.node_doc_id]),
                                            },
                                        ))
                                    }
                                    manifold_renderer::node_graph::scene_vm::SceneLightVm::Custom { index } => {
                                        manifold_ui::panels::scene_setup_panel::LightRowVm::Custom { index: *index }
                                    }
                                })
                                .collect();
                            let lens_row = |l: &manifold_renderer::node_graph::scene_vm::LensRow| {
                                manifold_ui::panels::scene_setup_panel::LensRowVm {
                                    focus_distance: mrow(l.node_doc_id, "focus_distance", row(
                                        l.node_doc_id,
                                        "focus_distance",
                                        0.0,
                                        is_driven(l.node_doc_id, "focus_distance"),
                                        0.0,
                                        1000.0,
                                    )),
                                    f_stop: mrow(l.node_doc_id, "f_stop", row(l.node_doc_id, "f_stop", 1000.0, is_driven(l.node_doc_id, "f_stop"), 0.5, 1000.0)),
                                    shutter_angle: mrow(l.node_doc_id, "shutter_angle", row(
                                        l.node_doc_id,
                                        "shutter_angle",
                                        0.0,
                                        is_driven(l.node_doc_id, "shutter_angle"),
                                        0.0,
                                        360.0,
                                    )),
                                    exposure_ev: mrow(l.node_doc_id, "exposure_ev", row(
                                        l.node_doc_id,
                                        "exposure_ev",
                                        0.0,
                                        is_driven(l.node_doc_id, "exposure_ev"),
                                        -8.0,
                                        8.0,
                                    )),
                                }
                            };
                            let camera = match &vm.camera {
                                manifold_renderer::node_graph::scene_vm::CameraVm::Orbit(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Orbit(Box::new(
                                        manifold_ui::panels::scene_setup_panel::OrbitCameraRowVm {
                                            orbit: mrow(c.node_doc_id, "orbit", row(c.node_doc_id, "orbit", 0.7, is_driven(c.node_doc_id, "orbit"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            tilt: mrow(c.node_doc_id, "tilt", row(c.node_doc_id, "tilt", 0.3, is_driven(c.node_doc_id, "tilt"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            distance: mrow(c.node_doc_id, "distance", row(c.node_doc_id, "distance", 4.0, is_driven(c.node_doc_id, "distance"), 0.01, 100.0)),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::Free(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Free(Box::new(
                                        manifold_ui::panels::scene_setup_panel::FreeCameraRowVm {
                                            pos: (
                                                mrow(c.node_doc_id, "pos_x", row(c.node_doc_id, "pos_x", 0.0, is_driven(c.node_doc_id, "pos_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_y", row(c.node_doc_id, "pos_y", 0.0, is_driven(c.node_doc_id, "pos_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_z", row(c.node_doc_id, "pos_z", -3.0, is_driven(c.node_doc_id, "pos_z"), -1000.0, 1000.0)),
                                            ),
                                            yaw: mrow(c.node_doc_id, "yaw", row(c.node_doc_id, "yaw", 0.0, is_driven(c.node_doc_id, "yaw"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            pitch: mrow(c.node_doc_id, "pitch", row(c.node_doc_id, "pitch", 0.0, is_driven(c.node_doc_id, "pitch"), -1.5, 1.5)),
                                            roll: mrow(c.node_doc_id, "roll", row(c.node_doc_id, "roll", 0.0, is_driven(c.node_doc_id, "roll"), -std::f32::consts::TAU, std::f32::consts::TAU)),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::LookAt(c) => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::LookAt(Box::new(
                                        manifold_ui::panels::scene_setup_panel::LookAtCameraRowVm {
                                            pos: (
                                                mrow(c.node_doc_id, "pos_x", row(c.node_doc_id, "pos_x", 0.0, is_driven(c.node_doc_id, "pos_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_y", row(c.node_doc_id, "pos_y", 0.0, is_driven(c.node_doc_id, "pos_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "pos_z", row(c.node_doc_id, "pos_z", -3.0, is_driven(c.node_doc_id, "pos_z"), -1000.0, 1000.0)),
                                            ),
                                            target: (
                                                mrow(c.node_doc_id, "target_x", row(c.node_doc_id, "target_x", 0.0, is_driven(c.node_doc_id, "target_x"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "target_y", row(c.node_doc_id, "target_y", 0.0, is_driven(c.node_doc_id, "target_y"), -1000.0, 1000.0)),
                                                mrow(c.node_doc_id, "target_z", row(c.node_doc_id, "target_z", 0.0, is_driven(c.node_doc_id, "target_z"), -1000.0, 1000.0)),
                                            ),
                                            fov_y: mrow(c.node_doc_id, "fov_y", row(c.node_doc_id, "fov_y", 0.9, is_driven(c.node_doc_id, "fov_y"), 0.05, 2.5)),
                                            lens: c.lens.as_ref().map(lens_row),
                                        },
                                    ))
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::Custom { .. } => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::Custom
                                }
                                manifold_renderer::node_graph::scene_vm::CameraVm::None => {
                                    manifold_ui::panels::scene_setup_panel::CameraRowVm::None
                                }
                            };
                            // C-P1a (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md
                            // D3): the converted Environment/Fog rows also
                            // need their driver/envelope/audio-mod facts —
                            // this crate is the only side with a
                            // `PresetInstance` to query. `gen_inst`/`mrow`
                            // are defined once, earlier in this match arm
                            // (moved there C-P1b so `transform_row`/
                            // `material_row` can reuse them too) — reused
                            // here unchanged.
                            // P2 slice 2a: the real P1 section string(s)
                            // covering the camera atom (+ lens, if wired)
                            // and World (environment/atmosphere) — see
                            // `SceneSetupVm::camera_sections`/
                            // `world_sections`'s doc comments. Computed from
                            // `vm.camera`/`vm.environment`/`vm.atmosphere`
                            // BEFORE the consuming matches below (reads the
                            // VM's own case analysis, never re-derives graph
                            // topology).
                            let camera_sections = {
                                use manifold_renderer::node_graph::scene_vm::CameraVm;
                                let mut ids = match &vm.camera {
                                    CameraVm::Orbit(c) => vec![c.node_doc_id],
                                    CameraVm::Free(c) => vec![c.node_doc_id],
                                    CameraVm::LookAt(c) => vec![c.node_doc_id],
                                    CameraVm::Custom { .. } | CameraVm::None => Vec::new(),
                                };
                                let lens_id = match &vm.camera {
                                    CameraVm::Orbit(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::Free(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::LookAt(c) => c.lens.as_ref().map(|l| l.node_doc_id),
                                    CameraVm::Custom { .. } | CameraVm::None => None,
                                };
                                if let Some(id) = lens_id {
                                    ids.push(id);
                                }
                                sections_for_doc_ids(def.as_ref(), &ids)
                            };
                            let world_sections = {
                                use manifold_renderer::node_graph::scene_vm::{AtmosphereVm, EnvironmentVm};
                                let mut ids = Vec::new();
                                match &vm.environment {
                                    EnvironmentVm::Importer(e) => ids.push(e.bake_node_id),
                                    EnvironmentVm::Bare(e) => ids.push(e.node_doc_id),
                                    EnvironmentVm::Custom { .. } | EnvironmentVm::None => {}
                                }
                                if let AtmosphereVm::Wired(a) = &vm.atmosphere {
                                    ids.push(a.node_doc_id);
                                }
                                sections_for_doc_ids(def.as_ref(), &ids)
                            };
                            let environment = match vm.environment {
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Importer(e) => {
                                    EnvironmentRowVm::Importer {
                                        // BUG-260's dead-chip case (design
                                        // doc §3b.9): reads through
                                        // `display_value` like every other
                                        // row so a bound "selector" still
                                        // shows correctly; still not a
                                        // clickable RowValue — unchanged
                                        // pre-existing behavior, not this
                                        // lane's scope.
                                        mode_is_hdri: display_value(e.switch_node_id, "selector", 0.0) != 0.0,
                                        intensity: mrow(
                                            e.bake_node_id,
                                            "intensity",
                                            row(
                                                e.bake_node_id,
                                                "intensity",
                                                1.0,
                                                is_driven(e.bake_node_id, "intensity"),
                                                0.0,
                                                4.0,
                                            ),
                                        ),
                                        fill: mrow(
                                            e.bake_node_id,
                                            "fill",
                                            row(
                                                e.bake_node_id,
                                                "fill",
                                                0.0,
                                                is_driven(e.bake_node_id, "fill"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                        hdri_file: e.hdri_file_value,
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Bare(e) => {
                                    EnvironmentRowVm::Bare {
                                        intensity: mrow(
                                            e.node_doc_id,
                                            "intensity",
                                            row(
                                                e.node_doc_id,
                                                "intensity",
                                                1.0,
                                                is_driven(e.node_doc_id, "intensity"),
                                                0.0,
                                                4.0,
                                            ),
                                        ),
                                        fill: mrow(
                                            e.node_doc_id,
                                            "fill",
                                            row(
                                                e.node_doc_id,
                                                "fill",
                                                0.0,
                                                is_driven(e.node_doc_id, "fill"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::Custom { .. } => {
                                    EnvironmentRowVm::Custom
                                }
                                manifold_renderer::node_graph::scene_vm::EnvironmentVm::None => {
                                    EnvironmentRowVm::None
                                }
                            };
                            let atmosphere = match vm.atmosphere {
                                manifold_renderer::node_graph::scene_vm::AtmosphereVm::Wired(a) => {
                                    AtmosphereRowVm::Wired {
                                        density: mrow(
                                            a.node_doc_id,
                                            // BUG-249: the GRAPH param key
                                            // ("fog_density"), not the panel's
                                            // curated row key ("density") —
                                            // `scene_row_modulation` resolves
                                            // the binding by the inner node's
                                            // real param name.
                                            "fog_density",
                                            row(
                                                a.node_doc_id,
                                                "fog_density",
                                                0.0,
                                                is_driven(a.node_doc_id, "fog_density"),
                                                0.0,
                                                1.0,
                                            ),
                                        ),
                                        height_falloff: mrow(
                                            a.node_doc_id,
                                            "height_falloff",
                                            row(
                                                a.node_doc_id,
                                                "height_falloff",
                                                0.0,
                                                is_driven(a.node_doc_id, "height_falloff"),
                                                0.0,
                                                2.0,
                                            ),
                                        ),
                                    }
                                }
                                manifold_renderer::node_graph::scene_vm::AtmosphereVm::None => {
                                    AtmosphereRowVm::None
                                }
                            };
                            let (audio_send_labels, audio_send_ids) = (
                                project.audio_setup.sends.iter().map(|s| s.label.clone()).collect(),
                                project.audio_setup.sends.iter().map(|s| s.id.clone()).collect(),
                            );
                            // P2 slice 2a: the layer's FULL generator
                            // `ParamSurface` — the SAME `gen_params_to_surface`
                            // the main inspector's generator card uses (see
                            // `ScenePanel::configure_params`'s doc comment for
                            // why THIS layer, never `active_layer`).
                            full_params = gen_inst.map(|gp| {
                                gen_params_to_surface(gp, layer_id.as_str(), None, automation_latched)
                            });
                            SceneSetupState::Live(Box::new(SceneSetupVm {
                                layer_id,
                                scene_name: l.name.clone(),
                                multiple_scenes: vm.multiple_scenes,
                                object_count: vm.header.object_count,
                                light_count: vm.header.light_count,
                                shadow_caster_count: vm.header.shadow_caster_count,
                                scene_root_node_id: vm.scene_root_node_id,
                                environment,
                                atmosphere,
                                audio_send_labels,
                                audio_send_ids,
                                objects,
                                lights,
                                camera,
                                camera_sections,
                                world_sections,
                            }))
                        }
                    }
                }
            }
        };
        ui.scene_setup_panel.configure(state);
        ui.scene_setup_panel.configure_params(full_params);
    }

    // ── Inspector tabs: the selection's ownership rungs (local→global) ──
    // The rung set is derived from the SELECTION's own layer (the clip's layer
    // or the selected layer), NOT `active_layer` — which now follows the active
    // tab (e.g. it points at the group when the Group rung is pinned). Deriving
    // from the stable selection keeps the full chain available no matter which
    // rung you're viewing. The active rung is the pin if one is set (a tab
    // click), else the selection-derived default.
    {
        use manifold_core::types::LayerType;
        use manifold_ui::InspectorTab;
        let has_clip = selection.primary_selected_clip_id.is_some();
        let sel_layer_idx = selection
            .selected_layer_id_for_clip
            .as_ref()
            .or(selection.primary_selected_layer_id.as_ref())
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .or(active_layer);
        let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));
        let layer_is_group = layer.is_some_and(|l| l.layer_type == LayerType::Group);
        let has_group_parent =
            sel_layer_idx.is_some_and(|i| project.timeline.find_group_parent(i).is_some());

        let mut tabs: Vec<InspectorTab> = Vec::new();
        if has_clip {
            tabs.push(InspectorTab::Clip);
        }
        if let Some(l) = layer {
            if l.layer_type == LayerType::Group {
                tabs.push(InspectorTab::Group);
            } else {
                tabs.push(InspectorTab::Layer);
                if has_group_parent {
                    tabs.push(InspectorTab::Group);
                }
            }
        }
        tabs.push(InspectorTab::Master);

        let active = selection
            .pinned_scope()
            .filter(|t| tabs.contains(t))
            .unwrap_or_else(|| {
                // Default to the LAYER scope on any selection — the layer (its
                // generator, effects, macros) is the persistent thing you tune,
                // so landing there is less jarring than the per-clip view. The
                // Clip tab is still one click away whenever a clip is selected.
                if layer_is_group {
                    InspectorTab::Group
                } else if layer.is_some() {
                    InspectorTab::Layer
                } else if has_clip {
                    InspectorTab::Clip
                } else {
                    InspectorTab::Master
                }
            });
        ui.inspector.configure_tabs(&tabs, active);
    }

    // Master effects → inspector (envelopes ride on each instance)
    let mut master_configs = effects_to_surfaces(
        &project.settings.master_effects,
        OscScope::Master,
        automation_latched,
    );
    attach_audio_sends(&mut master_configs, &project.audio_setup);
    ui.inspector.configure_master_effects(&master_configs);

    // Active layer effects + gen params → inspector
    if let Some(idx) = active_layer {
        if let Some(layer) = project.timeline.layers.get(idx) {
            // AUDIO TRIGGERS (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_
            // DESIGN.md) — the layer's own `clip_triggers`, structurally
            // configured alongside gen params/layer effects below. state_sync
            // stays the sole panel data boundary: the inspector never reads
            // `Project` directly. Row label is "{band} → {feature kind}"
            // (the design's own example, "Low → Kick").
            {
                use manifold_ui::panels::audio_trigger_section::{
                    AudioTriggerRowConfig, AudioTriggerSectionConfig,
                };
                let send_labels: Vec<String> =
                    project.audio_setup.sends.iter().map(|s| s.label.clone()).collect();
                let send_ids: Vec<manifold_core::AudioSendId> =
                    project.audio_setup.sends.iter().map(|s| s.id.clone()).collect();
                let rows: Vec<AudioTriggerRowConfig> = layer
                    .clip_triggers
                    .iter()
                    .map(|t| {
                        let feature = t.source.feature;
                        AudioTriggerRowConfig {
                            enabled: t.enabled,
                            label: format!("{} \u{2192} {}", feature.band.label(), feature.kind.label()),
                            kind_idx: feature.kind.index() as i32,
                            band_idx: feature.band.index() as i32,
                            sensitivity: t.shape.sensitivity,
                            send_id: Some(t.source.send_id.clone()),
                            one_shot_beats: t.one_shot_beats.0 as f32,
                        }
                    })
                    .collect();
                ui.inspector.audio_trigger_section_mut().configure(
                    Some(layer.layer_id.clone()),
                    &AudioTriggerSectionConfig { rows, send_labels, send_ids },
                );
            }

            // Layer effects — envelopes ride on each effect instance now.
            let lid = layer.layer_id.as_str();
            let mut layer_effects = layer
                .effects
                .as_ref()
                .map(|e| effects_to_surfaces(e, OscScope::Layer(lid), automation_latched))
                .unwrap_or_default();
            attach_audio_sends(&mut layer_effects, &project.audio_setup);
            ui.inspector
                .configure_layer_effects(&layer_effects, Some(&layer.layer_id));

            // Generator params — find clip's string_params for text fields.
            // Use selected clip if on this layer, otherwise first clip.
            let clip_string_params = selection
                .primary_selected_clip_id
                .as_ref()
                .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                .or_else(|| layer.clips.first())
                .and_then(|c| c.string_params.as_ref());
            let mut gen_config = layer
                .gen_params()
                .filter(|gp| *gp.generator_type() != PresetTypeId::NONE)
                .map(|gp| {
                    gen_params_to_surface(
                        gp,
                        lid,
                        clip_string_params,
                        automation_latched,
                    )
                });
            if let Some(c) = gen_config.as_mut() {
                attach_audio_sends(std::slice::from_mut(c), &project.audio_setup);
            }
            let layer_id = layer.layer_id.clone();
            ui.inspector
                .configure_gen_params(gen_config.as_ref(), Some(layer_id));
        } else {
            ui.inspector.configure_layer_effects(&[], None);
            ui.inspector.configure_gen_params(None, None);
            ui.inspector
                .audio_trigger_section_mut()
                .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
        }
    } else {
        ui.inspector.configure_layer_effects(&[], None);
        ui.inspector.configure_gen_params(None, None);
        ui.inspector
            .audio_trigger_section_mut()
            .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
    }

    // Clip chrome → inspector (per-clip effects removed)
    if let Some(clip_id) = &selection.primary_selected_clip_id {
        let clip = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *clip_id);
        if let Some(clip) = clip {
            // Sync clip chrome MODE before build so the tree layout is correct.
            // Value sync (name, bpm, etc.) happens in push_state after build.
            let is_video = !clip.video_clip_id.is_empty();
            let is_gen = clip.generator_type != PresetTypeId::NONE;
            let is_audio = clip.is_audio();
            ui.inspector
                .clip_chrome_mut()
                .set_mode(true, is_video, is_gen, is_audio, clip.is_looping);
            // Feed the detection rows before build so the row count drives layout.
            if is_audio {
                use manifold_core::audio_clip_detection::{
                    quantize_grid_label, DetectionConfig,
                };
                use manifold_core::types::LayerType;
                use manifold_ui::panels::clip_chrome::{DetectInstrumentRow, DetectionView};

                let default_cfg;
                let (cfg, detection) = match clip.audio_detection.as_ref() {
                    Some(d) => (&d.config, Some(d)),
                    None => {
                        default_cfg = DetectionConfig::default();
                        (&default_cfg, None)
                    }
                };

                // Candidate routing layers (non-group) for the per-row dropdowns.
                let candidates: Vec<(manifold_core::LayerId, String)> = project
                    .timeline
                    .layers
                    .iter()
                    .filter(|l| l.layer_type != LayerType::Group)
                    .map(|l| (l.layer_id.clone(), l.name.clone()))
                    .collect();

                let instruments = cfg
                    .instruments
                    .iter()
                    .map(|inst| {
                        let count =
                            detection.map_or(0, |d| d.count(inst.trigger_type));
                        let layer_label = inst
                            .target_layer
                            .as_ref()
                            .and_then(|id| {
                                candidates.iter().find(|(lid, _)| lid == id).map(|(_, n)| n.clone())
                            })
                            .unwrap_or_else(|| "Auto".to_string());
                        DetectInstrumentRow {
                            label: format!("{:?}", inst.trigger_type),
                            enabled: inst.enabled,
                            sensitivity: inst.sensitivity,
                            count,
                            layer_label,
                        }
                    })
                    .collect();

                let view = DetectionView {
                    quantize_label: quantize_grid_label(cfg.quantize_on, cfg.quantize_step_beats),
                    onset_ms: (cfg.onset_compensation.0 * 1000.0) as f32,
                    has_analysis: detection.is_some_and(|d| d.analysis.is_some()),
                    instruments,
                };
                ui.inspector.clip_chrome_mut().set_detection(&view);
                ui.set_clip_detect_layers(candidates);
            }
        } else {
            ui.inspector
                .clip_chrome_mut()
                .set_mode(false, false, false, false, false);
        }
    } else {
        ui.inspector
            .clip_chrome_mut()
            .set_mode(false, false, false, false, false);
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// OSC address scope for effect param configs.
/// Master effects use `/master/`, layer effects use `/layer/{id}/`, clips have no OSC.
#[derive(Clone, Copy)]
enum OscScope<'a> {
    Master,
    Layer(&'a str),
}

/// Convert a slice of `PresetInstance` into [`ParamSurface`]s for the UI.
/// Unity: EffectCardState.SyncFromDataModel — populates all data-derived visual state.
///
/// Iterates BOTH the def-declared static block AND the per-instance
/// user-tail bindings, producing one [`ParamRow`] per slot in
/// `effect.param_values` order. The card renders a slider for every
/// exposed entry; hidden static slots and unchecked user-tail entries
/// (the latter are removed from `user_param_bindings` rather than
/// hidden, so they never reach this loop) are filtered at build time.
/// Build the per-row driver + envelope + automation modulation facts for one
/// preset instance's card, one [`RowMod`] per row (D3), all sized to `n` (the
/// card's param count). Shared by the effect and generator card builders —
/// the only thing that differs between them is `resolve`, the `param_id → slot
/// index` mapping (an effect resolves via `param_id_to_value_index`, a generator
/// via its graph/registry `row_index_of`). The rows are identical; the
/// per-card `has_drv` / `has_env` summary flags stay with each caller (the
/// generator card intentionally forces them false).
///
/// `resolve` maps a modulation row's `param_id` to its card slot index.
/// `latched` is `ContentState::automation_latched_params` — checked against
/// `(inst.id, lane.param_id)` for the overridden-gray state (P4 §7's dot).
/// Always `inst.id`, which (fixed 2026-07-11) is now also the card's own
/// DISPLAYED `effect_id` for both kinds — `preset_to_config` used to blank
/// the generator arm's `effect_id` to `EffectId::new("")`, so this function
/// and the card disagreed about a generator's identity even though both
/// ultimately read the same real, freshly-synthesized `EffectId` (see
/// `manifold_playback::automation`'s `AutomationLatches` doc comment).
fn build_card_modulation(
    inst: &PresetInstance,
    n: usize,
    resolve: impl Fn(&str) -> Option<usize>,
    latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> Vec<RowMod> {
    let mut rows = vec![RowMod::default(); n];
    if let Some(ref drivers) = inst.drivers {
        for d in drivers {
            if !d.enabled {
                continue;
            }
            let Some(pi) = resolve(d.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.driver_active = true;
            row.trim_min = d.trim_min;
            row.trim_max = d.trim_max;
            row.driver_beat_div_idx = beat_div_to_button_index(d.beat_division.base_division());
            row.driver_waveform_idx = d.waveform as i32;
            row.driver_reversed = d.reversed;
            row.driver_dotted = d.beat_division.is_dotted();
            row.driver_triplet = d.beat_division.is_triplet();
            row.driver_free_period = d.free_period_beats;
        }
    }
    if let Some(ref envelopes) = inst.envelopes {
        for env in envelopes {
            if !env.enabled {
                continue;
            }
            let Some(pi) = resolve(env.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.envelope_active = true;
            row.target_norm = env.target_normalized;
            row.env_decay = env.decay_beats;
        }
    }
    if let Some(ref lanes) = inst.automation_lanes {
        for lane in lanes {
            // Enabled + non-empty only (§7: "an empty/disabled lane shows no
            // dot") — matches the sampler's own `has_lanes` gate in
            // `manifold_playback::automation`.
            if !lane.enabled || lane.points.is_empty() {
                continue;
            }
            let Some(pi) = resolve(lane.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.automation_active = true;
            row.automation_overridden = latched
                .iter()
                .any(|(eid, pid)| *eid == inst.id && *pid == lane.param_id);
        }
    }
    rows
}

/// Build the per-param audio-modulation display state for a card from the
/// instance's `audio_mods`. The card-level send list (`send_labels`/`send_ids`)
/// is filled separately by [`attach_audio_sends`] (it needs the project's
/// `AudioSetup`, which this per-instance builder doesn't carry).
///
/// §9: a trigger-gate row's config is a normal `ParameterAudioMod` like any
/// other, so this single walk covers it too — `trigger_mode_idx` is read off
/// `am.trigger_mode` (defaulting to `Both`, mirroring the evaluator's
/// `unwrap_or(TriggerFireMode::Both)` fallback) alongside the other fields.
/// No `is_trigger_gate` awareness needed here; only the UI's collapsed-row
/// badge and Mode row care which row it is.
fn build_audio_card_state(
    inst: &PresetInstance,
    n: usize,
    resolve: impl Fn(&str) -> Option<usize>,
) -> AudioCardState {
    let mut a = AudioCardState {
        rows: vec![AudioRowState::default(); n],
        send_labels: Vec::new(),
        send_ids: Vec::new(),
    };
    for am in inst.audio_mods.iter().flatten() {
        if !am.enabled {
            continue;
        }
        let Some(pi) = resolve(am.param_id.as_ref()).filter(|&pi| pi < n) else {
            continue;
        };
        let row = &mut a.rows[pi];
        row.active = true;
        row.send_id = Some(am.source.send_id.clone());
        row.range_min = am.shape.range_min;
        row.range_max = am.shape.range_max;
        row.invert = am.shape.invert;
        row.rate = am.shape.rate_of_change;
        row.sensitivity = am.shape.sensitivity;
        row.attack_ms = am.shape.attack_ms;
        row.release_ms = am.shape.release_ms;
        row.kind_idx = am.source.feature.kind.index() as i32;
        row.band_idx = am.source.feature.band.index() as i32;
        // PARAM_STEP_ACTIONS D3: an unset `trigger_mode`'s effective default
        // depends on the mod's action — a gate's (or a plain Continuous mod's)
        // arm-time default is `Both` (adding audio must not silently kill clip
        // launches, §9 U3); a Step/Random mod's default is `Transient` (a step
        // mod with no audio intent armed is meaningless — the user opened an
        // audio drawer). This must track the evaluator's own default exactly,
        // or the drawer shows a Mode selection that isn't what actually fires.
        let default_mode = if matches!(am.action, manifold_core::audio_mod::TriggerAction::Continuous)
        {
            manifold_core::audio_trigger::TriggerFireMode::Both
        } else {
            manifold_core::audio_trigger::TriggerFireMode::Transient
        };
        row.trigger_mode_idx = match am.trigger_mode.unwrap_or(default_mode) {
            manifold_core::audio_trigger::TriggerFireMode::ClipEdge => 0,
            manifold_core::audio_trigger::TriggerFireMode::Transient => 1,
            manifold_core::audio_trigger::TriggerFireMode::Both => 2,
        };
        match am.action {
            manifold_core::audio_mod::TriggerAction::Continuous => {
                row.action_idx = 0;
            }
            manifold_core::audio_mod::TriggerAction::Step { amount, wrap } => {
                row.action_idx = 1;
                row.step_amount = amount;
                row.wrap_idx = match wrap {
                    manifold_core::audio_mod::WrapMode::Wrap => 0,
                    manifold_core::audio_mod::WrapMode::Bounce => 1,
                    manifold_core::audio_mod::WrapMode::Clamp => 2,
                };
            }
            manifold_core::audio_mod::TriggerAction::Random => {
                row.action_idx = 2;
            }
        }
    }
    a
}

/// Reusable driver/envelope/audio-mod lookup for a SINGLE param id on a
/// [`PresetInstance`] — the same authority chain [`preset_to_config`] walks
/// for every card row ([`build_card_modulation`] + [`build_audio_card_state`]),
/// scoped down from a whole card's row list to one id. SCENE_PANEL_UX_DESIGN.md's
/// UX-P3b sizing amendment names this refactor as its own deliverable: the
/// Scene Setup panel's exposed-param rows resolve their driver/envelope/
/// audio-mod facts through this, instead of re-deriving the lookup a second
/// time against the layer's generator `PresetInstance`.
///
/// Returns `(Vec<RowMod>, AudioCardState)` sized to `n = 1` — index `0` is
/// always the queried param, regardless of its real position in `inst.params`.
/// `automation_latched` is `ContentState::automation_latched_params`, same as
/// every other caller of `build_card_modulation`.
///
/// Un-suppression trigger fired (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md
/// C-P1a): called by [`row_modulation_for_id`] below, which flattens this
/// query's sized-to-1 output into one [`RowModulation`] scalar struct per
/// Environment/Fog row for `sync_inspector_data`'s scene section.
pub(crate) fn lookup_param_mod_for_id(
    inst: &PresetInstance,
    param_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> (Vec<RowMod>, AudioCardState) {
    let resolve = |id: &str| (id == param_id).then_some(0);
    (
        build_card_modulation(inst, 1, resolve, automation_latched),
        build_audio_card_state(inst, 1, resolve),
    )
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D3): flatten
/// [`lookup_param_mod_for_id`]'s sized-to-1 `(Vec<RowMod>, AudioCardState)`
/// into one scalar [`manifold_ui::panels::scene_setup_panel::RowModulation`]
/// for a single Environment/Fog row. `inst = None` (no generator on the
/// layer yet, or the layer isn't a generator) returns the idle default —
/// same "no modulation, not an error" contract `lookup_param_mod_for_id`
/// itself has for an un-modulated param.
/// BUG-249: the scene-row entry point — translate `(node_doc_id, param_key)`
/// to the REAL exposed-param binding id before the modulation lookup. Scene
/// rows used to query by their synthesized `scene.{doc}.{param}` id, which
/// never exists on `inst.params`, so the UI read back the very arm it had
/// stored against an id the runtime silently drops (the closed loop the bug
/// names). An unexposed param has no binding → idle default, same "no
/// modulation, not an error" contract as `inst = None`.
pub(crate) fn scene_row_modulation(
    inst: Option<&PresetInstance>,
    effective_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    node_doc_id: u32,
    param_key: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> manifold_ui::panels::scene_setup_panel::RowModulation {
    // Instance graph first; a TRACKING instance (graph: None — fresh
    // imports) resolves against the effective catalog def instead.
    let real_id = inst
        .and_then(|i| i.binding_id_for_node_param(node_doc_id, param_key))
        .or_else(|| {
            manifold_core::effects::binding_id_for_node_param_in(
                effective_def?,
                node_doc_id,
                param_key,
            )
        });
    match real_id {
        Some(id) => row_modulation_for_id(inst, &id, automation_latched),
        None => manifold_ui::panels::scene_setup_panel::RowModulation::default(),
    }
}

pub(crate) fn row_modulation_for_id(
    inst: Option<&PresetInstance>,
    param_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> manifold_ui::panels::scene_setup_panel::RowModulation {
    use manifold_ui::panels::scene_setup_panel::RowModulation;
    let Some(inst) = inst else {
        return RowModulation::default();
    };
    let (m, a) = lookup_param_mod_for_id(inst, param_id, automation_latched);
    let row = &m[0];
    let audio_row = &a.rows[0];
    RowModulation {
        driver_active: row.driver_active,
        trim_min: row.trim_min,
        trim_max: row.trim_max,
        driver_beat_div_idx: row.driver_beat_div_idx,
        driver_waveform_idx: row.driver_waveform_idx,
        driver_reversed: row.driver_reversed,
        driver_dotted: row.driver_dotted,
        driver_triplet: row.driver_triplet,
        driver_free_period: row.driver_free_period,
        envelope_active: row.envelope_active,
        target_norm: row.target_norm,
        env_decay: row.env_decay,
        automation_active: row.automation_active,
        automation_overridden: row.automation_overridden,
        audio_active: audio_row.active,
        audio_send_id: audio_row.send_id.clone(),
        audio_kind_idx: audio_row.kind_idx,
        audio_band_idx: audio_row.band_idx,
        audio_range_min: audio_row.range_min,
        audio_range_max: audio_row.range_max,
        audio_invert: audio_row.invert,
        audio_rate: audio_row.rate,
        audio_sensitivity: audio_row.sensitivity,
        audio_attack_ms: audio_row.attack_ms,
        audio_release_ms: audio_row.release_ms,
        audio_trigger_mode_idx: audio_row.trigger_mode_idx,
        audio_action_idx: audio_row.action_idx,
        audio_step_amount: audio_row.step_amount,
        audio_wrap_idx: audio_row.wrap_idx,
    }
}

#[cfg(test)]
mod param_mod_lookup_tests {
    use super::*;
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};

    fn driver_for(param_id: &str) -> ParameterDriver {
        ParameterDriver {
            param_id: std::borrow::Cow::Owned(param_id.to_string()),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.1,
            trim_max: 0.9,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// UX-P3b (SCENE_PANEL_UX_DESIGN.md sizing amendment): the reusable
    /// single-param query must find the SAME driver `preset_to_config`'s
    /// `build_card_modulation` would find for that id at its real card
    /// position — it just doesn't need that position, because it always
    /// reports at index 0.
    #[test]
    fn lookup_finds_the_named_params_driver_regardless_of_manifest_position() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("intensity")]);

        let (modulation, _audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(modulation[0].driver_active);
        assert_eq!(modulation[0].trim_min, 0.1);
        assert_eq!(modulation[0].trim_max, 0.9);
    }

    /// A driver on a DIFFERENT param id must not leak into this param's slot
    /// — the query is scoped to the exact id it was asked about, not "any
    /// driver on the instance."
    #[test]
    fn lookup_ignores_drivers_on_other_param_ids() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("fill")]);

        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(!modulation[0].driver_active);
        assert!(!audio.rows[0].active);
    }

    /// No drivers/envelopes/audio-mods at all → an idle single-slot result,
    /// not a panic (the scene panel calls this for every exposed row on
    /// every rebuild, including ones with no modulation yet).
    #[test]
    fn lookup_on_unmodulated_param_returns_idle_slot() {
        let inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[]);
        assert!(!modulation[0].driver_active);
        assert!(!modulation[0].envelope_active);
        assert!(!audio.rows[0].active);
    }
}

/// Resolve a send's routed channels to a human label for the Audio Setup row:
/// the channel name(s) joined with " + ", or "Not routed" when empty. Falls
/// back to a 1-based index when no device metadata is available.
fn channel_label(
    device: Option<&manifold_audio::directory::DeviceInfo>,
    is_tap: bool,
    channels: &[u16],
) -> String {
    if channels.is_empty() {
        return "Not routed".to_string();
    }
    let name_of = |ch: u16| -> String {
        // A tap is a fixed stereo mixdown — channel 0/1 are Left/Right, matching
        // the tap channel picker. A hardware device uses its platform names.
        if is_tap {
            return match ch {
                0 => "Left".to_string(),
                1 => "Right".to_string(),
                n => format!("Channel {}", n + 1),
            };
        }
        device
            .and_then(|d| d.channels.get(ch as usize))
            .map(|c| c.display_name())
            .unwrap_or_else(|| format!("Channel {}", ch + 1))
    };
    channels.iter().map(|&ch| name_of(ch)).collect::<Vec<_>>().join(" + ")
}

/// Stamp the card-level available-send list (labels + ids) onto every card
/// config, from the project's `AudioSetup`. One pass after the configs are
/// built, so the per-instance builders stay project-agnostic.
fn attach_audio_sends(configs: &mut [ParamSurface], setup: &manifold_core::audio_setup::AudioSetup) {
    if setup.sends.is_empty() {
        return;
    }
    let labels: Vec<String> = setup.sends.iter().map(|s| s.label.clone()).collect();
    let ids: Vec<manifold_core::AudioSendId> = setup.sends.iter().map(|s| s.id.clone()).collect();
    for c in configs.iter_mut() {
        c.audio.send_labels = labels.clone();
        c.audio.send_ids = ids.clone();
    }
}

/// Thin adapter: build a card config for each effect in `effects`, skipping
/// any whose preset def is missing. The real work is the unified
/// [`param_surface`].
fn effects_to_surfaces(
    effects: &[PresetInstance],
    osc_scope: OscScope<'_>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> Vec<ParamSurface> {
    effects
        .iter()
        .enumerate()
        .filter_map(|(i, fx)| {
            param_surface(
                fx,
                manifold_core::preset_def::PresetKind::Effect,
                i,
                osc_scope,
                None,
                automation_latched,
            )
        })
        .collect()
}

/// The empty generator card (no resolvable param source). Mirrors the old
/// `gen_params_to_surface` fallback exactly.
fn empty_generator_surface(inst: &PresetInstance) -> ParamSurface {
    ParamSurface {
        kind: ParamCardKind::Generator,
        title: inst.generator_type().to_string(),
        collapsed: false,
        effect_index: 0,
        // Stays blank (unlike the real-id arm in `param_surface` below):
        // zero rows means zero audio-mod rows, so nothing on this card ever
        // hosts a fire-meter lookup — there's no divergence risk to fix here.
        effect_id: manifold_core::EffectId::new(""),
        enabled: true,
        supports_envelopes: true,
        has_graph_mod: false,
        layer_id: None,
        rows: vec![],
        string_params: vec![],
        audio: Default::default(),
        relight: crate::ui_translate::relight_card_config_from(inst),
    }
}

/// BUG-080 D2: release-mode once-per-instance warn for a provisional
/// manifest reaching this seam. Shaped like the BUG-038 OSC-send throttle —
/// a plain "seen once" set is enough here, not a reconnect transition.
/// `debug_assert!` already screams in dev builds; this is the release-mode
/// signal that a load/ingest path skipped `reconcile_param_manifests()`.
fn warn_provisional_manifest_once(id: &manifold_core::EffectId) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::HashSet<manifold_core::EffectId>>> =
        OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(id.clone()) {
        log::warn!(
            "BUG-080: provisional manifest reached param_surface for effect_id={id:?} \
             — a load/ingest path skipped reconcile_param_manifests()"
        );
    }
}

/// THE projection (D1, `docs/WIDGET_TREE_DESIGN.md` — replaces the former
/// two-pass builder and its per-call id-to-index map). ONE manifest walk
/// builds [`ParamRow`]s directly — descriptor
/// (`spec`) verbatim from the manifest's `ParamSpecDef` fields, state
/// (`value`) alongside; display-value resolution (D7) happens here and
/// nowhere else. Returns `None` only for an effect whose preset def is
/// missing (skipped as a card); a generator with no source returns the empty
/// card.
fn param_surface(
    inst: &PresetInstance,
    kind: manifold_core::preset_def::PresetKind,
    effect_index: usize,
    osc_scope: OscScope<'_>,
    clip_string_params: Option<&std::collections::BTreeMap<String, String>>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> Option<ParamSurface> {
    use manifold_core::preset_def::PresetKind;
    let preset_type = inst.effect_type();
    let reg_def = manifold_core::preset_definition_registry::try_get(preset_type);

    match kind {
        PresetKind::Effect => {
            reg_def.as_deref()?; // skip cards for def-less effects
        }
        PresetKind::Generator => {
            if inst.params.is_empty() {
                // No resolvable param source (mirrors the old
                // graph-metadata-empty + registry-empty fallback chain,
                // now resolved once inside `build_param_manifest`).
                return Some(empty_generator_surface(inst));
            }
        }
    }

    // BUG-080 seam: a provisional manifest (built against an incomplete
    // registry, not yet reconciled) reaching UI row translation means a
    // load/ingest path skipped `reconcile_param_manifests()`. Loud in dev,
    // throttled-once in release. See docs/PARAM_MANIFEST_GATE_DESIGN.md D2.
    debug_assert!(
        !inst.manifest_provisional(),
        "BUG-080: provisional manifest reached param_surface — a load/ingest path \
         skipped reconcile_param_manifests() (effect_id={:?})",
        inst.id,
    );
    if inst.manifest_provisional() {
        warn_provisional_manifest_once(&inst.id);
    }

    // ── ONE walk over the manifest (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4):
    // `inst.params` already carries every fact a row needs — descriptor
    // (spec) + state (exposed), id-keyed, insertion order IS card order —
    // because `build_param_manifest` resolved the registry-vs-graph-metadata
    // authority chain ONCE at instantiation/load. This walk reads that
    // result; it does not re-derive the authority chain or re-read a
    // per-instance graph override live (that override, `meta.params`, is a
    // save-time-derived shadow now — D12 — not a second live source).
    let row_index_of: ahash::AHashMap<String, usize> =
        inst.params.iter().enumerate().map(|(i, p)| (p.id().to_string(), i)).collect();

    let mut rows: Vec<ParamRow> = inst
        .params
        .iter()
        .map(|p| {
            let id = p.id().to_string();
            let osc_address = match osc_scope {
                OscScope::Master => {
                    manifold_core::preset_definition_registry::get_osc_address_by_id(
                        preset_type,
                        &id,
                    )
                }
                OscScope::Layer(lid) => {
                    manifold_core::preset_definition_registry::get_osc_address_for_layer_by_id(
                        preset_type,
                        lid,
                        &id,
                    )
                }
            };
            let abl_mapping = inst.ableton_mappings.as_ref().and_then(|mappings| {
                if id.is_empty() {
                    return None;
                }
                mappings.iter().find(|m| m.param_id == id)
            });
            let ableton_display = abl_mapping.map(|mapping| AbletonMappingDisplay {
                macro_name: mapping.address.macro_name.clone(),
                track_name: mapping.address.track_name.clone(),
                device_name: mapping.address.device_name.clone(),
                status: crate::ui_translate::ableton_mapping_status_to_ui(mapping.status),
                inverted: mapping.inverted,
            });
            let ableton_range = abl_mapping.map(|m| (m.range_min, m.range_max));
            let value_labels = if p.spec.value_labels.is_empty() {
                None
            } else {
                Some(p.spec.value_labels.clone())
            };
            ParamRow {
                id: std::borrow::Cow::Owned(id),
                spec: RowSpec {
                    name: p.spec.name.clone(),
                    min: p.spec.min,
                    max: p.spec.max,
                    default: p.spec.default_value,
                    whole_numbers: p.spec.whole_numbers,
                    is_angle: p.spec.is_angle,
                    is_toggle: p.spec.is_toggle,
                    is_trigger: p.spec.is_trigger,
                    is_trigger_gate: p.spec.is_trigger_gate,
                    value_labels,
                    section: p.spec.section.clone(),
                },
                // D7: display-value resolution decided here — base/effective
                // straight off the manifest slot, `driven` false (state_sync
                // has no wire-fed presentation case; only the editor snapshot
                // path sets it).
                value: RowValue { base: p.base, effective: p.value, exposed: p.exposed, driven: false },
                modulation: RowMod::default(),
                mapping: RowMapping { osc_address, ableton_display, ableton_range, mappable: true },
            }
        })
        .collect();
    let n = rows.len();

    let mod_rows = build_card_modulation(
        inst,
        n,
        |id| row_index_of.get(id).copied(),
        automation_latched,
    );
    for (row, rm) in rows.iter_mut().zip(mod_rows) {
        row.modulation = rm;
    }
    let audio = build_audio_card_state(inst, n, |id| row_index_of.get(id).copied());

    // String params are a generator-only surface (text inputs, font dropdowns),
    // sourced from the registry def.
    let string_params: Vec<ParamCardStringInfo> = match kind {
        PresetKind::Generator => reg_def
            .as_deref()
            .map(|def| {
                def.string_param_defs
                    .iter()
                    .map(|sp_def| {
                        let value = clip_string_params
                            .and_then(|m| m.get(sp_def.key))
                            .cloned()
                            .unwrap_or_else(|| sp_def.default_value.to_string());
                        ParamCardStringInfo {
                            name: sp_def.name.to_string(),
                            key: sp_def.key.to_string(),
                            value,
                            use_dropdown: sp_def.use_dropdown,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        PresetKind::Effect => Vec::new(),
    };

    let (card_kind, effect_id, enabled, collapsed, has_graph_mod) = match kind {
        PresetKind::Effect => (
            ParamCardKind::Effect,
            inst.id.clone(),
            inst.enabled,
            inst.collapsed,
            inst.graph.is_some(),
        ),
        PresetKind::Generator => (
            ParamCardKind::Generator,
            // Real `inst.id`, not a blanked placeholder (fixed 2026-07-11):
            // this is the SAME id `build_card_modulation` already used for
            // its own lookups (see that fn's doc comment) and the SAME id
            // the content thread hashes into a fire-meter key
            // (`fire_meter_key_for_param`) — a blanked display id here meant
            // the UI's lookup key could never match the content thread's,
            // so a generator card's audio-mod meters never resolved.
            inst.id.clone(),
            true,
            false,
            // PRESET_LIBRARY_DESIGN D3/P4: a generator's per-card divergence
            // is the SAME `graph.is_some()` bit as an effect's (graph-home
            // unification put both on `PresetInstance`) — this was
            // hardcoded `false` (a pre-P4 gap that permanently suppressed
            // the MOD badge on generator cards regardless of actual
            // divergence), fixed to read the real state like the Effect arm
            // above.
            inst.graph.is_some(),
        ),
    };

    Some(ParamSurface {
        kind: card_kind,
        effect_index,
        effect_id,
        // A project-embedded (forked) preset's `display_name` — sourced from
        // `reg_def`, the same catalog-overlay-aware lookup the rows above
        // used — carries its own human name directly (D2: ids are now
        // display-based, so no id-format parsing is needed to render one).
        // Falls back to the static registry name for stock/user presets not
        // (yet) reflected in the overlay snapshot.
        title: reg_def.as_deref().map(|d| d.display_name.clone()).unwrap_or_else(|| {
            manifold_core::preset_type_registry::display_name(preset_type).to_string()
        }),
        enabled,
        collapsed,
        supports_envelopes: true,
        string_params,
        layer_id: None,
        rows,
        has_graph_mod,
        audio,
        relight: crate::ui_translate::relight_card_config_from(inst),
    })
}

/// Map a base BeatDivision to its button index (0-10).
/// Reverse of BeatDivision::from_button_index.
fn beat_div_to_button_index(div: BeatDivision) -> i32 {
    match div {
        BeatDivision::ThirtySecond => 0,
        BeatDivision::Sixteenth => 1,
        BeatDivision::Eighth | BeatDivision::EighthDotted | BeatDivision::EighthTriplet => 2,
        BeatDivision::Quarter | BeatDivision::QuarterDotted | BeatDivision::QuarterTriplet => 3,
        BeatDivision::Half | BeatDivision::HalfDotted | BeatDivision::HalfTriplet => 4,
        BeatDivision::Whole | BeatDivision::WholeDotted | BeatDivision::WholeTriplet => 5,
        BeatDivision::TwoWhole | BeatDivision::TwoWholeDotted => 6,
        BeatDivision::FourWhole => 7,
        BeatDivision::EightWhole => 8,
        BeatDivision::SixteenWhole => 9,
        BeatDivision::ThirtyTwoWhole => 10,
    }
}

/// Thin adapter: build the generator card config via the unified
/// [`preset_to_config`]. The generator branch always yields a config (a real
/// one, or the empty fallback when no param source resolves), so the `expect`
/// never fires.
fn gen_params_to_surface(
    gp: &manifold_core::effects::PresetInstance,
    layer_id: &str,
    clip_string_params: Option<&std::collections::BTreeMap<String, String>>,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
) -> ParamSurface {
    param_surface(
        gp,
        manifold_core::preset_def::PresetKind::Generator,
        0,
        OscScope::Layer(layer_id),
        clip_string_params,
        automation_latched,
    )
    .expect("generator param_surface always yields a config")
}

/// Build a human-readable description for a macro mapping target.
fn describe_macro_mapping(
    target: &manifold_core::MacroMappingTarget,
    project: &manifold_core::project::Project,
) -> String {
    use manifold_core::MacroMappingTarget;
    match target {
        MacroMappingTarget::MasterOpacity => "Master Opacity".to_string(),
        MacroMappingTarget::Effect {
            effect_id,
            param_id,
        } => {
            let Some(fx) = project.find_effect_by_id(effect_id) else {
                return "Effect → ?".to_string();
            };
            let effect_type = fx.effect_type();
            // Effect display name is type-level template metadata (a boundary
            // read); the param name comes off the LIVE manifest so user-added /
            // glb params resolve instead of rendering "?" (was a registry
            // id-lookup miss, the UI twin of the P4 blind spot).
            let effect_name = manifold_core::preset_definition_registry::try_get(effect_type)
                .map(|d| d.display_name.clone())
                .unwrap_or_else(|| effect_type.as_str().to_string());
            let param_name = fx
                .params
                .get(param_id.as_ref())
                .map(|p| p.spec.name.clone())
                .unwrap_or_else(|| "?".to_string());
            // Prefix with the owning layer's name; master effects have none.
            match project.layer_id_for_effect(effect_id) {
                Some(layer_id) => {
                    let layer_name = project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == layer_id)
                        .map(|l| l.name.as_str())
                        .unwrap_or("?");
                    format!("{} {} → {}", layer_name, effect_name, param_name)
                }
                None => format!("{} → {}", effect_name, param_name),
            }
        }
        MacroMappingTarget::LayerOpacity { layer_id } => {
            let layer_name = project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == *layer_id)
                .map(|l| l.name.as_str())
                .unwrap_or(layer_id.as_str());
            format!("{} Opacity", layer_name)
        }
        MacroMappingTarget::GenParam { layer_id, param_id } => {
            let layer = project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == *layer_id);
            let layer_name = layer.map(|l| l.name.as_str()).unwrap_or("?");
            // Param name off the LIVE manifest (user-added / glb params resolve).
            let param_name = layer
                .and_then(|l| l.gen_params())
                .and_then(|gp| gp.params.get(param_id.as_ref()).map(|p| p.spec.name.clone()))
                .unwrap_or_else(|| "?".to_string());
            format!("{} Gen → {}", layer_name, param_name)
        }
    }
}

#[cfg(test)]
mod param_label_tests {
    use super::*;
    use manifold_core::MacroMappingTarget;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};

    fn user_spec(id: &str, name: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }
    }

    /// P5: a macro-mapping label resolves a param's display name from the LIVE
    /// manifest, so a user-added param shows its name instead of "?" (before,
    /// the registry id-lookup missed it — the UI twin of the P4
    /// blind spot).
    #[test]
    fn describe_macro_mapping_uses_live_manifest_param_name() {
        let mut project = manifold_core::project::Project::default();
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::BLOOM);
        fx.params =
            ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow", "Glow Amount"))]);
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);

        let target = MacroMappingTarget::Effect {
            effect_id,
            param_id: std::borrow::Cow::Owned("user_glow".to_string()),
        };
        let label = describe_macro_mapping(&target, &project);
        assert!(
            label.contains("Glow Amount"),
            "label must show the live param name, got {label:?}"
        );
        assert!(!label.contains('?'), "label must not fall back to ?, got {label:?}");
    }
}

#[cfg(test)]
mod sync_card_values_tests {
    //! The extraction proof for `sync_card_values`: a param value changed in
    //! the (UI-local) project after the inspector was configured must reach
    //! the card's on-tree value text through `sync_card_values` alone — no
    //! structural re-sync, no rebuild. This is the exact call the
    //! graph-editor window's present path now makes every frame
    //! (`app_render.rs::present_graph_editor_window`), so the test guards the
    //! editor-window slider-freeze fix, not just the helper.
    use super::*;
    use manifold_core::params::{Param, ParamManifest};

    fn user_spec(id: &str, name: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }
    }

    fn tree_has_text(ui: &UIRoot, needle: &str) -> bool {
        ui.tree
            .nodes()
            .iter()
            .any(|n| n.text.as_deref() == Some(needle))
    }

    #[test]
    fn project_param_change_reaches_card_value_text_via_sync_card_values() {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params =
            ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow", "Glow Amount"))]);
        project.settings.master_effects.push(fx);

        // Configure + build exactly as the structural sync does, at the
        // pre-change value (0.5 → "0.50" via `format_param_value`'s `{:.2}`).
        let mut ui = UIRoot::new();
        let selection = SelectionState::default();
        sync_inspector_data(&mut ui, &project, None, &selection, &[]);
        ui.build_inspector_in_rect(manifold_ui::Rect::new(0.0, 0.0, 640.0, 2000.0));
        assert!(
            tree_has_text(&ui, "0.50"),
            "baseline: the configured card must show the pre-change value"
        );

        // A modulation-style write to the local project, then ONLY the
        // value-sync call — no configure, no rebuild.
        project.settings.master_effects[0]
            .params
            .get_mut("user_glow")
            .expect("user_glow param")
            .value = 0.75;
        sync_card_values(&mut ui, &project, None);

        assert!(
            tree_has_text(&ui, "0.75"),
            "sync_card_values must push the new value onto the already-built card"
        );
        // No "stale text is gone" assertion: "0.50" legitimately appears on
        // other widgets (e.g. mapping trim fields seeded from the same
        // default), so disappearance is not a sound oracle here.
    }
}

#[cfg(test)]
mod build_audio_card_state_trigger_mode_tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_core::effects::PresetInstance;
    use manifold_core::id::AudioSendId;

    fn resolve<'a>(params: &'a [&'a str]) -> impl Fn(&str) -> Option<usize> + 'a {
        move |id| params.iter().position(|&p| p == id)
    }

    /// §9: a trigger-gate row's fire mode lives on the mod itself
    /// (`ParameterAudioMod.trigger_mode`), not a separate per-instance
    /// config — `build_audio_card_state` reads it into `trigger_mode_idx`
    /// in the SAME walk that populates `active`/`send_id`/`band_idx`/etc.
    /// This is the function `param_surface` calls to populate
    /// `ParamSurface.audio`, so a green test here is the proof the
    /// config the card sees actually carries the project's live mode, not
    /// just that the model round-trips in isolation.
    #[test]
    fn trigger_mode_reads_off_the_mod_alongside_every_other_field() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        m.trigger_mode = Some(TriggerFireMode::Both);
        inst.audio_mods = Some(vec![m]);

        let params = ["amount", "clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));

        assert!(!cfg.rows[0].active);
        assert!(cfg.rows[1].active);
        assert_eq!(cfg.rows[0].send_id, None);
        assert_eq!(cfg.rows[1].send_id, Some(AudioSendId::new("send-kick")));
        assert_eq!(cfg.rows[1].band_idx, AudioBand::Low.index() as i32);
        assert_eq!(cfg.rows[1].trigger_mode_idx, 2); // Both
    }

    /// A disabled mod (armed once, then disarmed via the "A" button, which
    /// flips `enabled` without clearing the rest) reads as fully inactive —
    /// the standard per-param "skip if !enabled" rule covers a trigger-gate
    /// row automatically now; no trigger-specific gate to keep in sync.
    #[test]
    fn disabled_mod_reads_as_inactive() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.enabled = false;
        m.trigger_mode = Some(TriggerFireMode::ClipEdge);
        inst.audio_mods = Some(vec![m]);

        let params = ["clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));
        assert!(!cfg.rows[0].active);
    }

    /// No `trigger_mode` set on the mod (defensive — §9 U3 always arms with
    /// `Some(Both)`, but a hand-built or pre-§9-migrated mod could carry
    /// `None`) reads the SAME `Both` fallback the evaluator uses
    /// (`unwrap_or(TriggerFireMode::Both)`), so the badge/drawer never
    /// disagrees with what actually fires.
    #[test]
    fn missing_trigger_mode_defaults_to_both() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        assert_eq!(m.trigger_mode, None);
        inst.audio_mods = Some(vec![m]);

        let params = ["clip_trigger"];
        let cfg = build_audio_card_state(&inst, params.len(), resolve(&params));
        assert_eq!(cfg.rows[0].trigger_mode_idx, 2); // Both
    }
}

/// Display name for one D6 modifier-stack atom (P2 shows the chain as a
/// display-only line; the interactive stack is P5). Falls back to the raw
/// type_id for anything outside the curated D6 list — never blank, per D3's
/// "custom" degrade rule.
fn modifier_display_name(type_id: &str) -> String {
    match type_id {
        "node.bend_mesh" => "Bend".to_string(),
        "node.twist_mesh" => "Twist".to_string(),
        "node.taper_mesh" => "Taper".to_string(),
        "node.push_along_normals" => "Inflate".to_string(),
        "node.push_mesh" => "Displace by Texture".to_string(),
        "node.morph_mesh" => "Morph".to_string(),
        "node.rotate_3d" => "Rotate".to_string(),
        other => other.to_string(),
    }
}


/// End-to-end round-trip: every fire-meter key the UI's `update_fire_meters`
/// requests must resolve in the SAME `FireMeterCapture` the content-thread
/// evaluators (`evaluate_all_audio_mods` + `LiveTriggerState::evaluate`)
/// produce for the SAME `Project`. Producer (`manifold-playback`) and
/// consumer (`ParamCardPanel`/`AudioTriggerSection` via this module's
/// `sync_inspector_data`) each independently recompute a fire-meter key from
/// an instance's identity — nothing previously verified they agree. That's
/// exactly the bug class the 2026-07-11 generator-card fix closed:
/// `preset_to_config`'s Generator arm used to display `EffectId::new("")`
/// instead of the real `inst.id`, so a generator's audio-mod meter asked for
/// a key the content thread never pushed anything under.
///
/// Builds ONE project carrying all four fire-meter-hosting shapes on a
/// single layer (so one `sync_inspector_data` + `build_in_rect` pass reaches
/// every one of them — `sync_inspector_data` only configures the ACTIVE
/// layer's effects/gen-params/clip-triggers, so spreading the shapes across
/// separate layers like `ui_snapshot::fixtures::inspector_scene` does would
/// mean only one shape's layer is ever active at a time):
///   (a) a GENERATOR instance's `is_trigger_gate` audio mod (Plasma's
///       `clip_trigger`) — the exact shape the generator-card bug shipped
///       under;
///   (b) an EFFECT's plain continuous audio mod on a non-gate param (Bloom)
///       — the shape the 2026-07-11 widening (128→512 `MAX_FIRE_METERS`,
///       `fire_meters.push` moved above the gate-mode fork) started
///       metering;
///   (c) an EFFECT's `is_trigger_gate` audio mod (Strobe's `clip_trigger`);
///   (d) a `Layer.clip_triggers` row.
/// Bloom/Strobe/Plasma are real shipping presets, chosen to mirror
/// `ui_snapshot::fixtures::inspector_scene` (the BUG-082/P3c gate scene) —
/// folded onto one layer instead of three so a single active-layer build
/// surfaces all four.
#[cfg(test)]
mod fire_meter_roundtrip_tests {
    use super::*;
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, AudioModSource, ParameterAudioMod,
    };
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::{
        FireMeterCapture, LayerClipTrigger, TriggerFireMode, fire_meter_key_for_clip_trigger,
        fire_meter_key_for_param,
    };
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::{EffectId, LayerId, PresetTypeId, Seconds};
    use manifold_playback::live_trigger::LiveTriggerState;
    use manifold_playback::modulation::{TriggerPulse, evaluate_all_audio_mods};
    use manifold_ui::node::Rect;

    /// Zero attack/release so a mod's conditioned level snaps instantly to
    /// its raw input within ONE evaluation tick — the same pattern
    /// `manifold-playback::modulation`'s own tests use for deterministic
    /// single-tick assertions (`attach_full_range_low_mod`).
    fn instant_mod(
        param_id: &str,
        send_id: &manifold_core::AudioSendId,
        feature: AudioFeature,
    ) -> ParameterAudioMod {
        let mut m = ParameterAudioMod::new(param_id.to_string().into(), send_id.clone(), feature);
        m.shape.attack_ms = 0.0;
        m.shape.release_ms = 0.0;
        m
    }

    /// The fixture project plus the identity facts the test needs to compute
    /// each shape's expected fire-meter key.
    struct Fixture {
        project: Project,
        layer_id: LayerId,
        bloom_id: EffectId,
        bloom_param: String,
        strobe_id: EffectId,
        plasma_id: EffectId,
    }

    /// One layer carrying all four fire-meter-hosting shapes at once: a
    /// generator (Plasma) with a gate mod, an effects chain (Bloom
    /// continuous + Strobe gate) riding on top of it, and its own
    /// clip-trigger row. A generator layer carrying an effects chain is the
    /// normal MANIFOLD shape (post-processing over a procedural source), so
    /// this isn't a contrived overlap — it's just all on one layer instead
    /// of `inspector_scene`'s three, specifically so ONE `active_layer`
    /// build reaches all four.
    fn build_fixture() -> Fixture {
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();

        let mut layer = Layer::new_generator("PLASMA".into(), PresetTypeId::PLASMA, 0);
        let layer_id = layer.layer_id.clone();

        // (a) GENERATOR instance, `is_trigger_gate` mod on `clip_trigger`.
        let mut gate_on_gen = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        gate_on_gen.trigger_mode = Some(TriggerFireMode::Both);
        layer.gen_params_or_init().audio_mods = Some(vec![gate_on_gen]);
        let plasma_id = layer.gen_params().unwrap().id.clone();

        // (b) EFFECT, plain continuous mod on a non-gate param — the
        // newly-metered shape.
        let mut bloom = PresetInstance::new(PresetTypeId::BLOOM);
        bloom.init_defaults();
        let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
            .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
            .expect("Bloom has at least one param");
        bloom.audio_mods = Some(vec![instant_mod(
            &bloom_param,
            &send_id,
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        )]);
        let bloom_id = bloom.id.clone();

        // (c) EFFECT, `is_trigger_gate` mod on `clip_trigger`.
        let mut strobe = PresetInstance::new(PresetTypeId::new("Strobe"));
        strobe.init_defaults();
        let mut gate_on_fx = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        gate_on_fx.trigger_mode = Some(TriggerFireMode::Transient);
        strobe.audio_mods = Some(vec![gate_on_fx]);
        let strobe_id = strobe.id.clone();

        layer.effects = Some(vec![bloom, strobe]);

        // (d) `Layer.clip_triggers` row.
        let mut trigger = LayerClipTrigger::new(AudioModSource {
            send_id: send_id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Low),
        });
        trigger.enabled = true;
        trigger.shape.attack_ms = 0.0;
        trigger.shape.release_ms = 0.0;
        layer.clip_triggers.push(trigger);

        let mut project = Project::default();
        project.audio_setup.sends.push(send);
        project.timeline.layers.push(layer);

        Fixture { project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id }
    }

    /// One send ("Kick") with a nonzero level on every band/feature the
    /// fixture's four mods read, so a real evaluation tick leaves every one
    /// of them in the producer's `FireMeterCapture`.
    fn hot_snapshot() -> manifold_core::audio_features::AudioFeatureSnapshot {
        let mut bands = [manifold_core::BandFeatures::default(); 4];
        bands[AudioBand::Full.index()].amplitude = 0.8; // (b) Bloom
        bands[AudioBand::Full.index()].transients = 0.8; // (a) Plasma gate
        bands[AudioBand::Low.index()].transients = 0.8; // (c) Strobe gate
        bands[AudioBand::Low.index()].kick = 0.8; // (d) clip-trigger row (Kick always reads Low)
        manifold_core::audio_features::AudioFeatureSnapshot {
            sends: vec![manifold_core::SendFeatures { bands, ..Default::default() }],
        }
    }

    #[test]
    fn every_ui_requested_fire_meter_key_resolves_in_the_producer_capture() {
        let Fixture { mut project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id } =
            build_fixture();

        // ── Producer: the SAME two evaluators the content thread runs every
        // tick (`manifold_playback::modulation::evaluate_all_audio_mods` for
        // every enabled param audio mod, `LiveTriggerState::evaluate` for
        // clip triggers) — see `crates/manifold-playback/tests/engine_tick.rs`
        // and `live_trigger.rs`'s own tests for the same two-call pattern.
        let snapshot = hot_snapshot();
        let dt = Seconds(1.0 / 60.0);
        let mut fire_meters = FireMeterCapture::default();
        let mut pulses: Vec<TriggerPulse> = Vec::new();
        evaluate_all_audio_mods(&mut project, &snapshot, dt, &mut pulses, &[], &mut fire_meters);
        let mut live_trigger = LiveTriggerState::default();
        live_trigger.evaluate(&snapshot, &project.audio_setup, &project.timeline.layers, dt, &mut fire_meters);

        // ── Consumer: the real UI build — `sync_inspector_data` (the same
        // function `ui_snapshot::render_ui_scene` calls) configures the
        // inspector's cards + AUDIO TRIGGERS section from this SAME project,
        // then `build_in_rect` (what the graph-editor window's inspector
        // column, and the main window via `UIRoot::build`, both call)
        // materializes them into a real `UITree`.
        let mut ui = UIRoot::new();
        let mut selection = SelectionState::default();
        selection.select_layer(layer_id.clone());
        sync_inspector_data(&mut ui, &project, Some(0), &selection, &[]);

        // Open every fixture drawer. (a)/(b)/(c) need no explicit "open": an
        // armed audio mod's drawer builds automatically — a toggle/trigger
        // row's drawer whenever its mod is enabled (`build_toggle_trigger_row`),
        // a slider row's whenever Audio is its only (or resolved) active
        // mod-tab (`build_param_row`), both in `param_slider_shared.rs`. Only
        // the AUDIO TRIGGERS section's clip-trigger row (d) is gated behind
        // its OWN collapse/expand UI state (`AudioTriggerSection`), which
        // `configure()` never touches — so it must be opened explicitly here.
        ui.inspector.audio_trigger_section_mut().toggle_collapsed();
        ui.inspector.audio_trigger_section_mut().toggle_row_expanded(0);

        ui.build_inspector_in_rect(Rect::new(0.0, 0.0, 640.0, 4000.0));

        // Record every key the UI requests while pushing levels. `fire_level`
        // is `&dyn Fn`, not `FnMut`, so the recorder needs interior
        // mutability.
        let requested = std::cell::RefCell::new(Vec::<u64>::new());
        let record = |key: u64| -> Option<f32> {
            requested.borrow_mut().push(key);
            fire_meters.get(key)
        };
        ui.inspector.update_fire_meters(&mut ui.tree, &record, dt.0 as f32);
        let requested = requested.into_inner();

        // The four keys the PRODUCER computed for the exact same instances —
        // the ground truth the UI's own keys must agree with.
        let expected: Vec<(&str, u64)> = vec![
            (
                "(a) Plasma generator gate mod (clip_trigger)",
                fire_meter_key_for_param(plasma_id.as_str(), "clip_trigger"),
            ),
            (
                "(b) Bloom continuous mod",
                fire_meter_key_for_param(bloom_id.as_str(), &bloom_param),
            ),
            (
                "(c) Strobe gate mod (clip_trigger)",
                fire_meter_key_for_param(strobe_id.as_str(), "clip_trigger"),
            ),
            (
                "(d) layer clip-trigger row 0",
                fire_meter_key_for_clip_trigger(layer_id.as_str(), 0),
            ),
        ];

        let requested_set: std::collections::HashSet<u64> = requested.iter().copied().collect();

        // (i) every fixture drawer we opened must have had ITS EXACT expected
        // key requested — catches three distinct failure shapes in one
        // check: "meter built but never updated" and "drawer missing its
        // meter" (an audio_configs slot that resolved but whose `DrawerIds`
        // carries no Meter widget, so `update_fire_meters` silently skips
        // it) both mean the key is simply absent from `requested_set`; a
        // THIRD shape — the UI's card computed a DIFFERENT identity for the
        // same instance than the producer did (a blanked/stale `EffectId`,
        // e.g. the pre-2026-07-11 generator-card bug this test targets) —
        // also fails here: the row's drawer still opens and still requests
        // *some* key, just not this one, so `expected`'s real key is still
        // absent from `requested_set`.
        for (label, key) in &expected {
            assert!(
                requested_set.contains(key),
                "the UI never requested the expected fire-meter key for {label}. Either its \
                 drawer never built (the audio_configs slot stayed None), its DrawerIds carries \
                 no Meter widget (update_fire_meters silently skips both), OR — the producer/\
                 consumer divergence class this test exists to catch — the card computed a \
                 DIFFERENT identity key for this same instance than the content thread did (a \
                 blanked or stale EffectId/LayerId), so it requested some other key instead"
            );
        }
        assert_eq!(
            requested_set.len(),
            expected.len(),
            "expected exactly {} open fire-meter rows ({:?}) but the UI requested {} distinct \
             keys — an extra drawer built somewhere this fixture didn't arm",
            expected.len(),
            expected.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            requested_set.len(),
        );

        // (ii) every key the UI requested must resolve against the SAME
        // `FireMeterCapture` the content thread produced — the round-trip
        // proof, and the exact bug class the 2026-07-11 generator-card fix
        // closed: `preset_to_config`'s Generator arm once displayed
        // `EffectId::new("")` instead of the real `inst.id`, so a generator
        // card's audio-mod meter computed a key the content thread never
        // pushed anything under.
        for (label, key) in &expected {
            assert!(
                fire_meters.get(*key).is_some(),
                "producer/consumer key divergence for {label}: the UI's fire-meter key {key} \
                 was never recorded by the content-thread FireMeterCapture — the UI and the \
                 evaluators disagree about this instance's identity"
            );
        }
    }
}

#[cfg(test)]
mod audio_waveform_breakpoints_tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::project::Project;
    use manifold_core::tempo::TempoMapConverter;
    use manifold_core::types::TempoPointSource;
    use manifold_core::{Bpm, Seconds};

    /// (a) A single-point (i.e. effectively constant-tempo) map must yield
    /// exactly 2 breakpoints — clip start and end — reproducing the old
    /// `dur_beats * warped_secs_per_beat` window exactly: `file_secs_at(end)`
    /// must equal `duration_beats * (60 / bpm) * warp_ratio + in_point`.
    #[test]
    fn single_tempo_point_reproduces_old_constant_mapping() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(140.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(140.0), TempoPointSource::Manual, 0.001);

        let clip = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(8.0),
            Beats::from_f32(4.0),
            Seconds(1.5),
            Seconds(120.0),
        );

        let bp = audio_waveform_breakpoints(&clip, &project);
        assert_eq!(bp.len(), 2, "constant tempo must yield exactly 2 breakpoints");
        assert_eq!(bp[0], (0.0, 1.5), "clip start maps to in_point with no elapsed beats");

        let expected_win_secs = 4.0 * (60.0 / 140.0_f32); // warp off (recorded_bpm unset) → project spb
        assert!(
            (bp[1].0 - 1.0).abs() < 1e-6,
            "clip end must be at x_frac 1.0, got {}",
            bp[1].0
        );
        assert!(
            (bp[1].1 - (1.5 + expected_win_secs)).abs() < 1e-4,
            "clip end file_secs {} should equal old in_point + dur*spb window {}",
            bp[1].1,
            1.5 + expected_win_secs
        );
    }

    /// (b) A 3-point tempo map (120 BPM then 60 BPM partway through the clip)
    /// must place a breakpoint exactly at the tempo-change beat, and every
    /// breakpoint's `file_secs` must match
    /// `TempoMapConverter::beat_to_seconds_immut` differences directly — the
    /// same integration playback performs, not a flat per-beat scalar.
    #[test]
    fn three_point_map_matches_tempo_map_converter_directly() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        // Tempo halves to 60 BPM at beat 10 — inside the clip (beats 8..16).
        project
            .tempo_map
            .add_or_replace_point(Beats::from_f32(10.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        let clip = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(8.0),
            Beats::from_f32(8.0), // 8..16
            Seconds(0.0),
            Seconds(120.0),
        );

        let bp = audio_waveform_breakpoints(&clip, &project);
        assert_eq!(bp.len(), 3, "one tempo point strictly inside the clip → 3 breakpoints");

        // x_frac of the tempo-change beat (10) within the clip (8..16): 2/8 = 0.25.
        assert!((bp[1].0 - 0.25).abs() < 1e-6, "tempo point x_frac, got {}", bp[1].0);

        let start_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(8.0), Bpm(120.0)).0;
        let mid_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(10.0), Bpm(120.0)).0;
        let end_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(16.0), Bpm(120.0)).0;

        // recorded_bpm unset → warp_ratio == 1.0, in_point == 0 → file_secs is
        // exactly the tempo-map-integrated elapsed seconds since clip start.
        assert!((bp[0].1 - 0.0).abs() < 1e-6);
        assert!(
            (bp[1].1 - ((mid_secs - start_secs) as f32)).abs() < 1e-4,
            "mid breakpoint file_secs {} vs tempo-map delta {}",
            bp[1].1,
            (mid_secs - start_secs) as f32
        );
        assert!(
            (bp[2].1 - ((end_secs - start_secs) as f32)).abs() < 1e-4,
            "end breakpoint file_secs {} vs tempo-map delta {}",
            bp[2].1,
            (end_secs - start_secs) as f32
        );
    }

    /// (c) `in_point` must offset every breakpoint's `file_secs` uniformly —
    /// it enters the formula as a flat additive term, exactly like
    /// `AudioLayerPlayback`'s `expected = (now - clip_start) * ratio +
    /// clip.in_point`.
    #[test]
    fn in_point_offsets_every_breakpoint() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);

        let clip_no_offset = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(0.0),
            Beats::from_f32(4.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_with_offset = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(0.0),
            Beats::from_f32(4.0),
            Seconds(3.0),
            Seconds(120.0),
        );

        let bp_plain = audio_waveform_breakpoints(&clip_no_offset, &project);
        let bp_offset = audio_waveform_breakpoints(&clip_with_offset, &project);
        assert_eq!(bp_plain.len(), bp_offset.len());
        for (plain, offset) in bp_plain.iter().zip(bp_offset.iter()) {
            assert!((plain.0 - offset.0).abs() < 1e-6, "x_frac must be unaffected by in_point");
            assert!(
                (offset.1 - (plain.1 + 3.0)).abs() < 1e-4,
                "file_secs must be shifted by exactly in_point: {} vs {} + 3.0",
                offset.1,
                plain.1
            );
        }
    }

    /// Non-audio clips (no `audio_file_path`) get no breakpoints — the
    /// waveform painter is never invoked for them.
    #[test]
    fn non_audio_clip_yields_no_breakpoints() {
        let project = Project::default();
        let clip = TimelineClip::new_generator(Beats::ZERO, Beats::from_f32(4.0));
        assert!(audio_waveform_breakpoints(&clip, &project).is_empty());
    }
}
