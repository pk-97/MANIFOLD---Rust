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

use crate::ui_bridge::projection::cards::write_macro_mapping;
use std::fmt::Write as _;

pub use crate::ui_bridge::projection::cards::sync_card_values;
pub use crate::ui_bridge::projection::driver_readouts::sync_driver_readouts;
pub use crate::ui_bridge::projection::inspector::{sync_audio_dock, sync_inspector_data};
pub use crate::ui_bridge::projection::scene::{sync_scene_outliner_eyes, sync_scene_row_values};
pub use crate::ui_bridge::projection::timeline::{sync_automation_lane_geometry, sync_clip_positions, sync_markers, sync_project_data, sync_track_layout};
pub use crate::ui_bridge::projection::transport::{TransportDisplayCache, check_auto_scroll};

// Transport colors for play state.
const PLAY_GREEN: Color32 = Color32::new(56, 115, 66, 255);
const PLAY_ACTIVE: Color32 = Color32::new(64, 184, 82, 255);
const PAUSED_YELLOW: Color32 = Color32::new(209, 166, 38, 255);

fn graph_edit_diagnostic_toast(
    last_sequence: Option<u64>,
    diagnostic: Option<&crate::content_state::GraphEditDiagnostic>,
) -> Option<(u64, &str)> {
    diagnostic
        .filter(|event| last_sequence != Some(event.sequence))
        .map(|event| (event.sequence, event.message.as_str()))
}

fn trigger_delivery_diagnostic_toast(
    last: &mut Option<manifold_playback::engine::trigger_delivery::TriggerDeliveryFailure>,
    current: Option<manifold_playback::engine::trigger_delivery::TriggerDeliveryFailure>,
) -> Option<String> {
    let failure = current?;
    if *last == Some(failure) {
        return None;
    }
    *last = Some(failure);
    Some(failure.to_string())
}

/// Who holds the one toast slot this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastSlot {
    Free,
    /// The scene impulse toast is in flight.
    Ours,
    /// Another toast (undo, export, a graph edit) is in flight.
    Other,
}

/// Scene impulse refusals and discards the stage has shown
/// (BUG-mo4oa (scene force trigger skipped)). The counts only grow, so a
/// lower snapshot (a bare notice) changes nothing. Increases coalesce into
/// the impulse toast in flight; while another toast holds the slot they wait,
/// so an audio-rate stream never flickers or buries that toast.
#[derive(Debug, Default)]
pub(crate) struct SceneImpulseToast {
    /// Highest (refused, discarded) counts seen.
    seen: (u64, u64),
    /// Increases waiting for the slot.
    pending: (u64, u64),
    /// Counts the impulse toast in flight shows.
    shown: (u64, u64),
    /// The impulse toast's text, to tell it from another toast.
    message: String,
}

impl SceneImpulseToast {
    fn slot(&self, toast: &manifold_ui::panels::toast::ToastPanel) -> ToastSlot {
        match toast.pending() {
            None => ToastSlot::Free,
            Some((message, _)) if message == self.message => ToastSlot::Ours,
            Some(_) => ToastSlot::Other,
        }
    }

    /// Fold one snapshot in. Returns the text to show and whether it is a
    /// refusal (red): refusals first, with the newest reason, else discards.
    fn update(
        &mut self,
        current: &manifold_core::scene_impulse::SceneImpulseDiagnostics,
        slot: ToastSlot,
    ) -> Option<(&str, bool)> {
        self.pending.0 += current.refused.saturating_sub(self.seen.0);
        self.pending.1 += current.discarded.saturating_sub(self.seen.1);
        self.seen.0 = self.seen.0.max(current.refused);
        self.seen.1 = self.seen.1.max(current.discarded);
        if self.pending == (0, 0) {
            return None;
        }
        match slot {
            ToastSlot::Other => return None,
            ToastSlot::Ours => {
                self.shown.0 += self.pending.0;
                self.shown.1 += self.pending.1;
            }
            ToastSlot::Free => self.shown = self.pending,
        }
        self.pending = (0, 0);
        let (refused, discarded) = self.shown;
        self.message.clear();
        if refused > 0 {
            let reason = current.last_refusal.as_deref().unwrap_or("Impulse: a hit was refused");
            self.message.push_str(reason);
            if refused > 1 {
                let _ = write!(self.message, " ({refused} hits refused)");
            }
        } else if discarded == 1 {
            self.message.push_str("Impulse discarded: the simulation is paused or at Speed 0");
        } else {
            let _ = write!(self.message, "{discarded} impulses discarded: the simulation is paused or at Speed 0");
        }
        Some((&self.message, refused > 0))
    }
}

/// Push engine state into UI panels (called once per frame, AFTER build):
/// the event drain, the chrome push, the selection push and the live push
/// in that order (UI_INVALIDATION_DESIGN.md D15). The frame seam calls the
/// pieces itself so the selection push runs only on its inputs.
#[cfg(feature = "ui-snapshot")]
#[allow(clippy::too_many_arguments)]
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
    intake_overlay_state(ui, content_state);
    drain_events(ui, content_state);
    push_chrome(ui, project, content_state, selection, is_dirty, project_path, transport_cache);
    push_clock(ui, project, content_state, transport_cache);
    push_selection(ui, project, active_layer, selection);
    push_frame(ui, project, content_state, active_layer);
    push_live(ui, project, content_state, active_layer, LiveGroups { values: true, shapes: true });
}

/// One-shot events keyed on their own sequences: toasts and content-driven
/// selections. Runs every frame, after the build, since the selections find
/// cards this frame's projection made.
pub fn drain_events(ui: &mut UIRoot, content_state: &crate::content_state::ContentState) {
    // D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — real command
    // label instead of the generic "Undo"/"Redo" the M::Undo/M::Redo menu
    // handlers used to show directly (`app_render.rs`). Keyed on
    // `data_version` (bumped by every
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

    // Rejected graph edits do not change project data, so they carry an
    // independent sequence. Keep the diagnostic persistent in the content
    // snapshot and show each sequence exactly once.
    if let Some((sequence, message)) = graph_edit_diagnostic_toast(
        ui.last_graph_edit_diagnostic_sequence,
        content_state.graph_edit_diagnostic.as_deref(),
    ) {
        ui.toast.show_with_accent(message.to_owned(), color::RED_BASE);
        ui.last_graph_edit_diagnostic_sequence = Some(sequence);
    }

    if let Some(message) = trigger_delivery_diagnostic_toast(
        &mut ui.last_trigger_delivery_failure,
        content_state.trigger_delivery_failure,
    ) {
        ui.toast.show_with_accent(message, color::RED_BASE);
    }

    let slot = ui.scene_impulse_toast.slot(&ui.toast);
    if let Some((message, refused)) = ui.scene_impulse_toast.update(&content_state.scene_impulses, slot) {
        ui.toast.refresh(message.to_owned(), refused.then_some(color::RED_BASE));
        if slot == ToastSlot::Ours {
            // New text in an open toast: the overlays rebuild it.
            ui.mark_overlay_dirty();
        }
    }

    let tree = &mut ui.tree;
    if let Some(update) = &content_state.modifier_selection_update {
        if ui.last_modifier_selection_sequence != Some(update.sequence) {
            if let Some(id) = update.ids.iter().rev().find(|id| {
                ui.scene_setup_panel.force_card_info(id)
                    .is_some_and(|info| info.layer_id == update.layer_id)
            }) {
                ui.scene_setup_panel.set_selection(update.layer_id.clone(),
                    manifold_ui::panels::scene_setup_panel::SceneSelection::Force(id.clone()));
            }
            ui.inspector.select_modifier_ids(&update.layer_id, &update.ids);
            ui.inspector.apply_selection_visuals(tree);
            ui.inspector.reveal_pending_selection(tree);
            ui.last_modifier_selection_sequence = Some(update.sequence);
        }
    } else {
        ui.last_modifier_selection_sequence = None;
    }

    if let Some(update) = &content_state.object_modifier_selection_update {
        if ui.last_object_modifier_selection_sequence != Some(update.sequence) {
            ui.scene_setup_panel.select_object_modifier_by_address(
                &update.layer_id, update.owner_id, update.node_doc_id, tree,
            );
            ui.last_object_modifier_selection_sequence = Some(update.sequence);
        }
    } else {
        ui.last_object_modifier_selection_sequence = None;
    }
}

/// Content-driven overlay state, taken before the frame is planned so a
/// changed overlay moves the overlays' view key this frame (UI_INVALIDATION
/// D6). Taken after the build it would show one frame late.
pub fn intake_overlay_state(ui: &mut UIRoot, content_state: &crate::content_state::ContentState) {
    // MIDI device names and the Ableton session, taken by pointer.
    let session_changed = ui.intake_shared(content_state);

    // A new Ableton session refreshes the open picker.
    if session_changed
        && ui.ableton_picker.is_open()
        && let Some(session) = ui.ableton_session.clone()
    {
        ui.ableton_picker
            .update_session(crate::ui_root::build_picker_session(&session));
        ui.mark_overlay_dirty();
    }

    ui.setup_fits.clone_from(&content_state.setup_fits);

    // Runtime-only presentation state rides every ContentState, including
    // paused command snapshots; it must not wait for a project-version change.
    if let Some(sdr_preview) = content_state.sdr_preview
        && ui.settings_popup.set_sdr_preview(sdr_preview)
    {
        ui.mark_overlay_dirty();
    }
}

/// The header clock: time first, then bar.beat.sixteenth. Writes its label in
/// place without allocating while the shown digits move; the digits are
/// `frame_plan::clock_digits`, the planner's key for this push.
pub fn push_clock(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
    transport_cache: &mut TransportDisplayCache,
) {
    let beat = content_state.current_beat.as_f32();
    let t = content_state.current_time.0;
    let mins = (t / 60.0).floor() as i32;
    let secs = (t % 60.0).floor() as i32;
    let tenths = ((t * 10.0) % 10.0).floor() as i32;
    let bpb = (project.settings.time_signature_numerator.max(1)) as f32;
    let bar = (beat / bpb).floor() as i32 + 1;
    let beat_in_bar = (beat % bpb).floor() as i32 + 1;
    let sixteenth = ((beat % 1.0) * 4.0).floor() as i32 + 1;
    let display = transport_cache.time_display(mins, secs, tenths, bar, beat_in_bar, sixteenth);
    ui.header.set_time_display(&mut ui.tree, display);
}

/// Transport, sync, header, footer, settings and layer-header chrome.
#[allow(clippy::too_many_arguments)]
pub fn push_chrome(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
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

        // Automation globals (P4, docs/AUTOMATION_LANES_DESIGN.md section 4/section 5/section 7):
        // ARM mirrors the runtime arm flag; BACK lights red exactly when any
        // lane override latch is active (Live's Back to Arrangement).
        ui.transport.set_automation_state(
            content_state.automation_armed,
            !content_state.automation_latched_params.is_empty(),
        );
        // LANES button (view-only toggle, no content-thread state behind it).
        ui.transport
            .set_automation_mode_visible(selection.automation_mode_visible());
        ui.transport.set_automation_draw_mode(selection.automation_draw_mode);

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
        let decimals = if ppb >= 1.0 { 0 } else { 2 };
        ui.header.set_zoom_label(&format!("{ppb:.decimals$} px/beat"));

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
        ui.settings_popup.sync_sim_rate(tree, project.settings.physics.sim_rate);
        ui.settings_popup.set_resolution_text(&res_label);
        ui.settings_popup
            .set_render_scale(project.settings.render_scale);
        ui.settings_popup
            .set_tonemap_curve(crate::ui_translate::tonemap_curve_to_ui(
                project.settings.tonemap_curve,
            ));
        ui.settings_popup
            .set_tonemap_enabled(project.settings.tonemap_enabled);
        ui.settings_popup.set_hdr(project.settings.export_hdr);
        ui.settings_popup
            .set_split_sections(project.settings.split_at_markers);

        // RT Quality panel — feed the current settings
        ui.rt_quality_panel.configure(project.settings.rt_quality);
    }

    // Footer stats
    {
        let layers = project.timeline.layers.len();
        let clips: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let info = format!("Layers: {} | Clips: {}", layers, clips);
        ui.footer.set_selection_info(&info);
    }

    {
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            ui.layer_headers.set_mute_state(tree, i, layer.is_muted);
            ui.layer_headers.set_solo_state(tree, i, layer.is_solo);
            // routes_to_led (not the raw flag): an LED-type layer always feeds
            // the strips, so its (indicator, non-toggle) L chip stays truthfully
            // on — blit_to_led is inert on LED layers (LED_STRIPS_DESIGN.md D11).
            ui.layer_headers.set_led_state(tree, i, layer.routes_to_led());
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
    if let Some(clip_id) = selection.primary_selected_clip_id() {
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
                // Warp state is independent of remembered source tempo.
                chrome.sync_warp_enabled(tree, clip.is_audio_warp_enabled());
                // Source BPM remains visible while Warp is off.
                if clip.recorded_bpm > 0.0 {
                    chrome.sync_bpm(tree, &format!("{:.1}", clip.recorded_bpm));
                } else {
                    chrome.sync_bpm(tree, "Unknown");
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
}

/// Insert cursor, selection band, selected clips and layer highlight. Runs
/// when the selection, cursor or active layer moved, or after a projection
/// or rebuild replaced the nodes it writes.
pub fn push_selection(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) {
    // Selection → viewport (version-gated to avoid per-frame Vec allocation).
    // The panel's `selected_clip_ids` is a pure render cache fed from the enum
    // here — never an independent authority.
    ui.viewport.sync_selection(
        selection.selection_visual_version(),
        || selection.get_selected_clip_ids(),
        || selection.selected_marker_ids().iter().cloned().collect(),
    );
    if let Some(beat) = selection.insert_cursor_beat() {
        ui.viewport.set_insert_cursor(beat);
    }

    // Region → viewport (sync from UIState so clearing via set_insert_cursor
    // propagates). Only a `TimeRange` selection has a region; a `Clips`
    // selection pushes `None`, so no band draws behind a clip selection (D1).
    // A region whose edge layers are gone covers no layer, so it draws no band.
    let band = selection.current_region().and_then(|r| {
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        r.layer_index_range(&ui_layers).map(|(start, end)| (r, start, end))
    });
    if let Some((r, start_layer, end_layer)) = band {
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
    // section 19 timeline echo: the focused lane lifts in the viewport body too (track
    // index == layer index, the `tracks` vec is built 1:1 from project layers).
    ui.viewport.set_active_track_index(active_layer);
}

/// Every frame: the playhead and playing state (immediate-pass inputs) and
/// the LED composite preview band, whose card compares the readback version
/// itself; the structural presence rides `sync_inspector_data`.
pub fn push_frame(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
    active_layer: Option<usize>,
) {
    let playhead_beat = content_state.current_beat.as_f32();
    ui.viewport.set_playhead(Beats::from_f32(playhead_beat));
    ui.viewport.set_playing(content_state.is_playing);
    if let Some(layer) = active_layer.and_then(|i| project.timeline.layers.get(i)) {
        let is_dmx = layer.layer_type == manifold_core::types::LayerType::Dmx;
        let band = super::projection::inspector::map_led_preview(content_state.led_preview.as_ref(), is_dmx);
        ui.inspector.sync_led_preview(&mut ui.tree, band.as_ref());
    }
}

/// Which groups of the live push run (D22). Values displays follow the values
/// sequence and the Values stamp; drawers, handles, gains and range bands
/// follow the Shapes and MappingRanges stamps, so modulated playback never
/// touches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveGroups {
    pub values: bool,
    pub shapes: bool,
}

impl LiveGroups {
    pub fn any(self) -> bool {
        self.values || self.shapes
    }
}

/// The live push (D15 piece 1, D22): every display of a live scalar, in
/// place. The frame planner runs each group when its inputs moved, and both
/// after a projection or rebuild.
pub fn push_live(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
    active_layer: Option<usize>,
    groups: LiveGroups,
) {
    manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::ValuePush);
    if groups.values {
        push_live_values(ui, project, content_state, active_layer);
    }
    if groups.shapes {
        crate::ui_bridge::projection::live_shapes::sync_live_shapes(ui, project, active_layer);
    }
}

/// Write a macro's Ableton mapping into the display the macros panel builds
/// from, reusing the held strings; gaining a mapping is the one allocation.
fn sync_ableton_display(
    held: &mut Option<AbletonMappingDisplay>,
    mapping: Option<&manifold_core::ableton_mapping::AbletonParamMapping>,
) {
    let Some(m) = mapping else {
        *held = None;
        return;
    };
    let status = crate::ui_translate::ableton_mapping_status_to_ui(m.status);
    match held {
        Some(d) => {
            d.macro_name.clone_from(&m.address.macro_name);
            d.track_name.clone_from(&m.address.track_name);
            d.device_name.clone_from(&m.address.device_name);
            d.status = status;
            d.inverted = m.inverted;
        }
        None => {
            *held = Some(AbletonMappingDisplay {
                macro_name: m.address.macro_name.clone(),
                track_name: m.address.track_name.clone(),
                device_name: m.address.device_name.clone(),
                status,
                inverted: m.inverted,
            });
        }
    }
}

/// Values displays: macro, opacity and LED values, card and scene-row values.
fn push_live_values(
    ui: &mut UIRoot,
    project: &Project,
    content_state: &crate::content_state::ContentState,
    active_layer: Option<usize>,
) {
    let tree = &mut ui.tree;

    // Macro slider values and labels, and the labels and mapping
    // descriptions the context menus read. Every string is rewritten in
    // place, so a push allocates nothing once each has grown to its text.
    let slots = &project.settings.macro_bank.slots;
    let shown = slots.len().min(manifold_core::MACRO_COUNT);
    let mut macro_vals = [0.0; manifold_core::MACRO_COUNT];
    let abl_displays = ui.inspector.macros_panel_mut().ableton_displays_mut();
    for (i, slot) in slots.iter().take(shown).enumerate() {
        macro_vals[i] = slot.value;
        // The slider label carries the [ABL] suffix; `macro_labels` keeps
        // the raw label for the menus.
        let label = &mut ui.macro_display_labels[i];
        label.clear();
        if slot.label.is_empty() {
            let _ = write!(label, "M{}", i + 1);
        } else {
            label.push_str(&slot.label);
        }
        if let Some(mapping) = &slot.ableton_mapping {
            use manifold_core::ableton_mapping::AbletonMappingStatus;
            label.push_str(match mapping.status {
                AbletonMappingStatus::Active => " [ABL]",
                AbletonMappingStatus::Dormant => " [ABL-]",
                AbletonMappingStatus::Ambiguous => " [ABL?]",
            });
        }
        sync_ableton_display(&mut abl_displays[i], slot.ableton_mapping.as_ref());
        ui.macro_labels[i].clone_from(&slot.label);
        let descs = &mut ui.macro_mapping_descs[i];
        descs.resize_with(slot.mappings.len(), String::new);
        for (desc, mapping) in descs.iter_mut().zip(&slot.mappings) {
            write_macro_mapping(desc, &mapping.target, project);
        }
        ui.macro_ableton_mapped[i] = slot.ableton_mapping.is_some();
    }
    ui.inspector
        .macros_panel_mut()
        .sync_values(tree, &macro_vals[..shown], &ui.macro_display_labels[..shown]);

    // Active layer opacity and name in the layer chrome.
    if let Some(layer) = active_layer.and_then(|idx| project.timeline.layers.get(idx)) {
        ui.inspector
            .layer_chrome_mut()
            .sync_opacity(tree, layer.opacity);
        ui.inspector.layer_chrome_mut().sync_name(tree, &layer.name);
    }
    // The master chrome shows with or without an active layer.
    ui.inspector
        .master_chrome_mut()
        .sync_opacity(tree, project.settings.master_opacity);
    ui.inspector
        .master_chrome_mut()
        .sync_led_brightness(tree, project.settings.led_brightness);
    ui.inspector
        .master_chrome_mut()
        .sync_led_enabled(tree, content_state.led_enabled);
    super::write_led_exit_path_label(
        &mut ui.led_exit_label,
        project.settings.led_exit_index,
        &project.settings.master_effects,
    );
    ui.inspector
        .master_chrome_mut()
        .sync_exit_path(tree, &ui.led_exit_label);

    // Sync effect card values (master, layer, clip)
    sync_card_values(ui, project, active_layer);

    // Sync Scene Setup row values (same per-frame value plane, dock rows)
    sync_scene_row_values(ui, project);
}

#[cfg(test)]
mod tests {
    use super::graph_edit_diagnostic_toast;
    use crate::content_state::GraphEditDiagnostic;

    /// Every refused or discarded scene hit reaches the stage as a toast,
    /// once per increase; a lower snapshot never repeats one
    /// (BUG-mo4oa (scene force trigger skipped)).
    #[test]
    fn scene_impulse_toast_shows_each_refusal_and_discard_once() {
        use super::ToastSlot::Free;
        use manifold_core::scene_impulse::SceneImpulseDiagnostics;
        let mut toast = super::SceneImpulseToast::default();
        let mut counts = SceneImpulseDiagnostics::default();
        assert_eq!(toast.update(&counts, Free), None);
        counts.discarded = 1;
        let (message, refused) = toast.update(&counts, Free).unwrap();
        assert!(!refused && message.starts_with("Impulse discarded"), "{message}");
        assert_eq!(toast.update(&counts, Free), None, "shown once");
        counts.refuse("Physics: 256 hits are already in flight; this hit is refused");
        counts.refuse("Physics: 256 hits are already in flight; this hit is refused");
        let (message, refused) = toast.update(&counts, Free).unwrap();
        assert!(refused && message.ends_with("(2 hits refused)"), "{message}");
        assert_eq!(toast.update(&SceneImpulseDiagnostics::default(), Free), None);
        assert_eq!(toast.update(&counts, Free), None, "a bare notice never re-shows");
    }

    /// Audio-rate refusals coalesce into the impulse toast in flight (its
    /// count grows, in place) and wait while another toast holds the slot,
    /// so they never flicker or bury it.
    #[test]
    fn scene_impulse_toast_coalesces_and_never_buries_another_toast() {
        use super::ToastSlot::{Free, Other, Ours};
        use manifold_core::scene_impulse::SceneImpulseDiagnostics;
        use manifold_ui::panels::toast::ToastPanel;
        let reason = "Impulse: the hit waited more than 100 ms for its scene; this hit is refused";
        let mut toast = super::SceneImpulseToast::default();
        let mut panel = ToastPanel::new();
        let mut counts = SceneImpulseDiagnostics::default();
        counts.refuse(reason);
        assert_eq!(toast.slot(&panel), Free);
        let (message, _) = toast.update(&counts, Free).unwrap();
        panel.refresh(message.to_owned(), None);
        assert_eq!(toast.slot(&panel), Ours, "the toast in flight is the impulse toast");
        counts.refuse(reason);
        counts.refuse(reason);
        let (message, _) = toast.update(&counts, Ours).unwrap();
        assert!(message.ends_with("(3 hits refused)"), "the count grows in place: {message}");
        panel.refresh(message.to_owned(), None);

        panel.show("Export complete");
        assert_eq!(toast.slot(&panel), Other);
        counts.refuse(reason);
        assert_eq!(toast.update(&counts, Other), None, "another toast is never buried");
        assert_eq!(panel.pending().map(|(message, _)| message), Some("Export complete"));
        let (message, refused) = toast.update(&counts, Free).unwrap();
        assert_eq!((message, refused), (reason, true), "the waiting refusal shows once the slot is free");
    }

    #[test]
    fn graph_edit_diagnostic_toast_is_sequence_gated() {
        let first = GraphEditDiagnostic {
            sequence: 4,
            message: "Cannot remove connected node".into(),
        };
        assert_eq!(
            graph_edit_diagnostic_toast(None, Some(&first)),
            Some((4, "Cannot remove connected node"))
        );
        assert_eq!(graph_edit_diagnostic_toast(Some(4), Some(&first)), None);

        let newer = GraphEditDiagnostic {
            sequence: 5,
            message: "Cannot remove connected node".into(),
        };
        assert_eq!(
            graph_edit_diagnostic_toast(Some(4), Some(&newer)),
            Some((5, "Cannot remove connected node"))
        );
    }

    #[test]
    fn trigger_delivery_diagnostic_is_shown_once_across_partial_snapshots_and_new_epochs() {
        use manifold_playback::engine::trigger_delivery::{TriggerDeliveryError, TriggerDeliveryFailure};
        let failure = TriggerDeliveryFailure { epoch: 4, kind: TriggerDeliveryError::CapacityOverflow };
        let mut last = None;
        let message = super::trigger_delivery_diagnostic_toast(&mut last, Some(failure)).unwrap();
        assert_eq!(message, failure.to_string());
        assert!(super::trigger_delivery_diagnostic_toast(&mut last, Some(failure)).is_none());
        assert!(super::trigger_delivery_diagnostic_toast(&mut last, None).is_none());
        assert_eq!(last, Some(failure));
        assert!(super::trigger_delivery_diagnostic_toast(&mut last, Some(failure)).is_none());
        let newer = TriggerDeliveryFailure { epoch: 5, ..failure };
        assert!(super::trigger_delivery_diagnostic_toast(&mut last, Some(newer)).is_some());
        assert!(super::trigger_delivery_diagnostic_toast(&mut last, Some(newer)).is_none());
    }
}
