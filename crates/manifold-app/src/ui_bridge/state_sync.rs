//! Per-frame state push: `push_state` drains engine/project state into the UI
//! tree, delegating each domain to its projection module (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).
use manifold_core::Beats;
use manifold_core::PresetTypeId;
use manifold_core::project::Project;
use manifold_ui::color;
use manifold_ui::node::Color32;
use manifold_ui::panels::param_slider_shared::AbletonMappingDisplay;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

use crate::ui_bridge::projection::cards::describe_macro_mapping;

pub use crate::ui_bridge::projection::cards::sync_card_values;
pub use crate::ui_bridge::projection::inspector::sync_inspector_data;
pub use crate::ui_bridge::projection::scene::sync_scene_row_values;
pub use crate::ui_bridge::projection::timeline::{sync_clip_positions, sync_project_data};
pub use crate::ui_bridge::projection::transport::{TransportDisplayCache, check_auto_scroll};

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
const PAUSED_YELLOW: Color32 = Color32::new(209, 166, 38, 255);

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
