//! Export functionality for ContentThread — extracted from content_thread.rs.
//! Contains `run_export`, `export_one_frame`,
//! `send_export_progress`, and `send_export_finished`.

use std::sync::Arc;

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
use std::cell::{Cell, RefCell};

use crossbeam_channel::Receiver;

use manifold_core::{Beats, Seconds};
use manifold_playback::engine::PlaybackEngine;

use crate::content_command::ContentCommand;
use crate::content_state::{ContentState, ExportFinishedEvent, StateTx};
use crate::content_thread::ContentThread;

use manifold_gpu::gpu_fault::{FaultKind, FaultKinds};

/// Header metadata for an exported file: how it was made. `title` and
/// `software` are standard fields; `com.manifold.*` keys are custom. Only the
/// global project settings are recorded, nothing per layer. RT quality is the
/// export column, since that is what an export renders with.
pub(crate) fn export_metadata(
    project: &manifold_core::project::Project,
    config: &manifold_media::export_config::ExportConfig,
) -> Vec<(String, String)> {
    use manifold_core::settings::{RtQualityTier, RtRayResolution, RtSpatialDenoise};
    use manifold_core::types::TonemapCurve;

    fn tier(t: RtQualityTier) -> &'static str {
        match t {
            RtQualityTier::UltraLow => "Ultra Low",
            RtQualityTier::Low => "Low",
            RtQualityTier::Medium => "Medium",
            RtQualityTier::High => "High",
            RtQualityTier::ExtraHigh => "Extra High",
            RtQualityTier::Ultra => "Ultra",
        }
    }
    let rt = &project.settings.rt_quality.export;
    let ray_res = match rt.ray_resolution {
        RtRayResolution::Quarter => "Quarter",
        RtRayResolution::Half => "Half",
        RtRayResolution::ThreeQuarter => "Three Quarter",
        RtRayResolution::Native => "Native",
    };
    let denoise = match rt.spatial_denoise {
        RtSpatialDenoise::Off => "Off",
        RtSpatialDenoise::Low => "Low",
        RtSpatialDenoise::Medium => "Medium",
        RtSpatialDenoise::High => "High",
    };
    // The curve only applies to SDR output with the tone map switched on.
    let tonemap = if config.hdr || !project.settings.tonemap_enabled {
        "Off"
    } else {
        match project.settings.tonemap_curve {
            TonemapCurve::AcesNarkowicz => "ACES Narkowicz",
            TonemapCurve::AcesHill => "ACES Hill",
            TonemapCurve::Agx => "AgX",
            TonemapCurve::KhronosPbrNeutral => "Khronos PBR Neutral",
        }
    };
    let kv = |k: &str, v: String| (k.to_string(), v);
    let mut out = Vec::new();
    if !project.project_name.is_empty() {
        out.push(kv("title", project.project_name.clone()));
    }
    out.push(kv(
        "software",
        format!("MANIFOLD {}", env!("CARGO_PKG_VERSION")),
    ));
    out.push(kv(
        "com.manifold.resolution",
        format!("{}x{}", config.width, config.height),
    ));
    out.push(kv("com.manifold.fps", format!("{}", config.fps)));
    out.push(kv(
        "com.manifold.dynamic_range",
        if config.hdr { "HDR10" } else { "SDR" }.into(),
    ));
    out.push(kv(
        "com.manifold.codec",
        if config.hdr { "HEVC Main10" } else { "H.264" }.into(),
    ));
    out.push(kv("com.manifold.sdr_tone_map", tonemap.into()));
    out.push(kv("com.manifold.rt_shadows", tier(rt.shadows).into()));
    out.push(kv("com.manifold.rt_ao", tier(rt.ao).into()));
    out.push(kv("com.manifold.rt_gi", tier(rt.gi).into()));
    out.push(kv(
        "com.manifold.rt_reflections",
        tier(rt.reflections).into(),
    ));
    out.push(kv("com.manifold.rt_ray_resolution", ray_res.into()));
    out.push(kv("com.manifold.rt_denoise", denoise.into()));
    out.push(kv(
        "com.manifold.bpm",
        format!("{:.2}", project.settings.bpm.0),
    ));
    out
}

/// How many times a frame is re-rendered after a recoverable GPU hang.
const EXPORT_HANG_RETRIES: u32 = 2;

/// `fatal` is set only when the device or queue can no longer be trusted
/// (page fault, device loss, blacklisted queue); every other failure ends the
/// export but leaves the app running.
struct ExportFrameFailure {
    message: String,
    fatal: bool,
}

/// Before a hang retry, ask the RT tracer for its most conservative tiles so
/// the retried frame is less likely to trip the watchdog again.
fn request_conservative_rt_tiles(
    _pipeline: &mut crate::content_pipeline::ContentPipeline,
    attempt: u32,
) {
    manifold_gpu::raytrace::request_conservative_trace_tiles();
    log::warn!("[Export] Hang retry {attempt}: re-rendering with conservative RT tiles");
}

/// One production export frame immediately before the native encoder call.
/// This is test-only evidence: it observes the already-rendered frame and
/// never evaluates the graph or performs another render submission. The
/// explicit colour-proof opt-in may additionally request one blocking readback
/// of frame 0's already-mapped SDR texture or the canonical HDR texture fed to
/// the PQ encoder.
#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
#[derive(Clone, Debug)]
pub(crate) struct ExportFrameObservation {
    pub frame_idx: u32,
    pub time_seconds: f64,
    pub tick_dt: f64,
    pub status: manifold_node_engine::runtime::frame_status::FrameRenderStatus,
    pub rt_updates: manifold_gpu::raytrace::RtAccelUpdate,
    pub rt_dispatches: u32,
    pub history_resets: u32,
    pub beat: f64,
    pub generator_values: Vec<(String, f32)>,
    pub sdr_mapped_rgba16f: Option<Vec<u8>>,
    pub hdr_scene_rgba16f: Option<Vec<u8>>,
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportTestFault {
    BeforeEncode,
    PendingGeometry,
    Preparation,
    Encode,
    /// Reported as a page fault: fatal.
    GpuFault,
    IgnoredSubmission,
    /// No fault, the fence just never signals: the export fails, the app lives.
    CompletionTimeout,
    /// The first `failing_attempts` renders of the frame hang.
    Hang {
        failing_attempts: u32,
    },
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
thread_local! {
    static EXPORT_OBSERVER: RefCell<Option<crossbeam_channel::Sender<ExportFrameObservation>>> = const { RefCell::new(None) };
    /// The export frame whose SDR output is read back.
    static EXPORT_SDR_READBACK: Cell<Option<u32>> = const { Cell::new(None) };
    static EXPORT_HDR_READBACK: Cell<bool> = const { Cell::new(false) };
    static EXPORT_FAILURE_FRAME: Cell<Option<(u32, ExportTestFault)>> = const { Cell::new(None) };
    static EXPORT_GPU_ABORT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    /// Renders of export frames, retries included.
    static EXPORT_RENDER_ATTEMPTS: Cell<u32> = const { Cell::new(0) };
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) fn export_test_render_attempts() -> u32 {
    EXPORT_RENDER_ATTEMPTS.with(Cell::get)
}

/// Installs the test-only observer and optional pre-encode fault injection for
/// the current content thread. The guard clears the hooks when it
/// is dropped, so separate exports cannot inherit observation state.
#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) struct ExportObservationGuard;

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) fn install_export_observer(
    sender: crossbeam_channel::Sender<ExportFrameObservation>,
    fail_before_encode_frame: Option<(u32, ExportTestFault)>,
) -> ExportObservationGuard {
    install_export_observer_with_export_readbacks(sender, fail_before_encode_frame, false, false)
}

/// Install the observer with explicit first-frame readback of already-rendered
/// export textures. This remains test-only; normal observers perform no GPU
/// readback and no production allocation.
#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) fn install_export_observer_with_export_readbacks(
    sender: crossbeam_channel::Sender<ExportFrameObservation>,
    fail_before_encode_frame: Option<(u32, ExportTestFault)>,
    capture_first_sdr_frame: bool,
    capture_first_hdr_frame: bool,
) -> ExportObservationGuard {
    EXPORT_OBSERVER.with(|slot| {
        assert!(slot.borrow().is_none(), "export observer already installed");
        *slot.borrow_mut() = Some(sender);
    });
    EXPORT_SDR_READBACK.with(|capture| capture.set(capture_first_sdr_frame.then_some(0)));
    EXPORT_HDR_READBACK.with(|capture| capture.set(capture_first_hdr_frame));
    EXPORT_FAILURE_FRAME.with(|frame| frame.set(fail_before_encode_frame));
    EXPORT_GPU_ABORT_REQUESTED.with(|requested| requested.set(false));
    EXPORT_RENDER_ATTEMPTS.with(|count| count.set(0));
    ExportObservationGuard
}

/// Read back the SDR output of export frame `frame_idx` instead of the first;
/// call after installing the observer.
#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) fn capture_export_sdr_frame(frame_idx: u32) {
    EXPORT_SDR_READBACK.with(|capture| capture.set(Some(frame_idx)));
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
impl Drop for ExportObservationGuard {
    fn drop(&mut self) {
        EXPORT_OBSERVER.with(|slot| *slot.borrow_mut() = None);
        EXPORT_SDR_READBACK.with(|capture| capture.set(None));
        EXPORT_HDR_READBACK.with(|capture| capture.set(false));
        EXPORT_FAILURE_FRAME.with(|frame| frame.set(None));
    }
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
fn observe_export_frame(observation: ExportFrameObservation) {
    EXPORT_OBSERVER.with(|slot| {
        if let Some(sender) = slot.borrow().as_ref() {
            let _ = sender.send(observation);
        }
    });
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
fn export_test_fault(frame_idx: u32) -> Option<ExportTestFault> {
    EXPORT_FAILURE_FRAME.with(|frame| {
        frame
            .get()
            .filter(|(index, _)| *index == frame_idx)
            .map(|(_, fault)| fault)
    })
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
pub(crate) fn export_test_gpu_abort_requested() -> bool {
    EXPORT_GPU_ABORT_REQUESTED.with(Cell::get)
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
fn export_sdr_readback_requested(frame_idx: u32) -> bool {
    EXPORT_SDR_READBACK.with(Cell::get) == Some(frame_idx)
}

#[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
fn export_hdr_readback_requested(frame_idx: u32) -> bool {
    frame_idx == 0 && EXPORT_HDR_READBACK.with(Cell::get)
}

/// Why a frame's GPU work failed. `kinds` is empty for a plain timeout.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExportGpuFailure {
    pub message: String,
    pub kinds: FaultKinds,
}

/// A signalled fence is not success if any GPU work failed during the frame.
/// `kinds` are the fault kinds recorded since the frame's fault mark.
pub(crate) fn export_gpu_completion(
    completed: bool,
    initial_faults: u64,
    current_faults: u64,
    kinds: FaultKinds,
    submissions_ignored: bool,
    timed_out: bool,
) -> Option<Result<(), ExportGpuFailure>> {
    if current_faults != initial_faults || submissions_ignored {
        let kinds = if submissions_ignored {
            kinds.with(FaultKind::SubmissionsIgnored)
        } else {
            kinds
        };
        let kinds = if kinds.is_empty() {
            kinds.with(FaultKind::Other)
        } else {
            kinds
        };
        let cause = kinds.headline().map_or("GPU error", FaultKind::describe);
        Some(Err(ExportGpuFailure {
            message: format!("{cause} during export; see session encoder diagnostics"),
            kinds,
        }))
    } else if completed {
        Some(Ok(()))
    } else if timed_out {
        Some(Err(ExportGpuFailure {
            message: "GPU completion timed out during export".into(),
            kinds: FaultKinds::default(),
        }))
    } else {
        None
    }
}

#[cfg(test)]
mod gpu_completion_tests {
    use super::export_gpu_completion;
    use manifold_gpu::gpu_fault::{FaultKind, FaultKinds};

    fn none() -> FaultKinds {
        FaultKinds::default()
    }

    #[test]
    fn gpu_failure_overrides_signalled_fence() {
        let page = none().with(FaultKind::PageFault);
        assert!(
            export_gpu_completion(true, 4, 5, page, false, false)
                .unwrap()
                .unwrap_err()
                .kinds
                .is_fatal()
        );
        assert!(
            export_gpu_completion(true, 4, 4, none(), true, false)
                .unwrap()
                .unwrap_err()
                .kinds
                .is_fatal()
        );
    }

    #[test]
    fn hang_is_recoverable_and_named() {
        let failure = export_gpu_completion(true, 0, 1, none().with(FaultKind::Hang), false, false)
            .unwrap()
            .unwrap_err();
        assert!(failure.kinds.is_recoverable_hang());
        assert!(failure.message.contains("GPU hang"), "{}", failure.message);
    }

    #[test]
    fn unclassified_fault_is_neither_fatal_nor_retried() {
        let failure = export_gpu_completion(true, 0, 1, none(), false, false)
            .unwrap()
            .unwrap_err();
        assert!(!failure.kinds.is_fatal() && !failure.kinds.is_recoverable_hang());
    }

    #[test]
    fn unfinished_frame_waits_then_fails_at_deadline() {
        assert!(export_gpu_completion(false, 0, 0, none(), false, false).is_none());
        let timeout = export_gpu_completion(false, 0, 0, none(), false, true)
            .unwrap()
            .unwrap_err();
        assert!(!timeout.kinds.is_fatal() && !timeout.kinds.is_recoverable_hang());
        assert_eq!(
            export_gpu_completion(true, 0, 0, none(), false, true),
            Some(Ok(()))
        );
    }
}

/// Derive export sections from timeline markers. Every marker inside the
/// export range is a cut — sections are `[range_start, m₁)`, `[m₁, m₂)`, …,
/// `[mₙ, range_end)` over the sorted, deduplicated markers strictly inside
/// the range. Each section is named by the marker at its start; the first
/// section (starting at `range_start`) carries the name of a marker exactly
/// on `range_start` when one exists, and an empty name otherwise — which the
/// filename logic turns into `section-N`.
///
/// A marker exactly on `range_start` does not cut: it names the first section
/// (so Peter can put a marker at the export-in point to name the intro).
/// A marker exactly on `range_end` is excluded (it sits outside the half-open
/// range). Returns empty when there are no relevant markers (no marker on
/// `range_start`, none strictly inside) → the caller takes the single-export
/// path.
fn derive_sections(
    timeline: &manifold_core::timeline::Timeline,
    range_start: Beats,
    range_end: Beats,
) -> Vec<(Beats, Beats, String)> {
    let mut cuts: Vec<(Beats, String)> = timeline
        .markers
        .iter()
        .filter(|m| m.beat > range_start && m.beat < range_end)
        .map(|m| (m.beat, m.name.clone()))
        .collect();
    cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Duplicate beats collapse to one cut (a second cut at the same beat
    // would produce an empty section); the stable sort keeps the first name.
    cuts.dedup_by(|a, b| a.0 == b.0);

    // A marker exactly on `range_start` names the first section (doesn't
    // cut). First in list order wins when several share the beat.
    let first_name = timeline
        .markers
        .iter()
        .filter(|m| m.beat == range_start)
        .map(|m| m.name.clone())
        .next();

    if cuts.is_empty() && first_name.is_none() {
        return Vec::new();
    }

    let mut boundaries: Vec<(Beats, String)> = Vec::with_capacity(cuts.len() + 1);
    boundaries.push((range_start, first_name.unwrap_or_default()));
    boundaries.extend(cuts);

    let mut sections = Vec::with_capacity(boundaries.len());
    for window in boundaries.windows(2) {
        sections.push((window[0].0, window[1].0, window[0].1.clone()));
    }
    let last = boundaries.last().expect("boundaries is non-empty");
    sections.push((last.0, range_end, last.1.clone()));
    sections
}

/// Sanitize a marker name into a filename-safe stem: every run of
/// non-alphanumeric characters (whitespace, punctuation) collapses to a
/// single `-`, with leading/trailing dashes trimmed. See D6.
fn sanitize_section_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Per-section output paths from a base `output_path`. Each section's name
/// is sanitized (empty → `section-N` counting from 1); duplicate stems get
/// `-2`, `-3`, … suffixes. The base file's directory and extension are
/// preserved: `<base>--<stem>.<ext>`. See D6.
fn section_output_paths(base_output: &str, sections: &[(Beats, Beats, String)]) -> Vec<String> {
    let path = std::path::Path::new(base_output);
    let dir = path
        .parent()
        .map(|d| format!("{}/", d.display()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| base_output.to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    sections
        .iter()
        .enumerate()
        .map(|(i, (_, _, name))| {
            let mut key = sanitize_section_name(name);
            if key.is_empty() {
                key = format!("section-{}", i + 1);
            }
            let count = used.entry(key.clone()).or_insert(0);
            *count += 1;
            let suffixed = if *count == 1 {
                key
            } else {
                format!("{key}-{}", *count)
            };
            format!("{dir}{stem}--{suffixed}{ext}")
        })
        .collect()
}

/// What an export does with a command that arrives while it renders
/// (BUG-v2o9y: nothing may change during an export).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportDisposition {
    /// Stop the export.
    Cancel,
    /// Stop the export, then quit.
    Quit,
    /// Run after the export ends: only what is viewed, or window and preview
    /// plumbing the UI sends once and never resends.
    Hold,
    /// Throw away: it can change the show, what plays, records or exports.
    Drop,
}

/// The one place every command is classified. No wildcard arm: a new
/// `ContentCommand` does not compile until it is named here. When one is
/// ambiguous it is Dropped if it can change what the show outputs, else Held.
fn export_disposition(cmd: &ContentCommand) -> ExportDisposition {
    use ExportDisposition::{Cancel, Drop, Hold, Quit};
    match cmd {
        ContentCommand::CancelExport => Cancel,
        ContentCommand::Shutdown => Quit,

        // Window, surface and preview plumbing.
        ContentCommand::ResizeWorkspacePreview(..)
        | ContentCommand::UpdateDisplayCapabilities { .. }
        | ContentCommand::SetOutputSurface(_)
        | ContentCommand::ClearOutputSurface
        | ContentCommand::ResizeOutputSurface(..)
        | ContentCommand::SetOutputPresentSuspended(_)
        | ContentCommand::SurfaceReady
        | ContentCommand::FramePresented { .. }
        | ContentCommand::MarkCompositorDirty => Hold,

        // What the editors and previews look at. The runtime-only SDR
        // preview is a view of the output, not the output.
        ContentCommand::WatchEffectGraph(_)
        | ContentCommand::WatchGeneratorGraph(_)
        | ContentCommand::WatchGraphTarget(_)
        | ContentCommand::SetGraphPreviewNode(_)
        | ContentCommand::SetSceneViewport(_)
        | ContentCommand::SetPhysicsOverlay(_)
        | ContentCommand::DrawLayerAgain(_)
        | ContentCommand::SetSelectionOutline { .. }
        | ContentCommand::PickSceneItem(_)
        | ContentCommand::SetGizmoOverlay(_)
        | ContentCommand::SetModifierPreviewContext { .. }
        | ContentCommand::SetNodePreviewNormalize(_)
        | ContentCommand::SetNodeAtlasVisible(_)
        | ContentCommand::SetClipAtlasVisible(_)
        | ContentCommand::DumpGraphOutputs
        | ContentCommand::SetSdrPreview(_) => Hold,

        // No effect on the show: notices, clipboard copy, the clean flag (a
        // save cannot run during an export), the autosave snapshot request,
        // sync calibration prefs, profiling.
        ContentCommand::GraphEditRejected(_)
        | ContentCommand::CopyClips { .. }
        | ContentCommand::MarkClean
        | ContentCommand::MarkCleanAt(_)
        | ContentCommand::PrepareProjectSave(_)
        | ContentCommand::SetSyncCalibration { .. } => Hold,
        #[cfg(feature = "profiling")]
        ContentCommand::StartProfiling { .. } | ContentCommand::StopProfiling => Hold,

        // Transport, playback, recording and sync.
        ContentCommand::Play
        | ContentCommand::Pause
        | ContentCommand::Stop
        | ContentCommand::SeekTo(_)
        | ContentCommand::SeekToBeat(_)
        | ContentCommand::SetRecording(_)
        | ContentCommand::FinishAutomationRecording
        | ContentCommand::StartLiveRecording(_)
        | ContentCommand::StopLiveRecording
        | ContentCommand::ToggleLink
        | ContentCommand::ToggleMidiClock
        | ContentCommand::SetMidiClockDevice(_)
        | ContentCommand::ResetBpm
        | ContentCommand::ToggleOscSyncMode
        | ContentCommand::AbletonMapParam { .. }
        | ContentCommand::AbletonUnmapParam { .. }
        | ContentCommand::AbletonRediscover => Drop,

        // Sessions and automation.
        ContentCommand::SessionLaunchSlot { .. }
        | ContentCommand::SessionStopSlot { .. }
        | ContentCommand::SessionLaunchScene { .. }
        | ContentCommand::SessionStopAll
        | ContentCommand::SessionBackToArrangement { .. }
        | ContentCommand::SessionSetQuantize { .. }
        | ContentCommand::PreviewAutomationLane { .. }
        | ContentCommand::ClearAutomationPreviews
        | ContentCommand::AutomationBackToArrangement
        | ContentCommand::AutomationResumeParameter(..)
        | ContentCommand::AutomationSetArmed(_)
        | ContentCommand::AutomationHold { .. } => Drop,

        // Edits, undo and the project itself.
        ContentCommand::Execute(_)
        | ContentCommand::FireParameter { .. }
        | ContentCommand::ExecuteSelecting(..)
        | ContentCommand::ChangeGeneratorType { .. }
        | ContentCommand::GeneratorTypeChanged { .. }
        | ContentCommand::ExecuteBatch(..)
        | ContentCommand::MutateProject { .. }
        | ContentCommand::PasteClips { .. }
        | ContentCommand::DetectClip(_)
        | ContentCommand::ReplanClip(_)
        | ContentCommand::ClearClipTriggers(_)
        | ContentCommand::Undo
        | ContentCommand::Redo
        | ContentCommand::SetProject
        | ContentCommand::LoadProject(_) => Drop,

        // Show and output settings: each can change what the show outputs.
        ContentCommand::SetFrameRate(_)
        | ContentCommand::SetOutputSize(..)
        | ContentCommand::SetRenderScale(_)
        | ContentCommand::SetTonemapCurve(_)
        | ContentCommand::SetTonemapEnabled(_)
        | ContentCommand::ResizeContent(..)
        | ContentCommand::SetSpectrogramSend(_)
        | ContentCommand::InitLedOutput(_)
        | ContentCommand::ShutdownLedOutput => Drop,

        // A second export, or a capture, mid-export.
        ContentCommand::StartExport(_)
        | ContentCommand::ExportFrame { .. }
        | ContentCommand::CaptureStill
        | ContentCommand::DiscardStill => Drop,
    }
}
impl ContentThread {
    /// True when the export must stop: a cancel, or a quit. The quit is held
    /// so it proceeds after the cancelled export unwinds. Every other command
    /// is classified by [`export_disposition`]: held to run after the export,
    /// or dropped, so the show cannot change while it renders (BUG-v2o9y).
    fn poll_export_cancel(&mut self, cmd_rx: &Receiver<ContentCommand>) -> bool {
        // A cancel or quit can already be held, sent during the warmup the
        // export was queued behind.
        if self.quit_is_held() || self.take_held_export_cancel() {
            return true;
        }
        let mut dropped = 0u32;
        let mut stop = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match export_disposition(&cmd) {
                ExportDisposition::Cancel => {
                    stop = true;
                    break;
                }
                ExportDisposition::Quit => {
                    self.held_commands.push_back(cmd);
                    stop = true;
                    break;
                }
                ExportDisposition::Hold => self.hold_command(cmd),
                ExportDisposition::Drop => dropped += 1,
            }
        }
        if dropped > 0 {
            log::debug!("[ContentThread] export dropped {dropped} command(s)");
        }
        stop
    }

    /// Run the offline video export loop.
    ///
    /// Temporarily replaces the normal content loop: ticks the engine with fixed
    /// delta, renders each frame, and encodes via the platform's video encoder at
    /// maximum GPU speed (no frame pacing / sleep).
    ///
    /// With `split_at_markers`, this runs one full export per derived
    /// section, sequentially. A cancelled or
    /// failed section aborts the remaining sections.
    ///
    /// Port of Unity VideoExporter.ExportCoroutine() (offline / generator-only path).
    pub(crate) fn run_export(
        &mut self,
        config: manifold_media::export_config::ExportConfig,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &StateTx,
    ) {
        self.engine.clear_automation_previews();
        // Stop any active live recording before entering export.
        #[cfg(target_os = "macos")]
        if let Some(session) = self.content_pipeline.recording_session.take() {
            log::warn!("[ContentThread] Stopping active recording before export");
            let _ = session.stop();
        }
        self.send_export_phase(state_tx, true, 0.0, "Preparing export...", false);
        // A scene viewport (the editor's or the preview's navigation) is a
        // second render of the scene the export does not need: it sits out
        // the export and resumes after it. A new request sent meanwhile is
        // held and runs after this one is put back, so the newest still wins.
        let viewport = self.content_pipeline.take_scene_viewport_request();
        // The selection outline's mask pass is editing chrome: it sits out
        // the export the same way.
        let (outline_output, outline_viewport) = self.content_pipeline.take_selection_outline();
        // The gizmos an export includes are the Scene panel scene's, drawn
        // in the Scene panel's draw mode.
        let gizmos = config
            .include_gizmos
            .then(|| self.content_pipeline.begin_export_gizmos());
        self.run_export_inner(config, cmd_rx, state_tx);
        if let Some(previous) = gizmos {
            self.content_pipeline.end_export_gizmos(previous);
        }
        self.content_pipeline.set_scene_viewport_request(viewport);
        self.content_pipeline
            .set_selection_outline(outline_output, outline_viewport);
        // Edits sent while it ran were dropped, but a direct UI write may
        // still show one: publish the project once so the UI copy is replaced
        // by this thread's (the existing publish_pending path, no data_version bump).
        self.reactions.publish_pending = true;
        self.send_export_phase(state_tx, false, 1.0, "Export finished", true);
    }

    fn run_export_inner(
        &mut self,
        config: manifold_media::export_config::ExportConfig,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &StateTx,
    ) {
        log::info!("[ContentThread] Starting export: {:?}", config);

        self.commit_automation_recording(true);
        // 1. Save playback state for restore
        let restore = self.engine.export_restore_point();

        // 2. Resolve export range
        let Some(project) = self.engine.project() else {
            log::error!("[ContentThread] No project loaded, cannot export");
            self.send_export_finished(
                state_tx,
                false,
                "No project loaded".into(),
                &config.output_path,
            );
            return;
        };
        let bpm = project.settings.bpm;
        let (content_start, content_end) = project.timeline.content_range_beats();
        let content_start = content_start.0;
        let content_end = content_end.0;

        // Use config beats if set, otherwise use content range
        let start_beat = if config.start_beat > 0.0 {
            config.start_beat
        } else {
            content_start
        };
        let end_beat = if config.end_beat > 0.0 {
            config.end_beat
        } else {
            content_end
        };

        if start_beat >= end_beat || content_start >= content_end {
            log::error!("[ContentThread] No content in export range ({start_beat}..{end_beat})");
            self.send_export_finished(
                state_tx,
                false,
                "No content in export range".into(),
                &config.output_path,
            );
            return;
        }

        // Build base config with resolved range + audio info from content thread
        let mut base_config = config;
        base_config.start_beat = start_beat;
        base_config.end_beat = end_beat;

        // Derive sections from timeline markers. Empty when the setting is off or
        // no markers fall inside the range → single-export path below.
        let sections: Vec<(Beats, Beats, String)> = if base_config.split_at_markers {
            derive_sections(&project.timeline, Beats(start_beat), Beats(end_beat))
        } else {
            Vec::new()
        };

        // Enter export mode + resize once (resolution is constant across sections).
        self.engine.stop();
        self.engine.set_export_mode(true);
        let (cur_w, cur_h) = self.content_pipeline.dimensions();
        // Always request the export configuration. Output dimensions can stay
        // equal while the live render scale is below 1.0, in which case a
        // dimensions-only guard would leave the export at the wrong scale.
        if let Err(message) = self.content_pipeline.resize(
            &mut self.engine,
            base_config.width,
            base_config.height,
            1.0,
        ) {
            log::error!("[ContentThread] Export resize failed: {message}");
            self.engine.end_export_run(restore);
            self.send_export_finished(
                state_tx,
                false,
                format!("Export failed: resize rejected: {message}"),
                &base_config.output_path,
            );
            return;
        }

        let section_count = sections.len();
        if section_count == 0 {
            // Single export — today's behavior, unmodified output path.
            self.run_export_section(base_config.clone(), bpm, None, cmd_rx, state_tx);
        } else {
            let paths = section_output_paths(&base_config.output_path, &sections);
            for (i, ((start, end, _name), path)) in sections.iter().zip(paths.iter()).enumerate() {
                let mut sc = base_config.clone();
                sc.output_path = path.clone();
                sc.start_beat = start.0;
                sc.end_beat = end.0;
                // D8: audio per section uses the existing mux path — each
                // section's audio_start_beat is its start, which the muxer
                // turns into a zero-offset slice of the master audio.
                sc.audio_start_beat = start.0;
                let prefix = format!("section {} of {}", i + 1, section_count);
                let aborted = self.run_export_section(sc, bpm, Some(&prefix), cmd_rx, state_tx);
                if aborted {
                    log::info!(
                        "[ContentThread] Section export aborted — stopping remaining sections"
                    );
                    break;
                }
            }
        }

        // Restore playback state (once, after all sections).
        self.send_export_phase(state_tx, true, 1.0, "Restoring playback...", false);

        // Restore unconditionally: the saved output dimensions can match the
        // export dimensions while the saved live render scale differs.
        let render_scale = self
            .engine
            .project()
            .map_or(1.0, |p| p.settings.render_scale);
        if let Err(message) =
            self.content_pipeline
                .resize(&mut self.engine, cur_w, cur_h, render_scale)
        {
            log::error!("[ContentThread] Export restore resize failed: {message}");
            // Keep the engine stopped: resuming with a mismatched pipeline
            // would produce frames under an unverified render configuration.
            self.engine.stop();
            self.engine.set_export_mode(false);
            self.send_export_finished(
                state_tx,
                false,
                format!("Export failed: could not restore output size: {message}"),
                &base_config.output_path,
            );
            return;
        }
        self.engine.end_export_run(restore);
    }

    fn send_export_phase(
        &self,
        state_tx: &StateTx,
        is_exporting: bool,
        export_progress: f32,
        status: &str,
        export_run_finished: bool,
    ) {
        let state = ContentState {
            is_exporting,
            export_progress,
            export_status: Arc::from(status),
            export_run_finished,
            current_beat: self.engine.current_beat(),
            current_time: self.engine.current_time(),
            is_playing: self.engine.is_playing(),
            ..ContentState::default()
        };
        state_tx.publish(state);
    }

    /// Run one export pass for a single (possibly section) range — the original
    /// single-export body from timing through finalize. The caller owns playback
    /// save/restore and the export-mode / resize lifecycle; this does the
    /// per-range work. Returns `true` when the pass aborted (cancelled or
    /// failed) so the caller stops any remaining sections.
    fn run_export_section(
        &mut self,
        mut export_config: manifold_media::export_config::ExportConfig,
        bpm: manifold_core::Bpm,
        progress_prefix: Option<&str>,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &StateTx,
    ) -> bool {
        use manifold_core::tempo::TempoMapConverter;
        use manifold_media::audio_muxer::AudioMuxer;

        let status = match progress_prefix {
            Some(prefix) => format!("{prefix} — Preparing export..."),
            None => "Preparing export...".into(),
        };
        self.send_export_phase(state_tx, true, 0.0, &status, false);

        // Re-fetch the project (the caller's borrow ended before entering
        // export mode). Defensive: the caller already resolved it.
        let Some(project) = self.engine.project() else {
            log::error!("[ContentThread] No project loaded, cannot export");
            self.send_export_finished(
                state_tx,
                false,
                "No project loaded".into(),
                &export_config.output_path,
            );
            return true;
        };

        export_config.metadata = export_metadata(project, &export_config);
        let start_beat = export_config.start_beat;
        let end_beat = export_config.end_beat;

        // Calculate timing
        let mut tempo_map = project.tempo_map.clone();
        let start_seconds =
            TempoMapConverter::beat_to_seconds(&mut tempo_map, Beats(start_beat), bpm);
        let end_seconds = TempoMapConverter::beat_to_seconds(&mut tempo_map, Beats(end_beat), bpm);
        let duration = end_seconds - start_seconds;
        let total_frames = (duration * export_config.fps).0.round() as u32;
        let frame_dt = 1.0 / export_config.fps as f64;

        if total_frames == 0 {
            log::error!("[ContentThread] Zero frames to export");
            self.send_export_finished(
                state_tx,
                false,
                "Zero frames to export".into(),
                &export_config.output_path,
            );
            return true;
        }

        // Render the audio-layer mix for the export range into a temp WAV, then
        // wire it as the export's audio track. Mirrors live playback exactly
        // (warp / gain / solo); see manifold_playback::audio_mixdown. Aligned to
        // the export start, so audio_start_beat = start_beat → mux offset 0.
        //
        // The same render
        // also produces the mono buffers the offline audio-mod driver
        // analyzes — "one render, two consumers, no drift between what is
        // heard and what is analyzed" (design seam brief). `tapped_layers` is
        // every layer any consumed send reads (union over
        // `AudioSend::layers()`), so the mixdown renders exactly the taps the
        // driver will need and no more.
        let mut consumed_sends = project.analysis_consumed_sends();
        consumed_sends.extend(crate::audio_visualization::visualizer_consumed_sends(
            project,
        ));
        let mut tapped_layers_set: ahash::AHashSet<manifold_core::id::LayerId> =
            ahash::AHashSet::new();
        for send in &project.audio_setup.sends {
            if consumed_sends.contains(&send.id) {
                tapped_layers_set.extend(send.layers().iter().cloned());
            }
        }
        let tapped_layers: Vec<manifold_core::id::LayerId> =
            tapped_layers_set.into_iter().collect();

        let mix_wav_path = format!("{}.mixdown.wav", export_config.output_path);
        // Declared before `offline_audio_mod` so it outlives the driver that
        // borrows its buffers (Rust drops locals in reverse declaration order).
        let export_audio = match manifold_playback::audio_mixdown::render_export_audio(
            project,
            Beats(start_beat),
            Beats(end_beat),
            bpm,
            &mut tempo_map,
            &tapped_layers,
        ) {
            Ok(audio) => {
                // Byte-identical WAV semantics to the old `render_export_mix`
                // wrapper (P1-guaranteed): same Ok(true)/Ok(false)/Err handling.
                match manifold_playback::audio_mixdown::write_export_wav(&audio, &mix_wav_path) {
                    Ok(true) => {
                        export_config.audio_path = Some(mix_wav_path.clone());
                        export_config.audio_start_beat = start_beat;
                        export_config.audio_encoder_delay = 0.0;
                    }
                    Ok(false) => {
                        log::info!("[Export] No audio-layer clips in range — video-only export");
                    }
                    Err(e) => {
                        log::warn!(
                            "[Export] Audio mixdown WAV write failed ({e}) — exporting video-only"
                        );
                    }
                }
                Some(audio)
            }
            Err(e) => {
                log::warn!("[Export] Audio mixdown failed ({e}) — exporting video-only");
                None
            }
        };
        // The offline audio-mod driver analyzes the rendered buffer directly —
        // independent of whether the WAV muxing above succeeded, since that's
        // a disk-write concern and this is the in-memory render (P2).
        let mut offline_audio_mod = export_audio.as_ref().and_then(|audio| {
            crate::offline_audio_mod::OfflineAudioModDriver::new(
                project,
                audio,
                export_config.fps as f64,
                start_seconds,
            )
        });

        // BUG-130: resolve FFmpeg presence up front, before rendering a single
        // frame. `session.finalize()` used to be the only place this was
        // checked, which meant a machine without ffmpeg installed burned a
        // full (potentially multi-minute) render only to fail at the very
        // last step. Fail fast instead — and reuse the resolved path at
        // finalize time so it isn't re-resolved (and can't disagree).
        let ffmpeg_path = match Self::ffmpeg_preflight(export_config.has_audio(), || {
            AudioMuxer::resolve_ffmpeg("")
        }) {
            Ok(path) => path,
            Err(reason) => {
                log::error!(
                    "[ContentThread] {reason} — aborting export before rendering any frames"
                );
                let _ = std::fs::remove_file(&mix_wav_path);
                self.send_export_finished(
                    state_tx,
                    false,
                    format!("Export failed: {reason}"),
                    &export_config.output_path,
                );
                return true;
            }
        };

        // Detect generator-only projects: no video clips means no decode
        // backpressure needed, enabling faster-than-realtime export.
        // Matches Unity's IsGeneratorOnlyProject() → Time.captureFramerate path.
        let generator_only = project.timeline.layers.iter().all(|layer| {
            layer.is_group() || layer.clips.iter().all(|c| c.video_clip_id.is_empty())
        });
        let mode_label = if generator_only {
            "offline"
        } else {
            "real-time"
        };

        log::info!(
            "[Export] {} mode: {} frames, {:.2}s, beats {:.1}-{:.1}, \
             {}x{} @ {} fps, audio={}",
            mode_label,
            total_frames,
            duration,
            start_beat,
            end_beat,
            export_config.width,
            export_config.height,
            export_config.fps,
            export_config.has_audio(),
        );
        // BUG-k1n2w: after a firmware GPU reset, more hangs in the same boot
        // are likely whatever the project; only a reboot cleared them.
        if let Some(resets) = manifold_gpu::firmware_reports::summary_since_boot() {
            log::warn!(
                "[Export] {resets}. Further GPU hangs are likely until a reboot; reboot before a long export."
            );
        }

        // 4. Create export session (opens the platform's video encoder).
        //    Share the content pipeline's device to avoid cross-device GPU sync.
        let session_result = manifold_media::export_session::ExportSession::new(
            export_config.clone(),
            bpm.0,
            &mut tempo_map,
            self.content_pipeline.native_device(),
        );
        let mut session = match session_result {
            Ok(s) => s,
            Err(e) => {
                log::error!("[ContentThread] Failed to create export session: {e}");
                let _ = std::fs::remove_file(&mix_wav_path);
                self.send_export_finished(
                    state_tx,
                    false,
                    format!("Export failed: {e}"),
                    &export_config.output_path,
                );
                return true;
            }
        };

        // 4b. Seek to start (export mode was entered and the pipeline resized
        // once by the caller, before the section loop) on the export grid:
        // frame 0 is the first export-grid frame at or after the start
        // (FRAME_EXACT_TRANSPORT_DESIGN D9, D17). Video projects first tick
        // until the decoders have a frame (pre_render drains decode results),
        // with no GPU rendering; the engine then re-seeks to the start.
        let start_time = self.engine.beat_to_timeline_time(Beats(start_beat));
        self.engine
            .begin_export_run(export_config.fps, start_time, |engine| {
                if generator_only {
                    return false;
                }
                const MAX_WARMUP_TICKS: u32 = 120;
                for warmup_i in 0..MAX_WARMUP_TICKS {
                    if engine.export_warm_up_tick() {
                        break;
                    }
                    if warmup_i % 30 == 29 {
                        log::warn!(
                            "[Export] Still waiting for decoders after {} warmup ticks",
                            warmup_i + 1
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                true
            });

        // 5. Export frame loop.
        //    The engine is parked on the export start's grid frame; export
        //    frame k is that frame + k, set exactly whatever the sync state.
        // Project warmup can evaluate a variable number of frames while
        // imported assets load. Restart simulation and RT sampling once per
        // section so those preparation frames cannot affect exported pixels.
        for renderer in self.engine.renderers_mut() {
            if let Some(generator) = renderer
                .as_any_mut()
                .downcast_mut::<manifold_compositor::generator_renderer::GeneratorRenderer>(
            ) {
                generator.reset_all_generator_state();
            }
        }
        manifold_gpu::raytrace::request_conservative_trace_tiles();
        //    Each iteration is wrapped in an autoreleasepool to drain Metal's
        //    autoreleased ObjC objects per-frame.
        let mut cancelled = false;
        let mut encode_error: Option<String> = None;
        let mut gpu_fatal = false;
        for frame_idx in 0..total_frames {
            // Check for cancel command (non-blocking drain)
            cancelled = self.poll_export_cancel(cmd_rx);
            if cancelled {
                session.cancel();
                break;
            }

            #[cfg(target_os = "macos")]
            let frame_err: Option<ExportFrameFailure> = manifold_gpu::autorelease_scope(|| {
                self.export_one_frame(
                    &mut session,
                    &export_config,
                    frame_idx,
                    total_frames,
                    frame_dt,
                    state_tx,
                    progress_prefix,
                    generator_only,
                    offline_audio_mod.as_mut(),
                )
            });
            #[cfg(not(target_os = "macos"))]
            let frame_err: Option<ExportFrameFailure> = self.export_one_frame(
                &mut session,
                &export_config,
                frame_idx,
                total_frames,
                frame_dt,
                state_tx,
                progress_prefix,
                generator_only,
                offline_audio_mod.as_mut(),
            );

            if let Some(err) = frame_err {
                gpu_fatal = err.fatal;
                encode_error = Some(err.message);
                break;
            }
        }

        // 6. Finalize
        let mut failed = cancelled || encode_error.is_some();
        let mut finalize_failed = false;
        let frames_encoded = session.frames_encoded();
        if failed {
            session.cancel();
        } else {
            // Finalization can block in the native encoder/audio muxer. Honor a
            // request both before and after it, before reporting this file done.
            let status = match progress_prefix {
                Some(prefix) => format!("{prefix} — Finalizing video..."),
                None => "Finalizing video...".into(),
            };
            self.send_export_phase(state_tx, true, 1.0, &status, false);
            cancelled = self.poll_export_cancel(cmd_rx);
            if cancelled {
                session.cancel();
            } else {
                let result = session.finalize(ffmpeg_path.as_deref());
                cancelled = self.poll_export_cancel(cmd_rx);
                if !cancelled {
                    match result {
                        Ok(result) => {
                            log::info!(
                                "[ContentThread] Export complete: {} frames, {:.2}s -> {}",
                                result.frames_encoded,
                                result.duration_seconds,
                                result.output_path,
                            );
                            self.send_export_finished(
                                state_tx,
                                true,
                                format!("Export complete: {} frames", result.frames_encoded),
                                &result.output_path,
                            );
                        }
                        Err(e) => {
                            log::error!("[ContentThread] Export finalization failed: {e}");
                            self.send_export_finished(
                                state_tx,
                                false,
                                format!("Export failed: {e}"),
                                &export_config.output_path,
                            );
                            finalize_failed = true;
                        }
                    }
                }
            }
            failed = cancelled;
        }
        if failed {
            if cancelled {
                log::info!(
                    "[ContentThread] Export cancelled at frame {}",
                    frames_encoded
                );
            }
            let _ = std::fs::remove_file(&export_config.output_path);
            let temp_video = format!("{}.video_only.mp4", export_config.output_path);
            let _ = std::fs::remove_file(&temp_video);
        }

        // Remove the temporary audio mixdown WAV (already muxed into the final
        // file; a no-op when no audio was rendered).
        let _ = std::fs::remove_file(&mix_wav_path);

        if failed {
            let msg = if let Some(err) = encode_error {
                format!("Export failed: {err}")
            } else {
                "Export cancelled".into()
            };
            self.send_export_finished(state_tx, false, msg, &export_config.output_path);
        }

        if gpu_fatal {
            // Only a page fault, device loss or blacklisted queue lands here
            // (hangs retry; other failures end the export and keep the app).
            // Synthetic faults must prove the decision and the partial-file
            // cleanup without terminating the test process.
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            if EXPORT_FAILURE_FRAME.with(|fault| fault.get().is_some()) {
                EXPORT_GPU_ABORT_REQUESTED.with(|requested| requested.set(true));
                return failed || finalize_failed;
            }
            crate::abort_gpu_work("Export GPU failure; partial export cancelled");
        }
        failed || finalize_failed
    }

    /// Render and encode a single export frame. Returns Some(error) on failure.
    fn export_one_frame(
        &mut self,
        session: &mut manifold_media::export_session::ExportSession,
        export_config: &manifold_media::export_config::ExportConfig,
        frame_idx: u32,
        _total_frames: u32,
        frame_dt: f64,
        state_tx: &StateTx,
        progress_prefix: Option<&str>,
        generator_only: bool,
        mut offline_audio_mod: Option<&mut crate::offline_audio_mod::OfflineAudioModDriver>,
    ) -> Option<ExportFrameFailure> {
        // Frame k is the export start's grid frame + k on the song-time-0
        // export grid, so it samples the same time live play does. dt-driven
        // effect state does not integrate on the first frame (frames 0): it
        // renders the state at the export start, then advances one frame per
        // output frame.
        let ctx =
            PlaybackEngine::export_frame_context(frame_idx, Seconds(frame_idx as f64 * frame_dt));
        // Feed this frame's
        // export-rendered audio through the analyzer chain and write the
        // resulting features into the engine's audio snapshot BEFORE the
        // tick that consumes them for param modulation, param triggers, and
        // live clip triggers — deterministic audio reactivity in the export.
        // No restore after export: `AudioModRuntime::update` overwrites
        // `snap.sends` unconditionally on every live tick (including its
        // `active == false` branch, which still clears+resizes), so
        // export-written features cannot leak into subsequent live playback.
        if let Some(driver) = offline_audio_mod.as_deref_mut()
            && let Err(error) = driver.feed_frame(frame_idx, &mut self.engine)
        {
            return Some(ExportFrameFailure {
                message: format!("Export stopped at frame {frame_idx}: {error}."),
                fatal: false,
            });
        }
        let tick_result = self.engine.tick(ctx);
        if let Some(error) = self.engine.trigger_delivery_failure() {
            self.engine.reclaim_tick_result(tick_result);
            return Some(ExportFrameFailure {
                message: format!("Export stopped at frame {frame_idx}: {error}"),
                fatal: false,
            });
        }
        if let Some(message) = self
            .engine
            .project()
            .and_then(manifold_playback::modulation::control_capture_error)
        {
            return Some(ExportFrameFailure {
                message,
                fatal: false,
            });
        }

        // Wait for any in-flight video decodes to complete before rendering.
        // At GPU speed the export outruns the async decoder — without this,
        // the same stale video frame gets encoded for dozens of frames.
        // Skipped for generator-only projects (no video decoders).
        if !generator_only {
            self.engine.flush_pending_decodes();
        }

        // A recoverable GPU hang re-renders this same frame. Everything above
        // (audio feed, tick, decode flush) has run once and is not repeated, so
        // the timeline position and audio are consumed exactly once.
        let mut attempt = 0u32;
        #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
        let mut sdr_mapped_rgba16f = None;
        #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
        let mut hdr_scene_rgba16f = None;
        let mut export_texture;
        loop {
            let fault_mark = manifold_gpu::gpu_fault::mark();
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            EXPORT_RENDER_ATTEMPTS.with(|count| count.set(count.get() + 1));
            self.content_pipeline.render_content(
                &self.gpu,
                &mut self.engine,
                &tick_result,
                frame_idx as u64,
                true,
                self.editing_service.stamps(),
                offline_audio_mod.as_deref().map(|driver| driver.visuals()),
            );

            // Block until async effect workers complete (blob tracking, wireframe depth,
            // depth-of-field). During live playback 1-2 frame latency is acceptable, but
            // export must be frame-perfect: each frame's async results must resolve before
            // the frame is encoded.
            self.content_pipeline.flush_all_background_work();

            // The frame must be
            // Complete before its texture is selected and encoded. Pending at
            // this boundary is an error, never permission to tick again — the
            // export loop must not rerun the graph to settle RT.
            let frame_status = self.content_pipeline.frame_render_status();
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
        let frame_status = match export_test_fault(frame_idx) {
            Some(ExportTestFault::PendingGeometry) => manifold_node_engine::runtime::frame_status::FrameRenderStatus::PendingGeometry,
            Some(ExportTestFault::Preparation) => manifold_node_engine::runtime::frame_status::FrameRenderStatus::Failed(manifold_node_engine::runtime::frame_status::FrameRenderFailure::RtAllocation),
            Some(ExportTestFault::Encode) => manifold_node_engine::runtime::frame_status::FrameRenderStatus::Failed(manifold_node_engine::runtime::frame_status::FrameRenderFailure::RtEncode),
            _ => frame_status,
        };
            if frame_status
                != manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete
            {
                let message = format!(
                    "Export frame {frame_idx} is not complete ({frame_status:?}); refusing to encode"
                );
                log::error!("[Export] {message}");
                return Some(ExportFrameFailure {
                    message,
                    fatal: false,
                });
            }

            if export_config.include_gizmos {
                self.content_pipeline.draw_export_gizmos();
            }

            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            let mut sdr_mapped_texture = None;
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            let mut hdr_scene_texture = None;
            export_texture = if export_config.hdr {
                let paper_white = 200.0f32;
                let max_nits = 10000.0f32;
                #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
                if export_hdr_readback_requested(frame_idx) {
                    // The canonical scene texture is the production source fed to
                    // PQ encoding. Clone only its native handle so readback can
                    // happen after the completion barrier without retaining a
                    // borrow into ContentPipeline.
                    hdr_scene_texture = Some(self.content_pipeline.export_output_texture().clone());
                }
                self.content_pipeline
                    .pq_encode_for_export(paper_white, max_nits)
                    .clone()
            } else {
                let texture = self.content_pipeline.sdr_export_output_texture();
                #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
                if export_sdr_readback_requested(frame_idx) {
                    // Clone only the native texture handle so the readback can
                    // happen after the export completion barrier without holding
                    // a borrow into ContentPipeline across that wait.
                    sdr_mapped_texture = Some(texture.clone());
                }
                texture.clone()
            };

            let completion = self.content_pipeline.wait_for_export_complete(&fault_mark);
            // Exercise the real completion decision without faulting or hanging
            // hardware, and only after the actual submitted work has completed.
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            let completion = completion.and_then(|()| match export_test_fault(frame_idx) {
                Some(ExportTestFault::GpuFault) => export_gpu_completion(
                    true,
                    0,
                    1,
                    FaultKinds::default().with(FaultKind::PageFault),
                    false,
                    false,
                )
                .unwrap(),
                Some(ExportTestFault::IgnoredSubmission) => {
                    export_gpu_completion(true, 0, 0, FaultKinds::default(), true, false).unwrap()
                }
                Some(ExportTestFault::CompletionTimeout) => {
                    export_gpu_completion(false, 0, 0, FaultKinds::default(), false, true).unwrap()
                }
                Some(ExportTestFault::Hang { failing_attempts }) if attempt < failing_attempts => {
                    export_gpu_completion(
                        true,
                        0,
                        1,
                        FaultKinds::default().with(FaultKind::Hang),
                        false,
                        false,
                    )
                    .unwrap()
                }
                _ => Ok(()),
            });
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            if completion.is_ok()
                && let Some(texture) = sdr_mapped_texture.as_ref()
            {
                let device = self
                    .content_pipeline
                    .native_device()
                    .expect("SDR colour proof requires the export Metal device");
                sdr_mapped_rgba16f = Some(
                    manifold_node_engine::gpu::headless_readback::readback_raw_halves(
                        device,
                        texture,
                        texture.width,
                        texture.height,
                    )
                    .expect("GPU readback completes"),
                );
            }
            #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
            if completion.is_ok()
                && let Some(texture) = hdr_scene_texture.as_ref()
            {
                let device = self
                    .content_pipeline
                    .native_device()
                    .expect("HDR colour proof requires the export Metal device");
                hdr_scene_rgba16f = Some(
                    manifold_node_engine::gpu::headless_readback::readback_raw_halves(
                        device,
                        texture,
                        texture.width,
                        texture.height,
                    )
                    .expect("GPU readback completes"),
                );
            }
            if let Err(failure) = completion {
                log::error!(
                    "[Export] Frame {frame_idx} attempt {attempt} failed: {}",
                    failure.message
                );
                if failure.kinds.is_recoverable_hang() && attempt < EXPORT_HANG_RETRIES {
                    // Victim buffers from the hang must land before the retry
                    // takes its fault mark, or they would be blamed on it.
                    if !manifold_gpu::gpu_fault::drain_completions(std::time::Duration::from_secs(
                        2,
                    )) {
                        return Some(ExportFrameFailure {
                            message: format!(
                                "Export stopped at frame {frame_idx}: {}. The GPU did not settle, so the frame was not retried.",
                                failure.message
                            ),
                            fatal: false,
                        });
                    }
                    attempt += 1;
                    log::warn!(
                        "[Export] Re-rendering frame {frame_idx} after a GPU hang (retry {attempt} of {EXPORT_HANG_RETRIES}). \
                     Timeline and audio are not re-run; layers with feedback or simulation state advance twice."
                    );
                    request_conservative_rt_tiles(&mut self.content_pipeline, attempt);
                    continue;
                }
                let retried = match attempt {
                    0 => String::new(),
                    1 => " after 1 retry".to_owned(),
                    n => format!(" after {n} retries"),
                };
                return Some(ExportFrameFailure {
                    message: format!(
                        "Export stopped at frame {frame_idx}{retried}: {}",
                        failure.message
                    ),
                    fatal: failure.kinds.is_fatal(),
                });
            }
            break;
        }

        #[cfg(all(test, feature = "journey-proofs", target_os = "macos"))]
        {
            let (rt_updates, rt_dispatches, history_resets) =
                self.content_pipeline.frame_rt_observation();
            observe_export_frame(ExportFrameObservation {
                frame_idx,
                time_seconds: frame_idx as f64 * frame_dt,
                tick_dt: self.engine.tick_dt().0,
                status: manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
                rt_updates,
                rt_dispatches,
                history_resets,
                beat: self.engine.current_beat().0,
                generator_values: self
                    .engine
                    .project()
                    .into_iter()
                    .flat_map(|project| &project.timeline.layers)
                    .filter_map(|layer| layer.gen_params())
                    .flat_map(|instance| instance.params.iter())
                    .map(|param| (param.id().to_owned(), param.value))
                    .collect(),
                sdr_mapped_rgba16f,
                hdr_scene_rgba16f,
            });
            if export_test_fault(frame_idx) == Some(ExportTestFault::BeforeEncode) {
                return Some(ExportFrameFailure {
                    message: format!("injected export failure before encode at frame {frame_idx}"),
                    fatal: false,
                });
            }
        }

        let Some(device) = self.content_pipeline.native_device() else {
            log::error!("[ContentThread] No GPU device at frame {frame_idx}");
            return Some(ExportFrameFailure {
                message: format!("No GPU device at frame {frame_idx}"),
                fatal: false,
            });
        };
        if let Err(e) = session.encode_frame(device, &export_texture) {
            log::error!("[ContentThread] Encode failed at frame {frame_idx}: {e}");
            return Some(ExportFrameFailure {
                message: format!("Encode failed at frame {frame_idx}: {e}"),
                fatal: false,
            });
        }

        if !tick_result.stopped_clips.is_empty() {
            self.content_pipeline
                .cleanup_stopped_clips(&tick_result.stopped_clips);
        }
        self.engine.reclaim_tick_result(tick_result);

        if frame_idx.is_multiple_of(10) {
            self.send_export_progress(state_tx, session, progress_prefix);
        }

        None
    }

    /// Send export progress to the UI thread.
    /// Consumed as notifications by the export modal (`ui_root/export.rs`),
    /// without replacing the full playback snapshot with default fields.
    fn send_export_progress(
        &self,
        state_tx: &StateTx,
        session: &manifold_media::export_session::ExportSession,
        progress_prefix: Option<&str>,
    ) {
        let status = match progress_prefix {
            Some(prefix) => format!("{prefix} — {}", session.status_text()),
            None => session.status_text(),
        };
        let state = ContentState {
            is_exporting: true,
            export_progress: session.progress(),
            export_status: Arc::from(status),
            current_beat: self.engine.current_beat(),
            current_time: self.engine.current_time(),
            is_playing: self.engine.is_playing(),
            ..ContentState::default()
        };
        state_tx.publish(state);
    }

    /// Submit a pending still-frame export's GPU readback, if one is waiting and
    /// hasn't been submitted yet. Called right after `render_content` so the blit
    /// reads a fully-rendered frame. Records the captured dimensions on the job.
    pub(crate) fn submit_still_export_if_pending(&mut self) {
        if let Some(job) = self.still_export.as_mut()
            && job.dims.is_none()
        {
            let dims = self.content_pipeline.submit_still_readback();
            // Re-borrow: submit_still_readback took &mut self.content_pipeline.
            if let Some(job) = self.still_export.as_mut() {
                job.dims = Some(dims);
            }
        }
    }

    /// Read back a submitted still-frame export, then convert colour, encode,
    /// and write to disk on a detached thread (decoding linear f16, sRGB
    /// encoding, and PNG-ing a 4000×4000 frame is far too heavy for the content
    /// thread). The finished event is sent from that thread. No-op until the
    /// readback has been submitted (`dims` set) and the GPU copy is readable.
    pub(crate) fn poll_still_export(&mut self, state_tx: &StateTx) {
        let Some(job) = self.still_export.as_mut() else {
            return;
        };
        // Only act once the readback has been submitted (dims set on the prior tick).
        if job.dims.is_none() {
            return;
        }
        if job.pixels.is_none() {
            let Some(packed_f16) = self.content_pipeline.take_still_readback() else {
                return;
            };
            let Some(job) = self.still_export.as_mut() else {
                return;
            };
            job.pixels = Some(packed_f16);
        }
        // Held until the save dialog names a destination.
        if self.still_export.as_ref().is_none_or(|j| j.dest.is_none()) {
            return;
        }
        let job = self.still_export.take().expect("checked above");
        let (w, h) = job.dims.expect("checked above");
        let packed_f16 = job.pixels.expect("checked above");
        let (path, format) = job.dest.expect("checked above");
        let tx = state_tx.clone();

        std::thread::Builder::new()
            .name("still-export-encode".into())
            .spawn(move || {
                // Linear Rgba16Float → sRGB-encoded RGBA8 (faithful: no highlight
                // rolloff, matching the on-screen image). Then encode to disk.
                let encode = manifold_media::still_exporter::linear_f16_rgba_to_srgb8(
                    &packed_f16,
                    w,
                    h,
                    /* rolloff */ false,
                )
                .and_then(|rgba8| {
                    manifold_media::still_exporter::save_still(
                        &rgba8,
                        w,
                        h,
                        std::path::Path::new(&path),
                        format,
                    )
                });
                let (success, message) = match encode {
                    Ok(()) => {
                        log::info!("[ContentThread] Exported frame to {path}");
                        (true, format!("Exported frame to {path}"))
                    }
                    Err(e) => {
                        log::error!("[ContentThread] Frame export failed: {e}");
                        (false, e)
                    }
                };
                let state = ContentState {
                    export_finished: vec![ExportFinishedEvent {
                        success,
                        message,
                        output_path: path,
                    }],
                    ..ContentState::default()
                };
                tx.publish(state);
            })
            .expect("failed to spawn still-export thread");
    }

    /// Send export finished event to the UI thread.
    pub(crate) fn send_export_finished(
        &self,
        state_tx: &StateTx,
        success: bool,
        message: String,
        output_path: &str,
    ) {
        let state = ContentState {
            export_finished: vec![ExportFinishedEvent {
                success,
                message,
                output_path: output_path.to_string(),
            }],
            ..ContentState::default()
        };
        state_tx.publish(state);
    }

    /// BUG-130: decide whether an export can proceed to frame 0.
    ///
    /// When the export needs audio muxing, ffmpeg must be resolvable — if
    /// it's not, the export must abort now, before any frame is rendered or
    /// encoded, rather than discovering the absence at `finalize()` after a
    /// full (potentially multi-minute) render.
    ///
    /// `resolve` is injected (rather than calling `AudioMuxer::resolve_ffmpeg`
    /// directly) so this decision is unit-testable without depending on
    /// which ffmpeg installs happen to exist on the machine running the test.
    fn ffmpeg_preflight(
        has_audio: bool,
        resolve: impl FnOnce() -> Option<String>,
    ) -> Result<Option<String>, &'static str> {
        if !has_audio {
            return Ok(None);
        }
        match resolve() {
            Some(path) => Ok(Some(path)),
            None => Err("ffmpeg not found (required to mux audio into the export)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_config(hdr: bool) -> manifold_media::export_config::ExportConfig {
        manifold_media::export_config::ExportConfig {
            output_path: String::new(),
            width: 1920,
            height: 1080,
            fps: 60.0,
            hdr,
            start_beat: 0.0,
            end_beat: 0.0,
            audio_path: None,
            audio_start_beat: 0.0,
            audio_encoder_delay: 0.0,
            split_at_markers: false,
            include_gizmos: false,
            metadata: Vec::new(),
        }
    }

    fn meta_get<'a>(m: &'a [(String, String)], key: &str) -> Option<&'a str> {
        m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn export_metadata_sdr_records_settings() {
        use manifold_core::project::Project;
        use manifold_core::types::TonemapCurve;
        let mut project = Project {
            project_name: "Night Set".into(),
            ..Project::default()
        };
        project.settings.tonemap_enabled = true;
        project.settings.tonemap_curve = TonemapCurve::Agx;
        let m = export_metadata(&project, &meta_config(false));
        assert_eq!(meta_get(&m, "title"), Some("Night Set"));
        assert_eq!(
            meta_get(&m, "software"),
            Some(concat!("MANIFOLD ", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(meta_get(&m, "com.manifold.resolution"), Some("1920x1080"));
        assert_eq!(meta_get(&m, "com.manifold.fps"), Some("60"));
        assert_eq!(meta_get(&m, "com.manifold.dynamic_range"), Some("SDR"));
        assert_eq!(meta_get(&m, "com.manifold.codec"), Some("H.264"));
        assert_eq!(meta_get(&m, "com.manifold.sdr_tone_map"), Some("AgX"));
        // Export column defaults.
        assert_eq!(meta_get(&m, "com.manifold.rt_shadows"), Some("High"));
        assert_eq!(
            meta_get(&m, "com.manifold.rt_reflections"),
            Some("Extra High")
        );
        assert_eq!(
            meta_get(&m, "com.manifold.rt_ray_resolution"),
            Some("Native")
        );
        assert_eq!(meta_get(&m, "com.manifold.rt_denoise"), Some("High"));
        assert!(meta_get(&m, "com.manifold.bpm").is_some());
    }

    #[test]
    fn export_metadata_hdr_and_untitled() {
        use manifold_core::project::Project;
        let mut project = Project::default();
        project.settings.tonemap_enabled = true;
        let m = export_metadata(&project, &meta_config(true));
        assert_eq!(meta_get(&m, "title"), None);
        assert_eq!(meta_get(&m, "com.manifold.dynamic_range"), Some("HDR10"));
        assert_eq!(meta_get(&m, "com.manifold.codec"), Some("HEVC Main10"));
        // The SDR curve does not apply to HDR output.
        assert_eq!(meta_get(&m, "com.manifold.sdr_tone_map"), Some("Off"));
        project.settings.tonemap_enabled = false;
        let m = export_metadata(&project, &meta_config(false));
        assert_eq!(meta_get(&m, "com.manifold.sdr_tone_map"), Some("Off"));
    }

    // BUG-130 (a): when the export needs audio muxing, missing ffmpeg must
    // be caught before frame 0 — not discovered only at finalize after a
    // full render.

    #[test]
    fn ffmpeg_preflight_passes_through_when_no_audio() {
        // No audio in this export -> ffmpeg's presence is irrelevant, and the
        // resolver must not even be consulted.
        let mut resolver_called = false;
        let result = ContentThread::ffmpeg_preflight(false, || {
            resolver_called = true;
            None
        });
        assert_eq!(result, Ok(None));
        assert!(
            !resolver_called,
            "resolver should be skipped when there's no audio to mux"
        );
    }

    #[test]
    fn ffmpeg_preflight_aborts_before_frame_0_when_audio_needs_missing_ffmpeg() {
        let result = ContentThread::ffmpeg_preflight(true, || None);
        assert!(
            result.is_err(),
            "must abort, not silently proceed to render frames"
        );
        let reason = result.unwrap_err();
        assert!(
            reason.contains("ffmpeg not found"),
            "reason should be clear: {reason}"
        );
    }

    #[test]
    fn ffmpeg_preflight_proceeds_when_audio_needs_resolvable_ffmpeg() {
        let result =
            ContentThread::ffmpeg_preflight(true, || Some("/opt/homebrew/bin/ffmpeg".to_string()));
        assert_eq!(result, Ok(Some("/opt/homebrew/bin/ffmpeg".to_string())));
    }

    // ── Section export ──

    /// P2 round-trip gate: a project saved with the split-export setting and
    /// plain markers reloads with both intact. Exercises the full `Project`
    /// serde shape (the `ProjectSettings` `split_at_markers` toggle and the
    /// marker list). The field's `split_at_section_markers` alias keeps the
    /// one-day-old name loading too.
    #[test]
    fn markers_and_split_setting_survive_project_roundtrip() {
        use manifold_core::project::Project;

        let mut project = Project::default();
        project.timeline.add_marker(
            manifold_core::marker::TimelineMarker::new(Beats::from_f32(4.0)).with_name("Drop"),
        );
        project.timeline.add_marker(
            manifold_core::marker::TimelineMarker::new(Beats::from_f32(8.0)).with_name("Break"),
        );
        project.settings.split_at_markers = true;

        let json = serde_json::to_string(&project).expect("serialize project");
        let reloaded: Project = serde_json::from_str(&json).expect("reload project");

        assert!(
            reloaded.settings.split_at_markers,
            "split setting must survive the round-trip"
        );
        assert_eq!(reloaded.timeline.markers.len(), 2);
        let drop = &reloaded.timeline.markers[0];
        assert_eq!(drop.name, "Drop");
        assert_eq!(drop.beat, Beats::from_f32(4.0));
        let brk = &reloaded.timeline.markers[1];
        assert_eq!(brk.name, "Break");
        assert_eq!(brk.beat, Beats::from_f32(8.0));

        // Projects saved during the one-day flavor carry the old name; serde
        // must load them, not fail.
        let old_name = r#"{"settings":{"splitAtSectionMarkers":true}}"#;
        let old_project: Project = serde_json::from_str(old_name).expect("old-name setting loads");
        assert!(old_project.settings.split_at_markers);
    }

    fn plain_marker(beat: f32, name: &str) -> manifold_core::marker::TimelineMarker {
        manifold_core::marker::TimelineMarker::new(Beats::from_f32(beat)).with_name(name)
    }

    fn timeline_with(
        markers: Vec<manifold_core::marker::TimelineMarker>,
    ) -> manifold_core::timeline::Timeline {
        let mut t = manifold_core::timeline::Timeline::default();
        for m in markers {
            t.add_marker(m);
        }
        t
    }

    #[test]
    fn derive_sections_returns_empty_without_in_range_markers() {
        // Invariant (b): setting on but no in-range markers → empty → the
        // caller takes the single-export path (one file at output_path).
        // Out-of-range markers must not slice.
        let t = timeline_with(vec![plain_marker(20.0, "outside")]);
        assert!(derive_sections(&t, Beats::from_f32(4.0), Beats::from_f32(16.0)).is_empty());

        // No markers at all → empty over any range.
        let t = timeline_with(vec![plain_marker(2.0, "below-range")]);
        assert!(derive_sections(&t, Beats::from_f32(4.0), Beats::from_f32(16.0)).is_empty());
    }

    #[test]
    fn derive_sections_splits_chapter_style() {
        // D2: sections are [in, m₁), [m₁, m₂), …, [mₙ, out); each section named
        // by the marker at its start; the leading [in, m₁) section is unnamed.
        // Every marker in range cuts — there is no flavor.
        let t = timeline_with(vec![
            plain_marker(4.0, "Drop"),
            plain_marker(5.0, "Half"),
            plain_marker(8.0, "Break"),
        ]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (
                    Beats::from_f32(4.0),
                    Beats::from_f32(5.0),
                    "Drop".to_string()
                ),
                (
                    Beats::from_f32(5.0),
                    Beats::from_f32(8.0),
                    "Half".to_string()
                ),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Break".to_string()
                ),
            ]
        );
    }

    #[test]
    fn derive_sections_edge_cases() {
        // A marker exactly on `out` is excluded (outside the half-open range):
        // it must not cut and must not name. Marker exactly on `in` names the
        // first section (see `derive_sections_names_first_section_from_marker_on_in`).
        let t = timeline_with(vec![plain_marker(8.0, "Mid"), plain_marker(16.0, "OnOut")]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(8.0), String::new()),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Mid".to_string()
                ),
            ]
        );

        // Duplicate beats collapse to a single cut. The surviving name is the
        // first in marker-list order (stable sort + dedup keeps the first), so
        // set the list directly to pin the order rather than relying on
        // `add_marker`'s insert-before-equal placement.
        let mut t = manifold_core::timeline::Timeline::default();
        t.markers = vec![
            plain_marker(4.0, "First"),
            plain_marker(4.0, "Second"),
            plain_marker(8.0, "Next"),
        ];
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (
                    Beats::from_f32(4.0),
                    Beats::from_f32(8.0),
                    "First".to_string()
                ),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Next".to_string()
                ),
            ]
        );
    }

    #[test]
    fn derive_sections_names_first_section_from_marker_on_in() {
        // A marker exactly on `in` names the first section (doesn't cut), so
        // Peter can place one at the export-in point to name the intro instead
        // of getting `section-1`.
        let t = timeline_with(vec![plain_marker(0.0, "Intro"), plain_marker(8.0, "Drop")]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (
                    Beats::from_f32(0.0),
                    Beats::from_f32(8.0),
                    "Intro".to_string()
                ),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Drop".to_string()
                ),
            ]
        );

        // Marker on `in` with no interior cuts → one named section (no split).
        let t = timeline_with(vec![plain_marker(0.0, "Only")]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![(
                Beats::from_f32(0.0),
                Beats::from_f32(16.0),
                "Only".to_string()
            )]
        );

        // Two markers on `in` → first in list order wins.
        let mut t = manifold_core::timeline::Timeline::default();
        t.markers = vec![
            plain_marker(0.0, "First"),
            plain_marker(0.0, "Second"),
            plain_marker(8.0, "Drop"),
        ];
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (
                    Beats::from_f32(0.0),
                    Beats::from_f32(8.0),
                    "First".to_string()
                ),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Drop".to_string()
                ),
            ]
        );
    }

    #[test]
    fn derive_sections_sorts_unsorted_markers() {
        // `add_marker` keeps `markers` sorted, but `derive_sections` must not
        // depend on that: hand it an unsorted `markers` vec directly.
        let mut t = manifold_core::timeline::Timeline::default();
        t.markers = vec![plain_marker(8.0, "Break"), plain_marker(4.0, "Drop")];
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (
                    Beats::from_f32(4.0),
                    Beats::from_f32(8.0),
                    "Drop".to_string()
                ),
                (
                    Beats::from_f32(8.0),
                    Beats::from_f32(16.0),
                    "Break".to_string()
                ),
            ]
        );
    }

    #[test]
    fn section_output_paths_sanitize_empty_and_collisions() {
        use std::path::Path;
        let base = Path::new("/tmp/export")
            .join("my show.mp4")
            .to_string_lossy()
            .into_owned();

        // Sanitization + the leading unnamed section → `section-1`.
        let sections = vec![
            (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
            (
                Beats::from_f32(4.0),
                Beats::from_f32(8.0),
                "Drop Build Up!".to_string(),
            ),
            (
                Beats::from_f32(8.0),
                Beats::from_f32(16.0),
                "Drop Build Up!".to_string(),
            ),
        ];
        let paths = section_output_paths(&base, &sections);
        assert_eq!(
            paths,
            vec![
                "/tmp/export/my show--section-1.mp4",
                "/tmp/export/my show--Drop-Build-Up.mp4",
                "/tmp/export/my show--Drop-Build-Up-2.mp4",
            ]
        );

        // An empty-but-named fallback colliding with an explicit `section-1`.
        let sections = vec![
            (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
            (
                Beats::from_f32(4.0),
                Beats::from_f32(8.0),
                "section-1".to_string(),
            ),
        ];
        let paths = section_output_paths(&base, &sections);
        assert_eq!(
            paths,
            vec![
                "/tmp/export/my show--section-1.mp4",
                "/tmp/export/my show--section-1-2.mp4",
            ]
        );
    }

    /// The export's command poll, as the frame loop calls it.
    fn poll(thread: &mut ContentThread, cmd_rx: &Receiver<ContentCommand>) -> bool {
        thread.poll_export_cancel(cmd_rx)
    }

    /// BUG-v2o9y: nothing changes during an export. Play, a seek, a show setting and an edit
    /// sent meanwhile are thrown away; a preview resize, a graph watch and a view setting
    /// are held and run once it ends; Cancel and
    /// Quit still stop it, and the quit proceeds right after.
    #[test]
    fn commands_during_export_are_dropped_and_a_quit_cancels_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut thread = crate::headless_harness::headless_content_thread(
            manifold_core::project::Project::default(),
            64,
            64,
        );
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let edited = Arc::new(AtomicBool::new(false));
        let edited_in_command = Arc::clone(&edited);

        cmd_tx.send(ContentCommand::Play).expect("send");
        cmd_tx
            .send(ContentCommand::SeekTo(Seconds::from_f32(5.0)))
            .expect("send");
        cmd_tx
            .send(ContentCommand::SetFrameRate(24.0))
            .expect("send");
        cmd_tx
            .send(ContentCommand::SetSdrPreview(true))
            .expect("send");
        cmd_tx
            .send(ContentCommand::ResizeWorkspacePreview(320, 180))
            .expect("send");
        let watched = manifold_core::GraphTarget::Generator(manifold_core::LayerId::new("watched"));
        cmd_tx
            .send(ContentCommand::WatchGraphTarget(Some(watched.clone())))
            .expect("send");
        cmd_tx
            .send(ContentCommand::mutate(
                crate::content_command::touch::VALUES,
                Box::new(move |_| edited_in_command.store(true, Ordering::SeqCst)),
            ))
            .expect("send");
        assert!(!poll(&mut thread, &cmd_rx), "edits do not cancel an export");
        // A long export: every frame the UI wakes the surface wait and
        // reports a stage present. Neither may pile up in the held queue.
        let now = std::time::Instant::now();
        for id in 0..1000u64 {
            cmd_tx.send(ContentCommand::SurfaceReady).expect("send");
            cmd_tx
                .send(ContentCommand::FramePresented {
                    drawable_id: id,
                    at: now,
                })
                .expect("send");
            assert!(!poll(&mut thread, &cmd_rx));
        }
        assert!(
            thread.held_commands.len() <= 4,
            "held queue grew to {}",
            thread.held_commands.len()
        );
        cmd_tx.send(ContentCommand::Shutdown).expect("send");
        assert!(poll(&mut thread, &cmd_rx), "a quit cancels the export");

        assert!(
            thread.drain_commands(&cmd_rx),
            "the quit proceeds after the export"
        );
        assert!(
            !thread.engine.is_playing(),
            "Play sent during the export was dropped"
        );
        assert_eq!(
            thread.engine.current_time(),
            Seconds::ZERO,
            "the seek was dropped"
        );
        assert_ne!(
            thread.engine.project().map(|p| p.settings.frame_rate),
            Some(24.0),
            "the show setting was dropped"
        );
        assert!(
            thread.content_pipeline.sdr_preview(),
            "the view setting was held and ran after the export"
        );
        assert_eq!(
            thread.watched_graph_target,
            Some(watched),
            "the graph watch was held and ran after the export"
        );
        assert!(
            !edited.load(Ordering::SeqCst),
            "the project edit was dropped"
        );

        // Control: the same Play and edit do run outside an export.
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let edited_after = Arc::clone(&edited);
        cmd_tx.send(ContentCommand::Play).expect("send");
        cmd_tx
            .send(ContentCommand::mutate(
                crate::content_command::touch::VALUES,
                Box::new(move |_| edited_after.store(true, Ordering::SeqCst)),
            ))
            .expect("send");
        thread.drain_commands(&cmd_rx);
        assert!(
            thread.engine.is_playing(),
            "control: Play runs in the normal loop"
        );
        assert!(
            edited.load(Ordering::SeqCst),
            "control: the edit runs in the normal loop"
        );
    }

    /// BUG-v2o9y: edits sent during an export are dropped; when the export
    /// ends the content thread publishes its project again so the UI shows
    /// the new allocation and every panel re-projects from it.
    #[test]
    fn export_end_republishes_the_project_to_the_ui() {
        let mut thread = crate::headless_harness::headless_content_thread(
            manifold_core::project::Project::default(),
            64,
            64,
        );
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
        drop(cmd_tx);
        let (state_tx, state_rx) = crate::content_state::state_channel();
        thread.reactions.publish_pending = true;
        thread.publish_snapshot_if_dirty(&state_tx);
        let published = state_rx
            .try_iter()
            .filter_map(|s| s.project_snapshot)
            .last()
            .expect("first publish");
        // The UI took the first publish.
        let mut ui_project = crate::ui_bridge::LocalProject::new(Default::default());
        let mut stamps = crate::frame_plan::UiStamps::default();
        assert!(crate::frame_plan::take_project_snapshot(
            &mut ui_project,
            &mut stamps,
            published.clone(),
            true
        ));
        let before = stamps.parts();

        thread.run_export(meta_config(false), &cmd_rx, &state_tx);
        thread.publish_snapshot_if_dirty(&state_tx);
        let after = state_rx
            .try_iter()
            .filter_map(|s| s.project_snapshot)
            .last()
            .expect("the export end republishes the project");
        // The UI's own intake function; data version and stamps unmoved.
        assert!(!std::sync::Arc::ptr_eq(&published, &after));
        assert!(crate::frame_plan::take_project_snapshot(
            &mut ui_project,
            &mut stamps,
            after.clone(),
            false
        ));
        assert!(
            std::ptr::eq(&*ui_project, &*after),
            "the UI shows the content thread's project"
        );
        assert_ne!(
            stamps.parts(),
            before,
            "panels re-project from the corrected copy"
        );
    }

    /// Export then Cancel pressed while a show warms up are both held; the
    /// export that starts from the held queue must see its held cancel.
    #[test]
    fn a_cancel_held_behind_its_export_cancels_it() {
        let mut thread = crate::headless_harness::headless_content_thread(
            manifold_core::project::Project::default(),
            64,
            64,
        );
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
        thread
            .held_commands
            .push_back(ContentCommand::StartExport(Box::new(meta_config(false))));
        thread.held_commands.push_back(ContentCommand::CancelExport);

        let started = thread.next_command(&cmd_rx);
        assert!(matches!(started, Ok(ContentCommand::StartExport(_))));
        assert!(
            poll(&mut thread, &cmd_rx),
            "the held cancel stops the export"
        );
        assert!(
            thread.held_commands.is_empty(),
            "the cancel is spent on its export"
        );
    }

    /// A live scene viewport (here the preview's navigation) sits out an
    /// export: no export frame renders or records a viewport frame, and the
    /// same request is back once the export ends.
    #[test]
    fn an_export_renders_no_scene_viewport_and_navigation_resumes_after() {
        use crate::scene_viewport::{SceneViewportNavigation, ViewportHost};
        let mut project = manifold_core::project::Project::default();
        let mut layer = manifold_core::layer::Layer::new_generator(
            "Scene".into(),
            manifold_core::PresetTypeId::PLASMA,
            0,
        );
        layer
            .clips
            .push(manifold_core::clip::TimelineClip::new_generator(
                manifold_core::Beats(0.0),
                manifold_core::Beats(0.5),
            ));
        let target = manifold_core::GraphTarget::Generator(layer.layer_id.clone());
        project.timeline.layers.push(layer);
        let mut thread = crate::headless_harness::headless_content_thread(project, 64, 64);
        let request = SceneViewportNavigation::new(ViewportHost::OutputPreview, 64, 64).request(
            &target,
            &manifold_core::NodeId::new("scene"),
            None,
            None,
        );
        thread.handle_command(ContentCommand::SetSceneViewport(Some(request.clone())));

        let dir =
            std::env::temp_dir().join(format!("manifold-viewport-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("export dir");
        let mut config = meta_config(false);
        (config.width, config.height, config.fps, config.end_beat) = (64, 64, 30.0, 0.5);
        config.output_path = dir.join("viewport.mp4").to_string_lossy().into_owned();
        let (_cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
        let (state_tx, state_rx) = crate::content_state::state_channel();
        thread.run_export(config, &cmd_rx, &state_tx);
        let finished = state_rx
            .try_iter()
            .flat_map(|state| state.export_finished.clone())
            .last();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            finished.is_some_and(|event| event.success),
            "the export renders its frames"
        );

        assert!(
            thread
                .content_pipeline
                .scene_viewport_frames()
                .iter()
                .all(Option::is_none),
            "an export frame rendered the scene viewport"
        );
        assert_eq!(
            thread.content_pipeline.scene_viewport_host(),
            Some(ViewportHost::OutputPreview)
        );
        assert!(
            thread
                .content_pipeline
                .take_scene_viewport_request()
                .is_some_and(|back| Arc::ptr_eq(&back, &request)),
            "navigation resumes with the same request"
        );
    }
}
