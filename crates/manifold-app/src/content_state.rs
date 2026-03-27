//! State pushed from the content thread to the UI thread each content frame.
//!
//! The UI thread reads the latest ContentState to display transport info,
//! project data version, and other engine state without accessing the
//! PlaybackEngine or EditingService directly.

use std::sync::Arc;
use manifold_core::project::Project;
use manifold_core::types::ClockAuthority;
use manifold_playback::stem_audio::STEM_COUNT;

/// Sent once when an export finishes.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ExportFinishedEvent {
    pub success: bool,
    pub message: String,
    pub output_path: String,
}

/// State snapshot sent from the content thread to the UI thread.
/// The UI thread drains these from a bounded channel and uses the latest.
#[derive(Clone)]
#[allow(dead_code)]
pub struct ContentState {
    // ── Transport ──────────────────────────────────────────────────
    pub current_beat: f32,
    pub current_time: f32,
    pub is_playing: bool,
    pub is_recording: bool,

    // ── Content thread perf ─────────────────────────────────────
    pub content_fps: f32,
    pub content_frame_time_ms: f32,
    pub active_clips: usize,

    // ── Editing ────────────────────────────────────────────────────
    pub data_version: u64,
    pub editing_is_dirty: bool,

    // ── Project settings (from authoritative project) ─────────────
    pub bpm: f64,
    pub frame_rate: f64,
    pub clock_authority: ClockAuthority,
    pub time_signature_numerator: i32,

    // ── Transport controller state ────────────────────────────────
    pub link_enabled: bool,
    pub link_tempo: f64,
    pub link_peers: i32,
    pub link_is_playing: bool,
    pub midi_clock_enabled: bool,
    pub midi_clock_bpm: f32,
    pub midi_clock_position_display: String,
    pub midi_clock_receiving: bool,
    pub midi_clock_device_name: String,
    /// Available MIDI input device names for the CLK device dropdown.
    pub midi_device_names: Vec<String>,
    pub osc_sender_enabled: bool,
    pub osc_receiving_timecode: bool,
    pub osc_timecode_display: String,

    // ── Stem audio state ──────────────────────────────────────────
    pub stem_expanded: bool,
    pub stem_ready: bool,
    pub stem_muted: [bool; STEM_COUNT],
    pub stem_soloed: [bool; STEM_COUNT],
    pub stem_available: [bool; STEM_COUNT],

    // ── Percussion status ─────────────────────────────────────────
    pub percussion_importing: bool,
    pub percussion_status_message: String,
    pub percussion_progress: f32,
    pub percussion_show_progress: bool,

    // ── Profiling ────────────────────────────────────────────────
    /// Whether a profiling session is currently recording.
    pub profiling_active: bool,
    /// Number of frames recorded in the current session.
    pub profiling_frame_count: u64,

    // ── LED output ────────────────────────────────────────────────
    /// Whether LED output is enabled.
    pub led_enabled: bool,
    /// Whether the LED pipeline is initialized and ready.
    pub led_initialized: bool,

    // ── Export ────────────────────────────────────────────────────
    /// Whether an export is currently in progress.
    pub is_exporting: bool,
    /// Export progress (0.0..1.0).
    pub export_progress: f32,
    /// Export status text (e.g. "Exporting 120/600 (20%)").
    pub export_status: String,
    /// Set once when export finishes (success or failure).
    pub export_finished: Option<ExportFinishedEvent>,

    // ── Project snapshot ──────────────────────────────────────────
    /// Sent when data_version changes or modulation is active so the UI
    /// thread can update its local_project. Arc avoids deep-cloning the
    /// entire Project struct every modulation frame — only pointer-copies
    /// the Arc when the underlying project hasn't changed structurally.
    pub project_snapshot: Option<Arc<Project>>,
}

impl Default for ContentState {
    fn default() -> Self {
        Self {
            current_beat: 0.0,
            current_time: 0.0,
            is_playing: false,
            is_recording: false,
            content_fps: 0.0,
            content_frame_time_ms: 0.0,
            active_clips: 0,
            data_version: 0,
            editing_is_dirty: false,
            bpm: 120.0,
            frame_rate: 60.0,
            clock_authority: ClockAuthority::Internal,
            time_signature_numerator: 4,
            link_enabled: false,
            link_tempo: 120.0,
            link_peers: 0,
            link_is_playing: false,
            midi_clock_enabled: false,
            midi_clock_bpm: 120.0,
            midi_clock_position_display: String::new(),
            midi_clock_receiving: false,
            midi_clock_device_name: String::new(),
            midi_device_names: Vec::new(),
            osc_sender_enabled: false,
            osc_receiving_timecode: false,
            osc_timecode_display: String::new(),
            stem_expanded: false,
            stem_ready: false,
            stem_muted: [false; STEM_COUNT],
            stem_soloed: [false; STEM_COUNT],
            stem_available: [false; STEM_COUNT],
            percussion_importing: false,
            percussion_status_message: String::new(),
            percussion_progress: 0.0,
            percussion_show_progress: false,
            profiling_active: false,
            profiling_frame_count: 0,
            led_enabled: false,
            led_initialized: false,
            is_exporting: false,
            export_progress: 0.0,
            export_status: String::new(),
            export_finished: None,
            project_snapshot: None,
        }
    }
}
