//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.

use manifold_core::project::Project;
use manifold_editing::command::Command;
use manifold_playback::audio_sync::PreloadedAudioData;
use manifold_playback::audio_decoder::DecodedAudio;

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
    ExecuteBatch(Vec<Box<dyn Command + Send>>, String),
    Record(Box<dyn Command + Send>),
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

    // ── Direct project mutation ────────────────────────────────────
    /// Closure runs on the content thread with &mut Project access.
    MutateProject(Box<dyn FnOnce(&mut Project) + Send>),

    // ── Save support ──────────────────────────────────────────────
    /// Request project clone for serialization. Content thread sends
    /// the project through the provided oneshot sender.
    RequestProjectSnapshot(std::sync::mpsc::Sender<Project>),

    // ── Compositor ────────────────────────────────────────────────
    MarkCompositorDirty,

    // ── Shutdown ──────────────────────────────────────────────────
    Shutdown,
}
