//! Transport bar business logic: sync controller management, BPM editing,
//! clock authority, playback actions.
//!
//! Pure logic — no UI. The app layer bridges this to the bitmap TransportPanel.
//! Mechanical translation of Unity TransportController.cs.

use manifold_core::project::Project;
use manifold_core::types::ClockAuthority;

use crate::engine::PlaybackEngine;
use crate::link_sync::LinkSyncController;
use crate::midi_clock_sync::MidiClockSyncController;

/// Insert cursor state needed by transport actions.
/// Mirrors the subset of UIState that TransportController reads.
pub struct InsertCursorState {
    pub has_insert_cursor: bool,
    pub insert_cursor_beat: f32,
}

/// Transport controller — orchestrates sync, BPM, and playback actions.
/// Port of Unity TransportController.cs.
pub struct TransportController {
    // Sync sources — concrete types so we can call their typed update() methods.
    pub link_sync: Option<LinkSyncController>,
    pub midi_clock_sync: Option<MidiClockSyncController>,
    /// Whether the OSC sync controller is authority-enabled.
    /// The OscSyncController itself lives on ContentThread as a sibling.
    pub osc_enabled: bool,
    pub osc_sender_enabled: bool,
    pub osc_sender_port: i32,
}

impl TransportController {
    pub fn new() -> Self {
        Self {
            link_sync: Some(LinkSyncController::new()),
            midi_clock_sync: Some(MidiClockSyncController::new()),
            osc_enabled: false,
            osc_sender_enabled: false,
            osc_sender_port: 9001,
        }
    }

    // ── Clock authority ──────────────────────────────────────────────

    pub fn get_clock_authority(project: Option<&Project>) -> ClockAuthority {
        project
            .map(|p| p.settings.clock_authority)
            .unwrap_or(ClockAuthority::Internal)
    }

    /// Cycle authority: Internal→Link→MidiClock→Osc→Internal.
    /// Then apply exclusively — enable the new authority, disable others.
    pub fn cycle_authority(&mut self, engine: &mut PlaybackEngine) {
        let authority = {
            let project = engine.project();
            let current = Self::get_clock_authority(project);
            current.next()
        };
        self.apply_authority_exclusively(engine, authority);
        log::info!("[TransportController] Clock authority: {:?}", authority);
    }

    /// Set clock authority and enforce exclusivity.
    /// Port of Unity TransportController.ApplyAuthorityExclusively.
    pub fn apply_authority_exclusively(&mut self, engine: &mut PlaybackEngine, authority: ClockAuthority) {
        // Set authority on project settings
        if let Some(project) = engine.project_mut() {
            project.settings.clock_authority = authority;
        }

        if authority == ClockAuthority::Internal {
            self.disable_non_authority_sources(ClockAuthority::Internal);
        } else {
            self.enable_authority_source(authority);
            self.disable_non_authority_sources(authority);

            // Fail-safe: if the source failed to enable, revert to Internal
            if !self.is_authority_source_enabled(authority) {
                if let Some(project) = engine.project_mut() {
                    project.settings.clock_authority = ClockAuthority::Internal;
                }
                self.disable_non_authority_sources(ClockAuthority::Internal);
                log::warn!("[TransportController] Failed to enable {:?}; reverted to Internal.", authority);
            }
        }

        // Disable recording when OSC is authority (can't record during performance sync)
        if authority == ClockAuthority::Osc {
            engine.set_recording(false);
        }

        // Clear external time sync on authority change
        engine.set_external_time_sync(false);
    }

    fn disable_non_authority_sources(&mut self, authority: ClockAuthority) {
        match authority {
            ClockAuthority::Link => {
                // Disable MIDI Clock and OSC; keep Link
                if let Some(ref mut clk) = self.midi_clock_sync {
                    if clk.is_midi_clock_enabled() { clk.disable_midi_clock(); }
                }
                self.osc_enabled = false;
            }
            ClockAuthority::MidiClock => {
                // Disable OSC; keep Link optional (background peer awareness)
                self.osc_enabled = false;
            }
            ClockAuthority::Osc => {
                // Disable MIDI Clock; keep Link optional
                if let Some(ref mut clk) = self.midi_clock_sync {
                    if clk.is_midi_clock_enabled() { clk.disable_midi_clock(); }
                }
            }
            ClockAuthority::Internal => {
                // Disable MIDI Clock and OSC; keep Link optional
                if let Some(ref mut clk) = self.midi_clock_sync {
                    if clk.is_midi_clock_enabled() { clk.disable_midi_clock(); }
                }
                self.osc_enabled = false;
            }
        }
    }

    fn is_authority_source_enabled(&self, authority: ClockAuthority) -> bool {
        match authority {
            ClockAuthority::Link => self.link_sync.as_ref().map_or(false, |s| s.is_link_enabled()),
            ClockAuthority::MidiClock => self.midi_clock_sync.as_ref().map_or(false, |s| s.is_midi_clock_enabled()),
            ClockAuthority::Osc => self.osc_enabled,
            ClockAuthority::Internal => true,
        }
    }

    fn enable_authority_source(&mut self, authority: ClockAuthority) {
        match authority {
            ClockAuthority::Link => {
                if let Some(ref mut link) = self.link_sync {
                    link.enable_link(120.0);
                }
            }
            ClockAuthority::MidiClock => {
                if let Some(ref mut clk) = self.midi_clock_sync {
                    clk.enable_midi_clock(clk.selected_source_index());
                }
            }
            ClockAuthority::Osc => {
                // OscSyncController enable is handled by ContentThread
                // which has access to the OscReceiver and OscSyncController.
                self.osc_enabled = true;
            }
            ClockAuthority::Internal => {}
        }
    }

    // ── Transport actions ────────────────────────────────────────────

    /// Toggle play/pause. If stopped/paused: seek to insert cursor, then play.
    /// Port of Unity TransportController.TogglePlayPause.
    pub fn toggle_play_pause(&self, engine: &mut PlaybackEngine, cursor: &InsertCursorState) {
        if engine.is_playing() {
            engine.pause();
        } else {
            if cursor.has_insert_cursor {
                let time = engine.beat_to_timeline_time(cursor.insert_cursor_beat);
                engine.seek_to(time);
            }
            engine.play();
        }
    }

    /// Stop playback. Seek to insert cursor if set.
    /// Port of Unity TransportController.StopPlayback.
    pub fn stop_playback(&self, engine: &mut PlaybackEngine, cursor: &InsertCursorState) {
        engine.stop();
        if cursor.has_insert_cursor {
            let time = engine.beat_to_timeline_time(cursor.insert_cursor_beat);
            engine.seek_to(time);
        }
    }

    /// Toggle recording state.
    pub fn toggle_record(&self, engine: &mut PlaybackEngine) {
        engine.set_recording(!engine.is_recording());
    }

    // ── Sync toggle actions ──────────────────────────────────────────

    /// Toggle Ableton Link on/off. If disabling and Link was authority, revert to Internal.
    /// Port of Unity TransportController.ToggleLink.
    pub fn toggle_link(&mut self, engine: &mut PlaybackEngine) {
        if let Some(ref mut link) = self.link_sync {
            if link.is_link_enabled() {
                link.disable_link();
                let auth = Self::get_clock_authority(engine.project());
                if auth == ClockAuthority::Link {
                    self.apply_authority_exclusively(engine, ClockAuthority::Internal);
                }
            } else {
                let bpm = engine.project().map_or(120.0, |p| p.settings.bpm as f64);
                link.enable_link(bpm);
            }
        } else {
            log::info!("[TransportController] Link sync not available");
        }
    }

    /// Toggle MIDI Clock. If disabling and CLK was authority, revert to Internal.
    /// If enabling, takes over authority.
    /// Port of Unity TransportController.ToggleMidiClock.
    pub fn toggle_midi_clock(&mut self, engine: &mut PlaybackEngine) {
        if let Some(ref mut clk) = self.midi_clock_sync {
            if clk.is_midi_clock_enabled() {
                clk.disable_midi_clock();
                let auth = Self::get_clock_authority(engine.project());
                if auth == ClockAuthority::MidiClock {
                    self.apply_authority_exclusively(engine, ClockAuthority::Internal);
                }
            } else {
                self.apply_authority_exclusively(engine, ClockAuthority::MidiClock);
            }
        } else {
            log::info!("[TransportController] MIDI Clock sync not available");
        }
    }

    /// Toggle OSC sync output (OscPositionSender).
    pub fn toggle_sync_output(&mut self, engine: &mut PlaybackEngine) {
        if self.osc_sender_enabled {
            self.osc_sender_enabled = false;
            log::info!("[TransportController] SYNC output disabled");
        } else {
            let port = engine.project()
                .map(|p| p.settings.osc_send_port)
                .unwrap_or(9001);
            self.osc_sender_port = port;
            self.osc_sender_enabled = true;
            log::info!("[TransportController] SYNC output enabled on port {}", port);
        }
    }

    // ── BPM editing ──────────────────────────────────────────────────

    /// Set BPM from text input. Parse, clamp [20,300], create command.
    /// Port of Unity TransportController.SetBpm.
    pub fn set_bpm(engine: &mut PlaybackEngine, editing: &mut manifold_editing::service::EditingService, value: &str) {
        let new_bpm = match value.parse::<f32>() {
            Ok(v) => v.clamp(20.0, 300.0),
            Err(_) => return,
        };

        if let Some(project) = engine.project_mut() {
            let old_bpm = project.settings.bpm;
            if (old_bpm - new_bpm).abs() < 0.01 { return; }

            let bpm_cmd = manifold_editing::commands::settings::ChangeBpmCommand::new(old_bpm, new_bpm);

            // Build rescale command (proportionally moves clip positions)
            let rescale_cmd = manifold_editing::commands::settings::RescaleBeatsForBpmChangeCommand::build(
                project, old_bpm, new_bpm,
            );

            if let Some(rescale) = rescale_cmd {
                let commands: Vec<Box<dyn manifold_editing::command::Command>> = vec![
                    Box::new(bpm_cmd),
                    Box::new(rescale),
                ];
                let composite = manifold_editing::command::CompositeCommand::new(
                    commands,
                    format!("Change BPM {:.1} → {:.1}", old_bpm, new_bpm),
                );
                editing.execute(Box::new(composite), project);
            } else {
                editing.execute(Box::new(bpm_cmd), project);
            }
        }
    }

    /// Reset BPM to recorded value (tempo lane or recorded project BPM).
    /// Port of Unity TransportController.ResetBpm.
    pub fn reset_bpm(engine: &mut PlaybackEngine, editing: &mut manifold_editing::service::EditingService) {
        if let Some(project) = engine.project_mut() {
            // Try recorded tempo lane first
            if !project.recording_provenance.recorded_tempo_lane.is_empty() {
                let old_bpm = project.settings.bpm;
                let old_points = project.tempo_map.clone_points();
                let new_points = project.recording_provenance.recorded_tempo_lane.clone();
                let cmd = manifold_editing::commands::settings::RestoreRecordedTempoLaneCommand::new(
                    old_bpm, old_points, new_points,
                );
                editing.execute(Box::new(cmd), project);
                return;
            }

            // Fall back to recorded project BPM
            if project.recording_provenance.has_recorded_project_bpm {
                let recorded_bpm = project.recording_provenance.recorded_project_bpm;
                let old_bpm = project.settings.bpm;
                if (old_bpm - recorded_bpm).abs() < 0.0001 { return; }

                let cmd = manifold_editing::commands::settings::ChangeBpmCommand::new(old_bpm, recorded_bpm);
                editing.execute(Box::new(cmd), project);
            }
        }
    }

    /// Clear tempo map to single point at current BPM.
    /// Port of Unity TransportController.ClearTempoMap.
    pub fn clear_tempo_map(engine: &mut PlaybackEngine, editing: &mut manifold_editing::service::EditingService) {
        if let Some(project) = engine.project_mut() {
            if project.tempo_map.points.len() <= 1 { return; }
            let old_points = project.tempo_map.clone_points();
            let bpm = project.settings.bpm;
            let cmd = manifold_editing::commands::settings::ClearTempoMapCommand::new(old_points, bpm);
            editing.execute(Box::new(cmd), project);
        }
    }
}

impl Default for TransportController {
    fn default() -> Self { Self::new() }
}
