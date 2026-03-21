//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.

use manifold_core::project::Project;
use manifold_editing::command::Command;
use manifold_playback::audio_sync::PreloadedAudioData;
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_playback::stem_audio::PreloadedStemData;

pub enum ContentCommand {
    // ── Transport ──────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
    TogglePlayback,
    SeekTo(f32),
    SeekToBeat(f32),
    SetRecording(bool),

    // ── Editing (commands cross thread boundary) ───────────────────
    Execute(Box<dyn Command + Send>),
    ExecuteBatch(Vec<Box<dyn Command>>, String),
    Undo,
    Redo,
    /// Mark editing service as clean after save/load.
    SetProject,

    // ── Project lifecycle ──────────────────────────────────────────
    LoadProject(Box<Project>),
    NewProject(Box<Project>),

    // ── Settings ───────────────────────────────────────────────────
    SetBpm(f64),
    SetFrameRate(f64),

    // ── GPU ────────────────────────────────────────────────────────
    ResizeContent(u32, u32),

    // ── Transport/sync ─────────────────────────────────────────────
    CycleClockAuthority,
    ToggleLink,
    ToggleMidiClock,
    ToggleSyncOutput,
    ResetBpm,

    // ── Audio ──────────────────────────────────────────────────────
    AudioLoaded {
        preloaded: PreloadedAudioData,
        waveform: Option<DecodedAudio>,
    },
    ResetAudio,

    // ── Stem audio ──────────────────────────────────────────────────
    /// Apply pre-loaded stem data on the content thread (fast — no I/O).
    StemAudioLoaded(PreloadedStemData),
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
        clip_ids: Vec<String>,
        region: Option<manifold_core::selection::SelectionRegion>,
    },
    /// Paste from clipboard at target position. Content thread mutates project and
    /// sends updated snapshot. `result_tx` receives pasted clip IDs for UI selection.
    PasteClips {
        target_beat: f32,
        target_layer: i32,
        result_tx: std::sync::mpsc::Sender<Vec<String>>,
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

    // ── Compositor ────────────────────────────────────────────────
    MarkCompositorDirty,

    // ── Generator ──────────────────────────────────────────────────
    /// Notify renderer that a layer's generator type changed.
    /// Port of C# PlaybackController.NotifyGeneratorTypeChanged().
    GeneratorTypeChanged { layer_index: i32, new_type: manifold_core::GeneratorType },

    // ── Lifecycle ─────────────────────────────────────────────────
    /// Pause rendering (content thread stops ticking/rendering but still drains commands).
    /// Used while native file dialogs are open to avoid GPU contention on macOS.
    PauseRendering,
    /// Resume rendering after a dialog closes.
    ResumeRendering,

    // ── Shutdown ──────────────────────────────────────────────────
    Shutdown,
}
