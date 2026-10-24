//! Transport bar business logic: sync controller management, BPM editing,
//! clock authority, playback actions.
//!
//! Pure logic — no UI. The app layer bridges this to the bitmap TransportPanel.
//! Mechanical translation of Unity TransportController.cs.

use manifold_core::types::ClockAuthority;

use crate::engine::PlaybackEngine;
use crate::link_sync::LinkSyncController;
use crate::midi_clock_sync::MidiClockSyncController;

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
}

impl TransportController {
    pub fn new() -> Self {
        Self {
            link_sync: Some(LinkSyncController::new()),
            midi_clock_sync: Some(MidiClockSyncController::new()),
            osc_enabled: false,
            osc_sender_enabled: false,
        }
    }

    // ── Sync toggle actions ──────────────────────────────────────────

    /// Toggle Ableton Link on/off.
    /// Authority is auto-determined by content thread — no explicit authority management here.
    pub fn toggle_link(&mut self, engine: &mut PlaybackEngine) {
        if let Some(ref mut link) = self.link_sync {
            if link.is_link_enabled() {
                link.disable_link();
                log::info!("[TransportController] Link disabled");
            } else {
                let bpm = engine.project().map_or(120.0, |p| p.settings.bpm.0 as f64);
                link.enable_link(bpm);
                log::info!("[TransportController] Link enabled");
            }
        } else {
            log::info!("[TransportController] Link sync not available");
        }
    }

    /// Toggle MIDI Clock on/off.
    /// Authority is auto-determined by content thread — no explicit authority management here.
    pub fn toggle_midi_clock(&mut self, _engine: &mut PlaybackEngine) {
        if let Some(ref mut clk) = self.midi_clock_sync {
            if clk.is_midi_clock_enabled() {
                clk.disable_midi_clock();
                log::info!("[TransportController] MIDI Clock disabled");
            } else {
                clk.enable_midi_clock(clk.selected_source_index());
                log::info!("[TransportController] MIDI Clock enabled");
            }
        } else {
            log::info!("[TransportController] MIDI Clock sync not available");
        }
    }

    // ── BPM editing ──────────────────────────────────────────────────

    /// Reset BPM to recorded value (tempo lane or recorded project BPM).
    /// Port of Unity TransportController.ResetBpm.
    pub fn reset_bpm(
        engine: &mut PlaybackEngine,
        editing: &mut manifold_editing::service::EditingService,
    ) {
        if let Some(project) = engine.project_mut() {
            // Try recorded tempo lane first
            if !project.recording_provenance.recorded_tempo_lane.is_empty() {
                let old_bpm = project.settings.bpm;
                let old_points = project.tempo_map.clone_points();
                let new_points = project.recording_provenance.recorded_tempo_lane.clone();
                let cmd =
                    manifold_editing::commands::settings::RestoreRecordedTempoLaneCommand::new(
                        old_bpm, old_points, new_points,
                    );
                editing.execute(Box::new(cmd), project);
                return;
            }

            // Fall back to recorded project BPM
            if project.recording_provenance.has_recorded_project_bpm {
                let recorded_bpm = project.recording_provenance.recorded_project_bpm;
                let old_bpm = project.settings.bpm;
                if (old_bpm.0 - recorded_bpm.0).abs() < 0.0001 {
                    return;
                }

                let cmd = manifold_editing::commands::settings::ChangeBpmCommand::master(
                    project,
                    recorded_bpm,
                );
                editing.execute(Box::new(cmd), project);
            }
        }
    }
}

impl Default for TransportController {
    fn default() -> Self {
        Self::new()
    }
}

/// Clock authority is derived from which sync source is live, never a user
/// choice, so every write is an audited tick write (D24).
pub fn write_clock_authority(engine: &mut PlaybackEngine, authority: ClockAuthority) {
    if engine
        .project()
        .is_some_and(|p| p.settings.clock_authority != authority)
        && let Some(edit) = engine.project_edit()
    {
        manifold_editing::edit::tick_write(
            manifold_editing::edit::TickField::ClockAuthority,
            edit,
            |p| {
                p.settings.clock_authority = authority;
            },
        );
    }
}
