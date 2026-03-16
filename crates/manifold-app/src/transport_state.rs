//! Transport state polling with dirty-checking.
//! Port of Unity WorkspaceController.UpdateBitmapTransport.
//!
//! Caches transport state values and only pushes to UI panels when changed.
//! Throttled to TRANSPORT_UPDATE_INTERVAL (0.25s) except for per-frame items
//! like time display.

use manifold_core::types::{ClockAuthority, PlaybackState};
use manifold_playback::engine::PlaybackEngine;
use manifold_playback::transport_controller::TransportController;
use manifold_ui::color;
use manifold_ui::node::Color32;
use manifold_ui::tree::UITree;

use crate::ui_root::UIRoot;

const TRANSPORT_UPDATE_INTERVAL: f32 = 0.25;

/// Cached state for dirty-checking transport UI updates.
/// Matches Unity WorkspaceController's cached transport fields exactly.
pub struct TransportStateCache {
    playback_state: PlaybackState,
    bpm: f32,
    dirty: bool,
    authority: ClockAuthority,
    link_enabled: bool,
    link_peers: i32,
    clk_enabled: bool,
    clk_receiving: bool,
    clk_position: String,
    sync_enabled: bool,
    last_update_time: f32,
}

impl TransportStateCache {
    pub fn new() -> Self {
        Self {
            playback_state: PlaybackState::Stopped,
            bpm: -1.0, // Force first update
            dirty: false,
            authority: ClockAuthority::Internal,
            link_enabled: false,
            link_peers: -1,
            clk_enabled: false,
            clk_receiving: false,
            clk_position: String::new(),
            sync_enabled: false,
            last_update_time: -1.0,
        }
    }

    /// Update transport UI with dirty-checking. Called once per frame.
    /// Header time display updates every frame; transport state throttled to 0.25s.
    pub fn update(
        &mut self,
        ui: &mut UIRoot,
        engine: &PlaybackEngine,
        transport: &TransportController,
        is_dirty: bool,
        project_path: Option<&std::path::Path>,
        current_time: f32,
    ) {
        let tree = &mut ui.tree;

        // ── Per-frame: time display (needs per-frame accuracy) ──
        self.update_time_display(ui, engine, project_path, is_dirty);

        // ── Throttled: transport state (0.25s) ──
        if current_time - self.last_update_time < TRANSPORT_UPDATE_INTERVAL {
            return;
        }
        self.last_update_time = current_time;

        self.update_playback_state(ui, engine);
        self.update_bpm(ui, engine);
        self.update_dirty(ui, is_dirty);
        self.update_authority(ui, engine);
        self.update_link_state(ui, transport);
        self.update_midi_clock_state(ui, transport);
        self.update_sync_output_state(ui, transport, engine);
        self.update_bpm_buttons(ui, engine);
    }

    fn update_time_display(
        &self,
        ui: &mut UIRoot,
        engine: &PlaybackEngine,
        project_path: Option<&std::path::Path>,
        is_dirty: bool,
    ) {
        let beat = engine.current_beat();
        let time = engine.current_time();

        if let Some(project) = engine.project() {
            let tree = &mut ui.tree;

            // Unity FormatTime: "{minutes:D2}:{seconds:D2}.{tenths}"
            let mins = (time / 60.0).floor() as i32;
            let secs = (time % 60.0).floor() as i32;
            let tenths = ((time * 10.0) % 10.0).floor() as i32;
            let time_str = format!("{:02}:{:02}.{}", mins, secs, tenths);

            let bpb = project.settings.time_signature_numerator.max(1) as f32;
            let bar = (beat / bpb).floor() as i32 + 1;
            let beat_in_bar = (beat % bpb).floor() as i32 + 1;
            let sixteenth = ((beat % 1.0) * 4.0).floor() as i32 + 1;
            let display = format!("{}  |  {}.{}.{}", time_str, bar, beat_in_bar, sixteenth);

            ui.header.set_time_display(tree, &display);

            // Project name + dirty bullet (per-frame for dirty tracking)
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

            // Zoom label
            let ppb = ui.viewport.pixels_per_beat();
            ui.header.set_zoom_label(tree, &format!("{:.0} px/beat", ppb));
        }
    }

    fn update_playback_state(&mut self, ui: &mut UIRoot, engine: &PlaybackEngine) {
        let state = engine.current_state();
        if state == self.playback_state { return; }
        self.playback_state = state;

        let tree = &mut ui.tree;
        let (play_text, play_color) = match state {
            PlaybackState::Playing => ("PAUSE", color::PLAY_ACTIVE),
            PlaybackState::Paused => ("PLAY", color::PAUSED_YELLOW),
            PlaybackState::Stopped => ("PLAY", color::PLAY_GREEN),
        };
        ui.transport.set_play_state(tree, play_text, play_color);

        // Record state — disabled when OSC is authority
        if let Some(project) = engine.project() {
            let auth = project.settings.clock_authority;
            let rec_allowed = auth != ClockAuthority::Osc;
            ui.transport.set_record_state(tree, engine.is_recording() && rec_allowed, rec_allowed);
        }
    }

    fn update_bpm(&mut self, ui: &mut UIRoot, engine: &PlaybackEngine) {
        if let Some(project) = engine.project() {
            let bpm = project.settings.bpm;
            if (bpm - self.bpm).abs() < 0.01 { return; }
            self.bpm = bpm;
            ui.transport.set_bpm_text(&mut ui.tree, &format!("{:.1}", bpm));
        }
    }

    fn update_dirty(&mut self, ui: &mut UIRoot, is_dirty: bool) {
        if is_dirty == self.dirty { return; }
        self.dirty = is_dirty;
        ui.transport.set_save_text(&mut ui.tree, if is_dirty { "SAVE *" } else { "SAVE" });
    }

    fn update_authority(&mut self, ui: &mut UIRoot, engine: &PlaybackEngine) {
        if let Some(project) = engine.project() {
            let auth = project.settings.clock_authority;
            if auth == self.authority { return; }
            self.authority = auth;

            let auth_color = match auth {
                ClockAuthority::Internal => color::BUTTON_INACTIVE_C32,
                ClockAuthority::Link => color::LINK_ORANGE,
                ClockAuthority::MidiClock => color::MIDI_PURPLE,
                ClockAuthority::Osc => color::ABLETON_LINK_BLUE,
            };
            ui.transport.set_clock_authority(&mut ui.tree, auth.transport_label(), auth_color);
        }
    }

    fn update_link_state(&mut self, ui: &mut UIRoot, transport: &TransportController) {
        let enabled = transport.link_sync.as_ref().map_or(false, |s| s.is_enabled());
        // For now, no peer count available — will be populated when LinkSyncController exists
        let peers: i32 = 0;

        if enabled == self.link_enabled && peers == self.link_peers { return; }
        self.link_enabled = enabled;
        self.link_peers = peers;

        let tree = &mut ui.tree;
        if !enabled {
            ui.transport.set_link_state(tree, false, color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);
        } else if peers > 0 {
            let status = if peers == 1 { "1 peer".to_string() } else { format!("{} peers", peers) };
            ui.transport.set_link_state(tree, true, color::STATUS_DOT_GREEN, &status, color::TEXT_WHITE_C32);
        } else {
            ui.transport.set_link_state(tree, true, color::STATUS_DOT_YELLOW, "Listening", color::TEXT_DIMMED_C32);
        }
    }

    fn update_midi_clock_state(&mut self, ui: &mut UIRoot, transport: &TransportController) {
        let enabled = transport.midi_clock_sync.as_ref().map_or(false, |s| s.is_enabled());
        // For now, no receiving/position available — will be populated when MidiClockSyncController exists
        let receiving = false;
        let position = String::new();

        if enabled == self.clk_enabled && receiving == self.clk_receiving && position == self.clk_position { return; }
        self.clk_enabled = enabled;
        self.clk_receiving = receiving;
        self.clk_position = position.clone();

        let device_text = "Select..."; // Will be populated from controller

        let tree = &mut ui.tree;
        if !enabled {
            ui.transport.set_clk_state(tree, false, device_text, color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);
        } else if receiving {
            ui.transport.set_clk_state(tree, true, device_text, color::STATUS_DOT_GREEN, &position, color::TEXT_WHITE_C32);
        } else {
            ui.transport.set_clk_state(tree, true, device_text, color::STATUS_DOT_YELLOW, "Waiting", color::TEXT_DIMMED_C32);
        }
    }

    fn update_sync_output_state(&mut self, ui: &mut UIRoot, transport: &TransportController, engine: &PlaybackEngine) {
        let enabled = transport.osc_sender_enabled;
        if enabled == self.sync_enabled { return; }
        self.sync_enabled = enabled;

        let tree = &mut ui.tree;
        if !enabled {
            ui.transport.set_sync_state(tree, false, color::STATUS_DOT_INACTIVE, "Off", color::TEXT_DIMMED_C32);
        } else {
            let port = engine.project()
                .map(|p| p.settings.osc_send_port)
                .unwrap_or(9001);
            let status = format!(":{}", port);
            ui.transport.set_sync_state(tree, true, color::STATUS_DOT_GREEN, &status, color::TEXT_WHITE_C32);
        }
    }

    fn update_bpm_buttons(&mut self, ui: &mut UIRoot, engine: &PlaybackEngine) {
        if let Some(project) = engine.project() {
            let bpm = project.settings.bpm;

            // Reset: enabled when recorded tempo differs from current
            let can_reset = !project.recording_provenance.recorded_tempo_lane.is_empty()
                || (project.recording_provenance.has_recorded_project_bpm
                    && (bpm - project.recording_provenance.recorded_project_bpm).abs() >= 0.0001);
            ui.transport.set_bpm_reset_active(&mut ui.tree, can_reset);

            // Clear: enabled when tempo map has >1 point
            let can_clear = project.tempo_map.points.len() > 1;
            ui.transport.set_bpm_clear_active(&mut ui.tree, can_clear);
        }
    }
}

impl Default for TransportStateCache {
    fn default() -> Self { Self::new() }
}
