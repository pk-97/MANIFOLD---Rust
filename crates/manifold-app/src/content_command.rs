//! Commands sent from the UI thread to the content thread.
//!
//! The UI thread communicates with the content thread via a bounded
//! crossbeam channel. Each variant represents an action that must
//! execute on the content thread where PlaybackEngine and EditingService live.
use manifold_core::change::ChangeSet;
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
    /// An unexecuted command: the content thread runs it once through
    /// `EditingService` and the UI sees the result in a later snapshot.
    Execute(Box<dyn Command + Send>),
    /// Increment a trigger against the authoritative content project. A UI
    /// snapshot supplies only its identity, never the next counter value.
    FireParameter {
        target: manifold_core::GraphTarget,
        param_id: manifold_core::effects::ParamId,
    },
    /// Publish selection only after this insertion succeeds on the content thread.
    ExecuteSelecting(
        Box<dyn Command + Send>,
        crate::edit_selection::SelectAfterEdit,
    ),
    ChangeGeneratorType {
        layer_id: LayerId,
        new_type: manifold_core::PresetTypeId,
    },
    GraphEditRejected(String),
    ExecuteBatch(Vec<Box<dyn Command>>, String),
    /// Runtime envelope preview; never installs points into the project.
    PreviewAutomationLane {
        target: manifold_core::GraphTarget,
        param_id: manifold_core::effects::ParamId,
        points: Vec<manifold_core::effects::AutomationPoint>,
    },
    ClearAutomationPreviews,
    /// Close pending recording takes before an explicit envelope clear.
    FinishAutomationRecording,
    Undo,
    Redo,
    /// Reset editing service (clear undo, clipboard) after project load.
    SetProject,
    /// Mark editing service as clean (saved) without clearing undo history.
    MarkClean,
    /// Flush recording and return the authoritative project/version for saving.
    PrepareProjectSave(std::sync::mpsc::Sender<Option<(Project, u64)>>),
    MarkCleanAt(u64),

    // ── Project lifecycle ──────────────────────────────────────────
    LoadProject(Box<Project>),

    // ── Settings ───────────────────────────────────────────────────
    SetFrameRate(f64),
    /// Set the output width and height and resize the content pipeline as one
    /// content-owned, undoable operation.
    SetOutputSize(i32, i32),
    /// Apply render scale and resize the content pipeline as one content-owned,
    /// undoable operation.
    SetRenderScale(f32),
    /// Select a tonemapping curve; selecting a named curve also enables it.
    SetTonemapCurve(manifold_core::TonemapCurve),
    /// Enable or disable the saved SDR tonemapping curve.
    SetTonemapEnabled(bool),
    /// Toggle the runtime-only SDR presentation preview.
    SetSdrPreview(bool),

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

    // ── Session mode ───────────
    // No producer yet — the grid panel is P4. The variants + content-thread
    // plumbing (PlaybackEngine::session_* methods) are ready to wire up, same
    // as `ReplanClip`/`CancelExport` above.
    /// Launch a session slot, or — if the (layer, scene) cell is empty —
    /// issue the sparse-grid stop for that layer ("empty slot cells don't
    /// exist"). Starts the transport first if it was stopped, and in that
    /// case launches immediately rather than waiting for a quantize
    /// boundary. Not undoable — a performance gesture, like a MIDI trigger.
    #[allow(dead_code)]
    SessionLaunchSlot {
        layer_id: LayerId,
        scene_id: SceneId,
    },
    /// Quantized stop for one layer's session slot. `session_override`
    /// persists — the layer goes black, it does not fall back to the
    /// arrangement.
    #[allow(dead_code)]
    SessionStopSlot {
        layer_id: LayerId,
    },
    /// Launch every slot in a scene; layers currently playing a session slot
    /// with no slot in this scene get a quantized stop (Ableton "stop other
    /// tracks" default).
    #[allow(dead_code)]
    SessionLaunchScene {
        scene_id: SceneId,
    },
    /// Quantized stop of every currently-playing (or about-to-play) session
    /// slot. Distinct from a full transport stop: `session_override` is
    /// untouched.
    #[allow(dead_code)]
    SessionStopAll,
    /// Immediate (not quantized): clears `session_override` for the layer
    /// (or every layer if `None`) and stops its playing slot. Timeline
    /// clips resume immediately.
    #[allow(dead_code)]
    SessionBackToArrangement {
        layer_id: Option<LayerId>,
    },
    /// Set the global session launch quantize (0 = launch immediately).
    #[allow(dead_code)]
    SessionSetQuantize {
        beats: Beats,
    },

    // ── Automation lanes ──
    /// Automation lanes' "Back to Arrangement": clears every override latch
    /// (one global action, not per-layer — lights up red in the transport
    /// bar when any latch is set), resuming every automated param's lane.
    /// Mutates runtime latch state, not the project, so this is a
    /// `ContentCommand` (like `SessionBackToArrangement`) rather than an
    /// `EditingService` command — no undo entry. Sent by the transport bar's
    /// BACK button (`PanelAction::AutomationBackToArrangement` →
    /// `ui_bridge::transport::dispatch_transport`).
    AutomationBackToArrangement,
    /// Resume one parameter without disturbing other live overrides.
    AutomationResumeParameter(manifold_core::GraphTarget, manifold_core::effects::ParamId),
    /// Toggle the global Automation Arm (section 5): while on, touching an
    /// automated param (while playing) records into its lane instead of
    /// latching an override. Runtime-only state, same shape as
    /// `AutomationBackToArrangement` — no undo entry. Sent by the transport
    /// bar's ARM button (`PanelAction::ToggleAutomationArm`).
    AutomationSetArmed(bool),
    /// A pointer pressed (`held`) or let go of a param slider. While armed, a
    /// held param keeps recording its value while still; release ends its take.
    AutomationHold {
        target: manifold_core::GraphTarget,
        param_id: manifold_core::effects::ParamId,
        held: bool,
    },

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
        /// Pasted clip ids, plus why any clips were skipped.
        result_tx: std::sync::mpsc::Sender<(Vec<ClipId>, Option<String>)>,
    },

    // ── Direct project mutation ────────────────────────────────────
    /// A non-undoable write through `EditingService::mutate` (D3). `touches`
    /// names every part the closure writes; the reaction pass reacts to it
    /// once per drain. A live value write declares `Values`; a live drag of a
    /// modulator, gain, crossover or mapping shape declares `Shapes`; neither
    /// part has reactions (D21). A closure that cannot name its parts should
    /// be a command.
    MutateProject {
        touches: ChangeSet,
        f: Box<dyn FnOnce(&mut Project) + Send>,
        /// A gesture's live move: rows wait for its commit
        /// (`EditingService::mutate_gesture`).
        gesture: bool,
    },

    // ── Percussion ─────────────────────────────────────────────────
    /// Run per-clip detection on an existing audio clip (audio-clip-detection).
    /// Analyzes the clip's file and places its triggers, owned by the clip.
    /// Run per-clip detection on an audio clip. Produced by the inspector Detect
    /// button (P4).
    DetectClip(ClipId),
    /// Re-place a clip's triggers from its cached analysis (no backend run).
    /// Driven by the inspector's live config knobs (per-instrument UI is a P4
    /// follow-up; the command + content path are in place).
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
    /// Update one destination after a screen or available-brightness change.
    UpdateDisplayCapabilities {
        destination: manifold_compositor::presentation::DisplayDestination,
        capabilities: manifold_compositor::presentation::DisplayCapabilities,
    },

    // ── Output surface (direct present from content thread) ─────
    /// Attach an output surface for direct-to-drawable presentation.
    SetOutputSurface(manifold_gpu::GpuSurface),
    /// Detach the output surface (output window closed).
    ClearOutputSurface,
    /// Resize the output surface drawable (fullscreen toggle).
    ResizeOutputSurface(u32, u32),
    /// Suspend/resume direct present during display retarget.
    SetOutputPresentSuspended(bool),

    // ── Export ────────────────────────────────────────────────────
    /// Begin offline video export. Content thread enters export loop.
    StartExport(Box<ExportConfig>),
    /// Cancel in-progress export at the next frame boundary. Sent by the
    /// export modal's Cancel button or Escape; cleanup runs on this thread.
    CancelExport,
    /// Export the current composited frame as a still image. Captured across
    /// two content ticks (readback submit → read) so the live render never
    /// stalls; the encode + disk write then run off-thread. See
    /// `ContentThread::poll_still_export`.
    ExportFrame {
        path: String,
        format: manifold_media::still_exporter::StillFormat,
    },
    /// Grab the frame on screen now and hold it for a following `ExportFrame`
    /// (sent when the save dialog closes).
    CaptureStill,
    /// Drop a held capture — the save dialog was cancelled.
    DiscardStill,

    // ── Live Recording ───────────────────────────────────────────
    /// Start live recording. Captures output frames + optional audio.
    StartLiveRecording(Box<manifold_recording::LiveRecordingConfig>),
    /// Stop live recording and finalize the output file.
    StopLiveRecording,

    // ── GPU events ────────────────────────────────────────────────
    /// GPU finished with a surface — wake the content thread if it's
    /// blocked in `recv()` waiting for surface readiness.
    /// Sent from the GPU's completion handlers.
    SurfaceReady,
    /// The stage output's drawable went on screen at `at` (Metal's presented
    /// handler, host time mapped to `Instant`). Feeds the screen delay (D19).
    /// Only Metal reports presents so far.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    FramePresented {
        drawable_id: u64,
        at: std::time::Instant,
    },

    // ── Sync calibration (D19) ────────────────────────────────────
    /// Stored values from `UserPrefs`, sent before the first tick: the hidden
    /// extra output delay in ms, and the learned offsets in ms for MIDI
    /// clock, AbletonOSC and timecode (NaN where none is stored).
    SetSyncCalibration {
        extra_delay_ms: f64,
        offsets_ms: [f64; 3],
    },

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
    /// Watch any authored graph, including an applied modifier's local snapshot.
    WatchGraphTarget(Option<manifold_core::GraphTarget>),

    /// Set the node whose output the graph editor is previewing, within the
    /// currently-watched effect/generator. `None` clears the preview. The
    /// content thread combines this with `WatchEffectGraph` /
    /// `WatchGeneratorGraph` to drive the per-node output capture. Sent when
    /// the editor's node selection changes.
    SetGraphPreviewNode(Option<manifold_core::NodeId>),
    SetSceneViewport(Option<std::sync::Arc<crate::scene_viewport::SceneViewportRequest>>),
    /// The scene layer whose rendered camera, lights and fluid domains are
    /// reported for the Scene panel's gizmo overlay; `None` stops reporting.
    SetPhysicsOverlay(Option<manifold_core::LayerId>),
    /// Draw Again: a scene layer stopped by runaway GPU frames draws again.
    DrawLayerAgain(manifold_core::LayerId),
    /// The render slots the selection outline covers (D-102) in the output
    /// render and in the scene viewport, each with the layer it was read
    /// from. `None` draws none.
    SetSelectionOutline {
        output: Option<crate::selection_outline::OutlineSlots>,
        viewport: Option<crate::selection_outline::OutlineSlots>,
    },
    /// A click on a scene picture: select the item drawn there
    /// (SCENE_DOCUMENT_DESIGN.md section 7.14).
    PickSceneItem(crate::scene_pick::ScenePick),
    /// The gizmo lines to draw over the output preview and the scene viewport,
    /// and the draw mode for them and for an export (BUG-b36b8 gizmo draw modes).
    SetGizmoOverlay(Box<crate::gizmo_overlay::GizmoOverlay>),
    SetModifierPreviewContext {
        object: Option<manifold_core::NodeId>,
    },

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

    /// The set of clips that currently want a timeline thumbnail (section 24 5c) —
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

/// The change sets `MutateProject` senders declare, named once.
pub mod touch {
    use manifold_core::change::{ChangeSet, Part};
    pub const VALUES: ChangeSet = ChangeSet::of(&[Part::Values]);
    pub const SHAPES: ChangeSet = ChangeSet::of(&[Part::Shapes]);
    /// A live graph edit: a node param or mapping reshape (Shapes), which a
    /// card-owned param redirects to the card value (Values).
    pub const GRAPH_LIVE: ChangeSet = ChangeSet::of(&[Part::Shapes, Part::Values]);
    /// The first move of that edit may also materialize the override (D21).
    pub const OVERRIDES_SHAPES: ChangeSet =
        ChangeSet::of(&[Part::Overrides, Part::Shapes, Part::Values]);
    pub const MAPPING_RANGES: ChangeSet = ChangeSet::of(&[Part::MappingRanges]);
    pub const MAPPINGS: ChangeSet = ChangeSet::of(&[Part::Mappings]);
    pub const LAYERS: ChangeSet = ChangeSet::of(&[Part::Layers]);
    pub const MARKERS: ChangeSet = ChangeSet::of(&[Part::Markers]);
    pub const EFFECTS: ChangeSet = ChangeSet::of(&[Part::Effects]);
    /// Adding or removing a macro mapping also moves its range (Shapes).
    pub const EFFECTS_SHAPES: ChangeSet = ChangeSet::of(&[Part::Effects, Part::Shapes]);
    pub const LED: ChangeSet = ChangeSet::of(&[Part::Led]);
    pub const MEDIA: ChangeSet = ChangeSet::of(&[Part::Media]);

    /// `OVERRIDES_SHAPES` when `graph_absent` (the write may materialize the
    /// override), else `GRAPH_LIVE`.
    pub fn shapes_first_move(graph_absent: bool) -> ChangeSet {
        if graph_absent {
            OVERRIDES_SHAPES
        } else {
            GRAPH_LIVE
        }
    }
}

impl ContentCommand {
    /// Send a command to the content thread. Logs on failure (channel disconnected = shutdown).
    pub fn send(tx: &crossbeam_channel::Sender<ContentCommand>, cmd: ContentCommand) {
        if let Err(e) = tx.send(cmd) {
            log::error!("[UI] Content command channel disconnected: {e}");
        }
    }

    /// A `MutateProject` writing `touches`.
    pub fn mutate(touches: ChangeSet, f: Box<dyn FnOnce(&mut Project) + Send>) -> Self {
        ContentCommand::MutateProject {
            touches,
            f,
            gesture: false,
        }
    }

    /// A gesture's live move writing `touches`; its commit reports rows.
    pub fn gesture_move(touches: ChangeSet, f: Box<dyn FnOnce(&mut Project) + Send>) -> Self {
        ContentCommand::MutateProject {
            touches,
            f,
            gesture: true,
        }
    }
}
