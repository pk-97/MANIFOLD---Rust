//! State pushed from the content thread to the UI thread each content frame.
//!
//! The UI thread reads the latest ContentState to display transport info,
//! project data version, and other engine state without accessing the
//! PlaybackEngine or EditingService directly.

use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, LayerType, OscSyncMode};
use manifold_core::{Beats, Bpm, Seconds};
use manifold_playback::stem_audio::STEM_COUNT;
use std::sync::Arc;

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
    pub current_beat: Beats,
    pub current_time: Seconds,
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
    pub midi_clock_bpm: Bpm,
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

    // ── VSync ─────────────────────────────────────────────────────
    /// Whether vsync-driven pacing is active on the content thread.
    pub vsync_active: bool,
    /// Actual locked FPS in vsync mode (display_hz / divisor).
    /// In timer mode, equals target FPS.
    pub vsync_actual_fps: f32,

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

    // ── Ableton bridge ──────────────────────────────────────────
    /// Ableton session data for UI dropdown population.
    pub ableton_session:
        Option<Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Whether the Ableton bridge is currently connected.
    pub ableton_connected: bool,
    pub ableton_transport_enabled: bool,
    pub osc_sync_mode: OscSyncMode,

    // ── Project snapshot ──────────────────────────────────────────
    /// Sent when data_version changes so the UI thread can update its
    /// local_project. Only created on structural changes (editing commands,
    /// undo/redo) — never on modulation-only frames.
    pub project_snapshot: Option<Arc<Project>>,

    /// Lightweight modulation delta — just the param_values that
    /// drivers/envelopes wrote this frame. Applied in-place to the UI's
    /// local_project without a full Project clone.
    pub modulation_snapshot: Option<ModulationSnapshot>,
}

/// Lightweight snapshot of modulated param values.
/// Captures only the `Vec<f32>` param_values from effects and generator params,
/// avoiding a full `Project::clone()` on every modulation frame.
#[derive(Clone)]
pub struct ModulationSnapshot {
    /// Macro slot values, indexed by macro slot.
    pub macro_values: Vec<f32>,
    /// Master effect param values, indexed by effect position.
    pub master_params: Vec<Vec<f32>>,
    /// Per-layer modulation data, indexed by layer position.
    pub layers: Vec<ModulationLayerData>,
}

#[derive(Clone)]
pub struct ModulationLayerData {
    /// Layer effect param values, indexed by effect position.
    pub effect_params: Vec<Vec<f32>>,
    /// Generator param values (only for generator layers).
    pub gen_param_values: Option<Vec<f32>>,
}

impl ModulationSnapshot {
    /// Capture modulated param values from the project. Only clones small
    /// `Vec<f32>` buffers — no strings, no clips, no video library.
    pub fn capture(project: &Project) -> Self {
        let macro_values = project
            .settings
            .macro_bank
            .slots
            .iter()
            .map(|slot| slot.value)
            .collect();

        let master_params = project
            .settings
            .master_effects
            .iter()
            .map(|fx| fx.param_values.clone())
            .collect();

        let layers = project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                let effect_params = layer
                    .effects
                    .as_ref()
                    .map(|effects| effects.iter().map(|fx| fx.param_values.clone()).collect())
                    .unwrap_or_default();

                let gen_param_values = if layer.layer_type == LayerType::Generator {
                    layer.gen_params().map(|gp| gp.param_values.clone())
                } else {
                    None
                };

                ModulationLayerData {
                    effect_params,
                    gen_param_values,
                }
            })
            .collect();

        Self {
            macro_values,
            master_params,
            layers,
        }
    }

    /// Apply modulated values to a project in-place. Overwrites only
    /// `param_values` — no structural changes, no allocations if lengths match.
    pub fn apply(&self, project: &mut Project) {
        for (i, &value) in self.macro_values.iter().enumerate() {
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(i) {
                slot.value = value;
            }
        }

        // Master effects
        for (i, params) in self.master_params.iter().enumerate() {
            if let Some(fx) = project.settings.master_effects.get_mut(i)
                && fx.param_values.len() == params.len()
            {
                fx.param_values.copy_from_slice(params);
            }
        }

        // Layer effects + generator params
        for (i, layer_data) in self.layers.iter().enumerate() {
            if let Some(layer) = project.timeline.layers.get_mut(i) {
                // Layer effects
                if let Some(effects) = &mut layer.effects {
                    for (j, params) in layer_data.effect_params.iter().enumerate() {
                        if let Some(fx) = effects.get_mut(j)
                            && fx.param_values.len() == params.len()
                        {
                            fx.param_values.copy_from_slice(params);
                        }
                    }
                }

                // Generator params
                if let Some(ref params) = layer_data.gen_param_values
                    && let Some(gp) = layer.gen_params_mut()
                    && gp.param_values.len() == params.len()
                {
                    gp.param_values.copy_from_slice(params);
                }
            }
        }
    }
}

impl Default for ContentState {
    fn default() -> Self {
        Self {
            current_beat: Beats::ZERO,
            current_time: Seconds::ZERO,
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
            midi_clock_bpm: Bpm(120.0),
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
            vsync_active: false,
            vsync_actual_fps: 60.0,
            led_enabled: false,
            led_initialized: false,
            is_exporting: false,
            export_progress: 0.0,
            export_status: String::new(),
            export_finished: None,
            ableton_session: None,
            ableton_connected: false,
            ableton_transport_enabled: false,
            osc_sync_mode: OscSyncMode::M4L,
            project_snapshot: None,
            modulation_snapshot: None,
        }
    }
}
