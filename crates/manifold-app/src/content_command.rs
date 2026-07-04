//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.
use manifold_core::project::Project;
use manifold_core::{Beats, ClipId, EffectId, LayerId, SceneId, Seconds};
use manifold_editing::command::Command;
use manifold_media::export_config::ExportConfig;

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

    // ── Session mode (P2, docs/SESSION_MODE_DESIGN.md §5) ───────────
    // No producer yet — the grid panel is P4. The variants + content-thread
    // plumbing (PlaybackEngine::session_* methods) are ready to wire up, same
    // as `ReplanClip`/`CancelExport` above.
    /// Launch a session slot, or — if the (layer, scene) cell is empty —
    /// issue the sparse-grid stop for that layer ("empty slot cells don't
    /// exist"). Starts the transport first if it was stopped, and in that
    /// case launches immediately rather than waiting for a quantize
    /// boundary. Not undoable — a performance gesture, like a MIDI trigger.
    #[allow(dead_code)]
    SessionLaunchSlot { layer_id: LayerId, scene_id: SceneId },
    /// Quantized stop for one layer's session slot. `session_override`
    /// persists — the layer goes black, it does not fall back to the
    /// arrangement.
    #[allow(dead_code)]
    SessionStopSlot { layer_id: LayerId },
    /// Launch every slot in a scene; layers currently playing a session slot
    /// with no slot in this scene get a quantized stop (Ableton "stop other
    /// tracks" default).
    #[allow(dead_code)]
    SessionLaunchScene { scene_id: SceneId },
    /// Quantized stop of every currently-playing (or about-to-play) session
    /// slot. Distinct from a full transport stop: `session_override` is
    /// untouched.
    #[allow(dead_code)]
    SessionStopAll,
    /// Immediate (not quantized): clears `session_override` for the layer
    /// (or every layer if `None`) and stops its playing slot. Timeline
    /// clips resume immediately.
    #[allow(dead_code)]
    SessionBackToArrangement { layer_id: Option<LayerId> },
    /// Set the global session launch quantize (0 = launch immediately).
    #[allow(dead_code)]
    SessionSetQuantize { beats: Beats },

    // ── Automation lanes (P1, docs/AUTOMATION_LANES_DESIGN.md §4/§6) ──
    /// Automation lanes' "Back to Arrangement": clears every override latch
    /// (one global action, not per-layer — lights up red in the transport
    /// bar when any latch is set), resuming every automated param's lane.
    /// Mutates runtime latch state, not the project, so this is a
    /// `ContentCommand` (like `SessionBackToArrangement`) rather than an
    /// `EditingService` command — no undo entry.
    #[allow(dead_code)]
    AutomationBackToArrangement,
    /// Toggle the global Automation Arm (§5): while on, touching an
    /// automated param (while playing) records into its lane instead of
    /// latching an override. Runtime-only state, same shape as
    /// `AutomationBackToArrangement` — no undo entry.
    #[allow(dead_code)]
    AutomationSetArmed(bool),

    // ── Audio ──────────────────────────────────────────────────────
    /// Set which send the Audio Setup spectrogram scope is showing (`None` =
    /// panel closed / no selection). Drives the worker's VQT column producer.
    /// Like `WatchEffectGraph`, this is UI state pushed to the content thread.
    SetSpectrogramSend(Option<manifold_core::AudioSendId>),

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
    /// Run per-clip detection on an audio clip. Produced by the inspector Detect
    /// button (P4).
    DetectClip(ClipId),
    /// Re-place a clip's triggers from its cached analysis (no backend run).
    /// Driven by the inspector's live config knobs (per-instrument UI is a P4
    /// follow-up; the command + content path are in place).
    #[allow(dead_code)]
    ReplanClip(ClipId),
    /// Remove every trigger a given audio clip produced (one undoable step).
    /// Produced by the inspector Clear button (P4).
    ClearClipTriggers(ClipId),

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
    /// Export the current composited frame as a still image. Captured across
    /// two content ticks (readback submit → read) so the live render never
    /// stalls; the encode + disk write then run off-thread. See
    /// `ContentThread::poll_still_export`.
    ExportFrame {
        path: String,
        format: manifold_media::still_exporter::StillFormat,
    },

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

    /// The set of clips that currently want a timeline thumbnail (§24 5c) —
    /// on-screen generator/video clips wide enough to read. Sent by the UI when
    /// the visible-thumbnail scope changes (scroll/zoom/edit), deduped so a stable
    /// view costs nothing. The content thread keeps/refreshes those clips' atlas
    /// cells and evicts the rest. Empty = no timeline visible / nothing to show.
    SetClipAtlasVisible(Vec<manifold_core::ClipId>),

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
