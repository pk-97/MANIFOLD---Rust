//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.
use manifold_core::{Beats, Bpm, ClipId, LayerId, Seconds};
use manifold_core::project::Project;
use manifold_editing::command::Command;
use manifold_media::export_config::ExportConfig;
use manifold_playback::audio_sync::PreloadedAudioData;
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_playback::stem_audio::PreloadedStemData;

#[allow(dead_code)]
pub enum ContentCommand {
    // ── Transport ──────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
    TogglePlayback,
    SeekTo(Seconds),
    SeekToBeat(Beats),
    SetRecording(bool),

    // ── Editing (commands cross thread boundary) ───────────────────
    Execute(Box<dyn Command + Send>),
    ExecuteBatch(Vec<Box<dyn Command>>, String),
    Undo,
    Redo,
    /// Reset editing service (clear undo, clipboard) after project load.
    SetProject,
    /// Mark editing service as clean (saved) without clearing undo history.
    MarkClean,

    // ── Project lifecycle ──────────────────────────────────────────
    LoadProject(Box<Project>),

    // ── Settings ───────────────────────────────────────────────────
    SetBpm(Bpm),
    SetFrameRate(f64),
    /// Enable or disable vsync-driven content thread pacing.
    /// When enabled, the content thread renders in sync with the display's
    /// refresh cadence via GpuVsyncSignal. When disabled, timer-based pacing.
    SetVsyncEnabled(bool),

    // ── GPU ────────────────────────────────────────────────────────
    /// Resize the content pipeline to `(width, height)` output resolution
    /// at the given `render_scale` (1.0 = native, 0.5 = FSR performance).
    ResizeContent(u32, u32, f32),
    /// Resize the workspace preview surface. This does not affect the
    /// audience output resolution.
    ResizeWorkspacePreview(u32, u32),

    // ── Transport/sync ─────────────────────────────────────────────
    CycleClockAuthority,
    ToggleLink,
    ToggleMidiClock,
    ToggleSyncOutput,
    SetMidiClockDevice(i32),
    ResetBpm,

    // ── Audio ──────────────────────────────────────────────────────
    AudioLoaded {
        preloaded: Box<PreloadedAudioData>,
        waveform: Option<DecodedAudio>,
    },
    ResetAudio,

    // ── Stem audio ──────────────────────────────────────────────────
    /// Apply pre-loaded stem data on the content thread (fast — no I/O).
    StemAudioLoaded(Box<PreloadedStemData>),
    /// Toggle expand/collapse of stem playback.
    /// Port of C# StemAudioController.SetExpanded(bool).
    StemSetExpanded(bool),
    /// Toggle mute for a stem index.
    /// Port of C# StemAudioController.ToggleMuted(int).
    StemToggleMute(usize),
    /// Toggle solo for a stem index.
    /// Port of C# StemAudioController.ToggleSoloed(int).
    StemToggleSolo(usize),
    /// Reset all stems (on project switch/audio remove).
    StemReset,

    // ── Clipboard ────────────────────────────────────────────────
    /// Copy clips to clipboard on the content thread (EditingService owns clipboard).
    CopyClips {
        clip_ids: Vec<ClipId>,
        region: Option<manifold_core::selection::SelectionRegion>,
    },
    /// Paste from clipboard at target position. Content thread mutates project and
    /// sends updated snapshot. `result_tx` receives pasted clip IDs for UI selection.
    PasteClips {
        target_beat: Beats,
        target_layer: i32,
        result_tx: std::sync::mpsc::Sender<Vec<ClipId>>,
    },

    // ── Direct project mutation ────────────────────────────────────
    /// Closure runs on the content thread with &mut Project access.
    MutateProject(Box<dyn FnOnce(&mut Project) + Send>),

    // ── Save support ──────────────────────────────────────────────
    /// Request project clone for serialization. Content thread sends
    /// the project through the provided oneshot sender.
    RequestProjectSnapshot(std::sync::mpsc::Sender<Project>),

    // ── Percussion ─────────────────────────────────────────────────
    /// Trigger percussion import pipeline with the selected audio/JSON file path.
    /// Port of Unity: percussionImportController.OnImportPercussionMap(path).
    PercussionImport(String),
    /// Re-analyze triggers for a specific instrument group (e.g. "drums", "bass").
    /// Port of Unity: percussionImportController.OnReAnalyzeTriggers(instrumentGroup).
    ReAnalyzeTriggers(String),
    /// Re-import stems from current audio file (re-runs Demucs).
    /// Port of Unity: percussionImportController.OnReImportStems().
    ReImportStems,
    /// Calibrate percussion downbeat at the current playhead beat.
    /// Port of Unity: percussionImportController.CalibrateImportedPercussionDownbeatAtPlayhead().
    PercussionCalibrateDownbeat { playhead_beat: Beats, beats_per_bar: i32 },
    /// Nudge percussion alignment by delta_beats.
    /// Port of Unity: percussionImportController.NudgeImportedPercussionAlignment(delta).
    PercussionNudgeAlignment(Beats),
    /// Reset percussion alignment to beat 0.
    /// Port of Unity: percussionImportController.ResetImportedPercussionAlignment().
    PercussionResetAlignment,

    // ── Compositor ────────────────────────────────────────────────
    MarkCompositorDirty,

    // ── LED output ──────────────────────────────────────────────
    /// Initialize LED/ArtNet output with the given settings.
    InitLedOutput(Box<manifold_led::LedSettings>),
    /// Shut down LED output pipeline.
    ShutdownLedOutput,
    /// Enable or disable LED output (without reinitializing).
    SetLedEnabled(bool),

    // ── Generator ──────────────────────────────────────────────────
    /// Notify renderer that a layer's generator type changed.
    /// Port of C# PlaybackController.NotifyGeneratorTypeChanged().
    GeneratorTypeChanged { layer_id: LayerId, new_type: manifold_core::GeneratorTypeId },

    // ── Lifecycle ─────────────────────────────────────────────────
    /// Pause rendering (content thread stops ticking/rendering but still drains commands).
    /// Used while native file dialogs are open to avoid GPU contention on macOS.
    PauseRendering,
    /// Resume rendering after a dialog closes.
    ResumeRendering,

    // ── Profiling ────────────────────────────────────────────────
    /// Start a profiling session on the content thread.
    #[cfg(feature = "profiling")]
    StartProfiling {
        project_name: String,
        project_path: String,
        resolution: (u32, u32),
        target_fps: f32,
        gpu_name: String,
    },
    /// Stop profiling and dump session data to disk.
    #[cfg(feature = "profiling")]
    StopProfiling,

    // ── Display ───────────────────────────────────────────────────
    /// Update EDR headroom when window moves to a different display.
    UpdateEdrHeadroom(f64),

    // ── Export ────────────────────────────────────────────────────
    /// Begin offline video export. Content thread enters export loop.
    StartExport(Box<ExportConfig>),
    /// Cancel in-progress export.
    CancelExport,

    // ── Shutdown ──────────────────────────────────────────────────
    Shutdown,
}

impl ContentCommand {
    /// Send a command to the content thread. Logs on failure (channel full or disconnected).
    pub fn send(tx: &crossbeam_channel::Sender<ContentCommand>, cmd: ContentCommand) {
        if let Err(e) = tx.try_send(cmd) {
            log::error!("[UI] Content command dropped: {e}");
        }
    }
}
