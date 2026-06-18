//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.
use manifold_core::project::Project;
use manifold_core::{Beats, ClipId, EffectId, LayerId, Seconds};
use manifold_editing::command::Command;
use manifold_media::export_config::ExportConfig;
use manifold_playback::audio_sync::PreloadedAudioData;

pub enum ContentCommand {
    // ── Transport ──────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
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
    SetFrameRate(f64),

    // ── GPU ────────────────────────────────────────────────────────
    /// Resize the content pipeline to `(width, height)` output resolution
    /// at the given `render_scale` (1.0 = native, 0.5 = FSR performance).
    ResizeContent(u32, u32, f32),
    /// Resize the workspace preview surface. This does not affect the
    /// audience output resolution.
    ResizeWorkspacePreview(u32, u32),

    // ── Transport/sync ─────────────────────────────────────────────
    ToggleLink,
    ToggleMidiClock,
    SetMidiClockDevice(i32),
    ResetBpm,

    // ── Audio ──────────────────────────────────────────────────────
    AudioLoaded {
        preloaded: Box<PreloadedAudioData>,
    },
    ResetAudio,
    /// Set which send the Audio Setup spectrogram scope is showing (`None` =
    /// panel closed / no selection). Drives the worker's VQT column producer.
    /// Like `WatchEffectGraph`, this is UI state pushed to the content thread.
    SetSpectrogramSend(Option<manifold_core::AudioSendId>),

    // ── Stem audio ──────────────────────────────────────────────────
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
    ///
    /// After running the closure the handler re-syncs the renderer caches
    /// (video library), Ableton listeners, and the forked-preset overlay —
    /// the structural maintenance a closure that adds clips / forks a preset /
    /// recalibrates a mapping needs. Use this when the mutation may change
    /// anything beyond a live scalar value.
    MutateProject(Box<dyn FnOnce(&mut Project) + Send>),

    /// Maintenance-free twin of [`MutateProject`] for the live-performance
    /// instrument: per-mouse-move scalar writes from card-slider / opacity /
    /// macro drags. The closure runs and nothing else does — no renderer
    /// re-notify, no Ableton listener rebuild, no preset-overlay fingerprint.
    ///
    /// Those writes only touch `param_values` / settings scalars, which every
    /// consumer already reads fresh each frame, so none of that bookkeeping
    /// applies. Keeping it off this path means a slider drag costs exactly the
    /// value write, never project-scale work on the render tick. A mutation that
    /// changes structure (clips, forks, mappings, video library) must use
    /// [`MutateProject`] instead so the caches stay in sync.
    MutateProjectLive(Box<dyn FnOnce(&mut Project) + Send>),

    // ── Percussion ─────────────────────────────────────────────────
    /// Run per-clip detection on an existing audio clip (audio-clip-detection).
    /// Analyzes the clip's file and places its triggers, owned by the clip.
    /// Producer is the audio-clip inspector's Detect button (P4); the content-side
    /// handler is wired now so the orchestrator path is complete and testable.
    #[allow(dead_code)]
    DetectClip(ClipId),
    /// Re-place a clip's triggers from its cached analysis (no backend run).
    /// Driven by the inspector's live knobs (P4). Instant.
    #[allow(dead_code)]
    ReplanClip(ClipId),
    /// Remove every trigger a given audio clip produced (one undoable step).
    #[allow(dead_code)]
    ClearClipTriggers(ClipId),
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
    PercussionCalibrateDownbeat {
        playhead_beat: Beats,
        beats_per_bar: i32,
    },
    /// Nudge percussion alignment by delta_beats.
    /// Port of Unity: percussionImportController.NudgeImportedPercussionAlignment(delta).
    PercussionNudgeAlignment(Beats),
    /// Reset percussion alignment to beat 0.
    /// Port of Unity: percussionImportController.ResetImportedPercussionAlignment().
    PercussionResetAlignment,

    // ── Ableton bridge ─────────────────────────────────────────────
    /// Map an Ableton macro to a MANIFOLD parameter.
    AbletonMapParam {
        target: manifold_core::ableton_mapping::AbletonMappingTarget,
        address: manifold_core::ableton_mapping::AbletonMacroAddress,
    },
    /// Remove an Ableton mapping from a parameter.
    AbletonUnmapParam {
        target: manifold_core::ableton_mapping::AbletonMappingTarget,
    },
    /// Re-discover Ableton session structure (tracks, devices, macros).
    AbletonRediscover,
    /// Toggle OSC sync mode between M4L and AbletonOSC.
    ToggleOscSyncMode,

    // ── Compositor ────────────────────────────────────────────────
    MarkCompositorDirty,

    // ── LED output ──────────────────────────────────────────────
    /// Initialize LED/ArtNet output with the given settings.
    InitLedOutput(Box<manifold_led::LedSettings>),
    /// Shut down LED output pipeline.
    ShutdownLedOutput,

    // ── Generator ──────────────────────────────────────────────────
    /// Notify renderer that a layer's generator type changed.
    /// Port of C# PlaybackController.NotifyGeneratorTypeChanged().
    GeneratorTypeChanged {
        layer_id: LayerId,
        new_type: manifold_core::PresetTypeId,
    },

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

    // ── Output surface (direct present from content thread) ─────
    /// Attach an output surface for direct-to-drawable presentation.
    #[cfg(target_os = "macos")]
    SetOutputSurface(manifold_gpu::GpuSurface),
    /// Detach the output surface (output window closed).
    #[cfg(target_os = "macos")]
    ClearOutputSurface,
    /// Resize the output surface drawable (fullscreen toggle).
    #[cfg(target_os = "macos")]
    ResizeOutputSurface(u32, u32),
    /// Suspend/resume direct present during display retarget.
    #[cfg(target_os = "macos")]
    SetOutputPresentSuspended(bool),

    // ── Export ────────────────────────────────────────────────────
    /// Begin offline video export. Content thread enters export loop.
    StartExport(Box<ExportConfig>),
    /// Cancel in-progress export. Polled by the export loop at
    /// content_export.rs:242. No UI producer yet — the cancel button/hotkey
    /// is a known UX gap; leave the variant and plumbing ready to wire up.
    #[allow(dead_code)]
    CancelExport,

    // ── Live Recording ───────────────────────────────────────────
    /// Start live recording. Captures output frames + optional audio.
    StartLiveRecording(Box<manifold_recording::LiveRecordingConfig>),
    /// Stop live recording and finalize the output file.
    StopLiveRecording,

    // ── GPU events ────────────────────────────────────────────────
    /// GPU finished with a surface — wake the content thread if it's
    /// blocked in `recv()` waiting for surface readiness.
    /// Sent by the Metal `SharedEventListener` notification block.
    #[cfg(target_os = "macos")]
    SurfaceReady,

    // ── Editor canvas ─────────────────────────────────────────────
    /// Tell the content thread which effect instance's internal graph
    /// to snapshot for the editor canvas. `None` clears it (canvas
    /// goes empty). Sent when the user clicks a cog or closes the
    /// editor. Keyed by `EffectId` (not type id) so two cards of the
    /// same effect type produce independent snapshots — required for
    /// per-card graph divergence.
    WatchEffectGraph(Option<EffectId>),

    /// Generator-side counterpart of `WatchEffectGraph`. Sent when the
    /// user clicks the cog on a generator card. Snapshots the layer's
    /// generator graph (the bundled JSON for the generator type) into
    /// the editor canvas. `None` clears.
    ///
    /// Keyed by `LayerId` because generators live per-layer (one
    /// generator per layer at most) — the layer is the natural
    /// identity. Per-layer graph overrides are pending the edit-side
    /// follow-up; today the snapshot is the bundled JSON unchanged.
    WatchGeneratorGraph(Option<manifold_core::LayerId>),

    /// Set the node whose output the graph editor is previewing, within the
    /// currently-watched effect/generator. `None` clears the preview. The
    /// content thread combines this with `WatchEffectGraph` /
    /// `WatchGeneratorGraph` to drive the per-node output capture. Sent when
    /// the editor's node selection changes.
    SetGraphPreviewNode(Option<manifold_core::NodeId>),

    /// Toggle auto-gain/normalization on the graph editor's node-output
    /// preview. On by default; remaps the previewed texture's min..max to 0..1
    /// so dark intermediates (force fields, normals, depth) are legible. Only
    /// affects the node preview pane, never the live render. Sent when the user
    /// flips the toggle under the preview.
    SetNodePreviewNormalize(bool),

    /// The nodes the editor canvas can currently show, for per-node thumbnail
    /// capture into the atlas. Sent (deduped) while the graph editor is open —
    /// changes only on scope descend/ascend or topology edits — and as an empty
    /// vec when the editor closes. Only these nodes are captured, so hidden /
    /// off-scope / collapsed-group nodes cost nothing; empty = atlas off, so a
    /// live show pays nothing.
    SetNodeAtlasVisible(Vec<manifold_core::NodeId>),

    /// Dump every node output of the currently-watched effect to a temp folder
    /// as 16-bit linear PNGs + a manifest, for visual inspection. One-shot;
    /// the content thread picks the output directory and logs it. No-op unless
    /// an effect graph is being watched.
    DumpGraphOutputs,

    // ── Shutdown ──────────────────────────────────────────────────
    Shutdown,
}

impl ContentCommand {
    /// Send a command to the content thread. Logs on failure (channel disconnected = shutdown).
    pub fn send(tx: &crossbeam_channel::Sender<ContentCommand>, cmd: ContentCommand) {
        if let Err(e) = tx.send(cmd) {
            log::error!("[UI] Content command channel disconnected: {e}");
        }
    }
}
