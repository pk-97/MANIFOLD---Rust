// Port of Unity PercussionImportOrchestrator.cs (1501 lines).
// Central orchestration class for all percussion import operations.
//
// Unity's IEnumerator coroutines are translated to a poll-based state machine.
// The host calls `tick(unscaled_time, project, editing_service)` each frame.
// When an operation is in progress the state machine drives itself to completion
// over multiple frames via `PipelinePhase`.
//
// Unity's `Time.unscaledTime` maps to the `unscaled_time` parameter passed to tick().
// Unity's `Color` maps to `[f32; 4]` (RGBA).
// Unity's `Mathf.Clamp01` maps to `.clamp(0.0, 1.0)`.
// Unity's `Mathf.Approximately(a, b)` maps to `(a - b).abs() < 0.0001`.
// Unity's `Mathf.Round` maps to `.round()`.
// Unity's `Mathf.RoundToInt` maps to `.round() as i32`.
// Unity's `Mathf.Max(0, x)` maps to `x.max(0.0)`.
//
// File-dialog stub: callers supply a `selected_path: Option<String>` directly;
// the host is responsible for running a dialog before calling into the orchestrator.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::ClipId;
use manifold_core::audio_clip_detection::AudioClipDetection;
use manifold_core::percussion_analysis::{
    ClipDetectionAnchor, PercussionAnalysisData, PercussionTriggerType, ProjectBeatTimeConverter,
};
use manifold_core::percussion_binding::ProjectPercussionBindingResolver;
use manifold_core::percussion_settings::PercussionPipelineSettings;
use manifold_core::LayerId;
use manifold_core::audio_setup::AudioSend;
use manifold_core::layer::{DetectStemRole, Layer};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::audio_setup::{
    AddAudioSendCommand, RenameAudioSendCommand, SetLayerAudioSendCommand,
};
use manifold_editing::commands::clip::{AddClipCommand, DeleteClipCommand};
use manifold_editing::commands::layer::{AddLayerCommand, GroupLayersCommand, RenameLayerCommand};

use crate::percussion_backend::{PercussionPipelineBackendResolver, PercussionPipelineInvocation};
use crate::percussion_import::{
    PercussionImportService, build_clip_detection_options, get_trigger_layer_name,
};
use crate::percussion_parser::{JsonPercussionAnalysisParser, PercussionAnalysisParser};
use crate::percussion_planner::PercussionTimelinePlanner;
use crate::process_runner::{ExternalProcessRunner, ProcessHandle, ProcessRunnerImpl};

// ──────────────────────────────────────
// CONSTANTS (preserved from Unity)
// ──────────────────────────────────────

const PERCUSSION_PROGRESS_UNKNOWN: f32 = -1.0;
const PERCUSSION_NUDGE_STEP_BEATS: f32 = 0.25;

/// Unity: StemAudioController.StemCount = 4
const STEM_COUNT: usize = 4;

// ──────────────────────────────────────
// STATUS COLOR TYPE
// ──────────────────────────────────────

/// Port of Unity Color used for status messages. [r, g, b, a].
pub type StatusColor = [f32; 4];

const COLOR_GREY: StatusColor = [0.72, 0.72, 0.72, 1.0];
const COLOR_BLUE: StatusColor = [0.7, 0.85, 1.0, 1.0];
const COLOR_GREEN: StatusColor = [0.5, 0.95, 0.6, 1.0];
const COLOR_ORANGE: StatusColor = [1.0, 0.75, 0.35, 1.0];
const COLOR_RED: StatusColor = [1.0, 0.4, 0.35, 1.0];

pub use crate::percussion_progress_parser::{PercussionPipelineProgressParser, PipelineProgress};

// ──────────────────────────────────────
// Pipeline state machine
// ──────────────────────────────────────

/// Internal phase of an active pipeline run.
/// Encodes the sequential logic of RunPercussionPipelineAsync.
enum PipelineRunState {
    /// Currently running the single backend. `handle` is the live process.
    Running {
        handle: ProcessHandle,
        latest_process_line: String,
        /// Most recent parsed progress (phase label + 0..1). Retained across
        /// ticks so the status keeps showing the last known phase between the
        /// process's output lines.
        latest_progress: PipelineProgress,
    },
    /// Done (success or failure stored in `result`).
    Done {
        ok: bool,
        /// Last failure detail (used for diagnostics logging).
        details: String,
    },
}

/// Top-level orchestrator async operation phase.
/// Each coroutine from Unity maps to a variant here.
enum OrchestratorPhase {
    Idle,

    // ── Per-clip detection (audio-clip-detection) ──
    DetectClip(Box<DetectClipState>),
}

// ──────────────────────────────────────
// Per-operation state structs
// ──────────────────────────────────────

/// Per-clip detection: analyze one audio clip's file and place its triggers.
/// The clip owns the result (cached analysis + tagged triggers); no global state.
struct DetectClipState {
    clip_id: ClipId,
    sub_phase: DetectClipSubPhase,
    temp_output_json: String,
    invocation: PercussionPipelineInvocation,
    pipeline_run: Option<PipelineRunState>,
}

enum DetectClipSubPhase {
    RunningPipeline,
    ApplyingResult { pipeline_ok: bool },
}

// ──────────────────────────────────────
// PercussionImportOrchestrator
// ──────────────────────────────────────

/// Port of Unity PercussionImportOrchestrator.
/// Manages all percussion import operations.
/// Plain struct (not MonoBehaviour). Driven by the host calling tick() each frame.
pub struct PercussionImportOrchestrator {
    // ── Settings ──
    pipeline_settings: Option<PercussionPipelineSettings>,
    application_data_path: String,

    // ── Internal helpers ──
    percussion_analysis_parser: JsonPercussionAnalysisParser,
    pipeline_progress_parser: PercussionPipelineProgressParser,

    // ── Runtime state ──
    percussion_import_in_progress: bool,
    phase: OrchestratorPhase,

    // ── Status display ──
    percussion_import_status_message: String,
    percussion_import_status_color: StatusColor,
    percussion_import_status_animate: bool,
    percussion_import_status_clear_time: f32,
    percussion_import_status_progress01: f32,
    percussion_import_status_show_progress: bool,
}

impl PercussionImportOrchestrator {
    pub fn new(
        pipeline_settings: Option<PercussionPipelineSettings>,
        application_data_path: String,
    ) -> Self {
        Self {
            pipeline_settings,
            application_data_path,
            percussion_analysis_parser: JsonPercussionAnalysisParser,
            pipeline_progress_parser: PercussionPipelineProgressParser,
            percussion_import_in_progress: false,
            phase: OrchestratorPhase::Idle,
            percussion_import_status_message: String::new(),
            percussion_import_status_color: COLOR_GREY,
            percussion_import_status_animate: false,
            percussion_import_status_clear_time: -1.0,
            percussion_import_status_progress01: PERCUSSION_PROGRESS_UNKNOWN,
            percussion_import_status_show_progress: false,
        }
    }

    // ──────────────────────────────────────
    // STATUS PROPERTIES (polled by host)
    // ──────────────────────────────────────

    pub fn status_message(&self) -> &str {
        &self.percussion_import_status_message
    }

    pub fn status_color(&self) -> StatusColor {
        self.percussion_import_status_color
    }

    pub fn status_animating(&self) -> bool {
        self.percussion_import_status_animate
    }

    pub fn status_progress01(&self) -> f32 {
        self.percussion_import_status_progress01
    }

    pub fn show_progress_bar(&self) -> bool {
        self.percussion_import_status_show_progress
    }

    pub fn is_import_in_progress(&self) -> bool {
        self.percussion_import_in_progress
    }

    pub fn nudge_step_beats() -> f32 {
        PERCUSSION_NUDGE_STEP_BEATS
    }

    // ──────────────────────────────────────
    // FRAME TICK (called by host each Update)
    // ──────────────────────────────────────

    /// Called each frame by the host. Drives the state machine and handles the auto-clear timer.
    /// `unscaled_time` is `Time.unscaledTime` equivalent (seconds since startup, ignoring pause).
    pub fn tick(
        &mut self,
        unscaled_time: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        _current_beat: f32,
    ) {
        // Auto-clear status timer.
        // When set_percussion_import_status is called it stores -(interval) as a pending value.
        // On the first tick after that, we convert to an absolute deadline using current time.
        // This matches Unity's `Time.unscaledTime + clearAfterSeconds` pattern.
        if self.percussion_import_status_clear_time < -1.0 {
            // Convert pending negative interval to absolute deadline.
            self.percussion_import_status_clear_time =
                unscaled_time + (-self.percussion_import_status_clear_time);
        }
        if !self.percussion_import_in_progress
            && self.percussion_import_status_clear_time > 0.0
            && unscaled_time >= self.percussion_import_status_clear_time
        {
            self.clear_percussion_import_status();
        }

        // Drive the state machine.
        match &self.phase {
            OrchestratorPhase::Idle => {}
            OrchestratorPhase::DetectClip(_) => {
                self.tick_detect_clip(unscaled_time, project, editing_service);
            }
        }
    }

    // ──────────────────────────────────────
    // PUBLIC ENTRY POINTS
    // ──────────────────────────────────────


    /// Per-clip detection entry (audio-clip-detection). Analyze one audio clip's
    /// file and place its triggers, owned by that clip. Runs the same backend as
    /// the legacy wizard but caches the analysis on the clip and writes no
    /// project-global state. The clip must already exist (dropped on an audio
    /// layer); detection is a manual action, not automatic on drop.
    pub fn detect_clip(&mut self, clip_id: ClipId, project: &mut Project) {
        if self.percussion_import_in_progress {
            self.set_percussion_import_status(
                "Detection already running",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        // Resolve the clip's audio file (geometry is read at apply time).
        let audio_path = match project.timeline.find_clip_by_id(&clip_id) {
            Some(c) if c.is_audio() => c.audio_file_path.clone(),
            _ => {
                self.set_percussion_import_status(
                    "Detect: not an audio clip",
                    COLOR_RED,
                    false,
                    3.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        if audio_path.trim().is_empty() || !std::path::Path::new(&audio_path).exists() {
            self.set_percussion_import_status(
                "Detect: audio file not found",
                COLOR_RED,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let temp_output_json = Self::build_temp_percussion_output_path(&audio_path);
        let invocation = if let Some(settings) = self.pipeline_settings.as_ref() {
            PercussionPipelineBackendResolver::build_invocation_with_settings(
                &self.application_data_path,
                &audio_path,
                &temp_output_json,
                settings,
                None,
            )
        } else {
            PercussionPipelineBackendResolver::build_invocation(
                &self.application_data_path,
                &audio_path,
                &temp_output_json,
            )
        };

        let invocation = match invocation {
            Some(inv) => inv,
            None => {
                log::error!(
                    "[PercussionImportOrchestrator] Detection backend unavailable. \
                    Expected bundled runtime at Resources/{}",
                    PercussionPipelineBackendResolver::BUNDLED_RUNTIME_FOLDER_NAME
                );
                self.set_percussion_import_status(
                    "Detect: analysis backend missing",
                    COLOR_RED,
                    false,
                    4.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        self.percussion_import_in_progress = true;
        self.set_percussion_import_status(
            "Detecting triggers",
            COLOR_BLUE,
            true,
            0.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            true,
        );
        self.phase = OrchestratorPhase::DetectClip(Box::new(DetectClipState {
            clip_id,
            sub_phase: DetectClipSubPhase::RunningPipeline,
            temp_output_json,
            invocation,
            pipeline_run: None,
        }));
    }


    fn tick_detect_clip(
        &mut self,
        _unscaled_time: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        let state = match &mut self.phase {
            OrchestratorPhase::DetectClip(s) => s,
            _ => return,
        };

        match &state.sub_phase {
            DetectClipSubPhase::RunningPipeline => {
                let (done, ok, details, progress) = drive_pipeline_run(
                    &mut state.pipeline_run,
                    &state.invocation,
                    &self.pipeline_progress_parser,
                );

                // Surface the real backend phase ("separating stems", "writing
                // analysis JSON", …) and its 0..1 progress instead of a static
                // "Detecting triggers" with an indeterminate bar.
                self.percussion_import_status_message = match &progress {
                    Some(p) if !p.message.is_empty() => p.message.clone(),
                    _ => "Detecting triggers".to_string(),
                };
                self.percussion_import_status_color = COLOR_BLUE;
                self.percussion_import_status_animate = true;
                self.percussion_import_status_progress01 = match &progress {
                    Some(p) if p.has_progress => p.progress01,
                    _ => PERCUSSION_PROGRESS_UNKNOWN,
                };
                self.percussion_import_status_show_progress = true;

                if done {
                    if !ok {
                        log::error!(
                            "[PercussionImportOrchestrator] Clip detection failed. {}",
                            details
                        );
                        if let OrchestratorPhase::DetectClip(s) = &self.phase {
                            let _ = std::fs::remove_file(&s.temp_output_json);
                        }
                        self.set_percussion_import_status(
                            "Detect: analysis failed",
                            COLOR_RED,
                            false,
                            6.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                        self.percussion_import_in_progress = false;
                        self.phase = OrchestratorPhase::Idle;
                    } else if let OrchestratorPhase::DetectClip(s) = &mut self.phase {
                        s.sub_phase = DetectClipSubPhase::ApplyingResult { pipeline_ok: true };
                    }
                }
            }

            DetectClipSubPhase::ApplyingResult { pipeline_ok } => {
                let pipeline_ok = *pipeline_ok;
                let (clip_id, temp_json) = if let OrchestratorPhase::DetectClip(s) = &self.phase {
                    (s.clip_id.clone(), s.temp_output_json.clone())
                } else {
                    return;
                };

                if pipeline_ok {
                    self.apply_clip_detection(&clip_id, &temp_json, project, editing_service);
                }
                let _ = std::fs::remove_file(&temp_json);

                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }
        }
    }

    /// Parse the detection JSON, cache it on the clip, plan from the clip's
    /// `DetectionConfig`, and place its triggers (tagged with the clip id).
    /// Returns whether any clips were placed.
    fn apply_clip_detection(
        &mut self,
        clip_id: &ClipId,
        temp_json: &str,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) -> bool {
        let raw_json = match std::fs::read_to_string(temp_json) {
            Ok(j) if !j.trim().is_empty() => j,
            _ => {
                self.set_percussion_import_status(
                    "Detect: no analysis output",
                    COLOR_ORANGE,
                    false,
                    5.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return false;
            }
        };

        let analysis = match self.percussion_analysis_parser.try_parse(&raw_json) {
            Ok(a) => a,
            Err(parse_error) => {
                log::error!(
                    "[PercussionImportOrchestrator] Clip detection parse failed: {}",
                    parse_error
                );
                self.set_percussion_import_status(
                    "Detect: JSON parse failed",
                    COLOR_RED,
                    false,
                    6.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return false;
            }
        };

        // Detect path only: record the file's true tempo from a confident
        // detection. recorded_bpm is the clip's native tempo, and the analysis
        // events are in file-seconds — so the warp-on placement anchor
        // (60/recorded_bpm) needs the real BPM, not the project-tempo default the
        // warp toggle seeds. Set directly (no length rescale, per the chosen
        // behaviour) and only when warp is already on (recorded_bpm > 0), so
        // detection never flips a native-speed clip into warp.
        const CLIP_BPM_CONFIDENCE: f32 = 0.72;
        let detected = analysis.bpm.0;
        if detected.is_finite()
            && detected > 0.0
            && analysis.bpm_confidence >= CLIP_BPM_CONFIDENCE
            && let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id)
            && clip.recorded_bpm > 0.0
        {
            clip.set_recorded_bpm(detected);
        }

        // Detect path only: pre-fill routing so the inspector dropdowns land on
        // sensible layers without 9 manual picks. Only fills instruments left on
        // "Auto" (target_layer == None); never overrides an explicit choice.
        Self::auto_route_unset_instruments(clip_id, project);

        let placed = self.plan_and_apply_for_clip(clip_id, &analysis, project, editing_service);
        // Detect-and-Group (§8): split the demucs stems into analysis-only lanes,
        // wrap source + stems + trigger lanes in a named group, and route each
        // stem to its own send. The pipeline reports the persisted stem paths in
        // its JSON (`stemPaths`); we trust those rather than guessing the cache
        // layout. Lane-keyed reuse across clips on the same lane. No-ops if the
        // pipeline reported no stems.
        let stems = parse_pipeline_stem_paths(&raw_json);
        self.build_detect_group(clip_id, &stems, project, editing_service);
        placed
    }

    /// Detect-and-Group (§8): after triggers are placed, split the demucs stems
    /// into analysis-only audio lanes, route each to a send, and wrap the source
    /// lane + stems + trigger lanes in a named group. Keyed to the **source audio
    /// lane** (`Layer.detect_group_source`): re-detecting any clip on that lane
    /// reuses the same stem lanes + sends instead of making a second set. Recorded
    /// as one undo step. `stems` is the pipeline-reported persisted stem paths in
    /// drums/bass/other/vocals order; no-ops if it carries none.
    fn build_detect_group(
        &mut self,
        clip_id: &ClipId,
        stems: &[Option<String>],
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        const STEM_DISPLAY: [&str; STEM_COUNT] = ["Drums", "Bass", "Other", "Vocals"];
        // D8: role identity for each stem slot, index-for-index with
        // STEM_DISPLAY — the pair must never drift apart.
        const STEM_ROLES: [DetectStemRole; STEM_COUNT] = [
            DetectStemRole::Drums,
            DetectStemRole::Bass,
            DetectStemRole::Other,
            DetectStemRole::Vocals,
        ];

        // 1. Source clip geometry + the lane it sits on.
        let Some((
            source_layer_id,
            start_beat,
            duration_beats,
            in_point,
            source_duration,
            recorded_bpm,
            audio_path,
        )) = project.timeline.layers.iter().find_map(|l| {
            l.clips.iter().find(|c| c.id == *clip_id).map(|c| {
                (
                    l.layer_id.clone(),
                    c.start_beat,
                    c.duration_beats,
                    c.in_point,
                    c.source_duration,
                    c.recorded_bpm,
                    c.audio_file_path.clone(),
                )
            })
        }) else {
            return;
        };

        // 2. Stems the pipeline persisted, in drums/bass/other/vocals order. Only
        //    paths that still exist on disk count — a stale JSON shouldn't make a
        //    lane that points at nothing.
        let present: Vec<(usize, String)> = stems
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.clone().map(|p| (i, p)))
            .filter(|(_, p)| std::path::Path::new(p).exists())
            .collect();
        if present.is_empty() {
            log::warn!(
                "[DetectAndGroup] pipeline reported no stems for '{}' — placed triggers but skipped grouping",
                audio_path
            );
            return;
        }

        let base = std::path::Path::new(&audio_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Audio")
            .to_string();
        let spb =
            manifold_core::tempo::TempoMapConverter::seconds_per_beat_from_bpm(project.settings.bpm.0);

        // 3. Lane-keyed reuse: an existing group already built for this lane?
        let existing_group = project
            .timeline
            .layers
            .iter()
            .find(|l| l.detect_group_source.as_ref() == Some(&source_layer_id))
            .map(|l| l.layer_id.clone());

        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        // Clear this clip's prior stem clips (re-detect) so they don't pile up.
        for layer in project.timeline.layers.iter_mut() {
            let lid = layer.layer_id.clone();
            let stale: Vec<_> = layer
                .clips
                .iter()
                .filter(|c| c.is_audio() && c.detection_source.as_ref() == Some(clip_id))
                .cloned()
                .collect();
            for c in stale {
                commands.push(Box::new(DeleteClipCommand::new(c.clone(), lid.clone())));
                layer.remove_clip(&c.id);
            }
        }

        // 4. For each stem: ensure a lane (reuse or create), add its clip, ensure a send.
        //    Reuse is keyed by role first (D8) — survives the source song
        //    being renamed or replaced. Falls back to today's exact-name
        //    match only for pre-role projects (where no lane carries a role
        //    yet), and stamps the role the moment a lane is found that way,
        //    so every later detect on this lane goes through the role path.
        let mut new_stem_layers: Vec<LayerId> = Vec::new();
        // D8: whether any stem lane below actually followed the song to a
        // new base this call — the signal that the whole set (not just one
        // hand-renamed straggler) is still untouched, and the group's own
        // name is safe to follow too.
        let mut group_rename_warranted = false;
        for (i, stem_path) in present {
            let stem_role = STEM_ROLES[i];
            let lane_name = format!("{base} \u{00b7} {}", STEM_DISPLAY[i]);

            let by_role = existing_group.as_ref().and_then(|gid| {
                project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| {
                        l.parent_layer_id.as_ref() == Some(gid)
                            && l.is_audio()
                            && l.detect_stem_role == Some(stem_role)
                    })
                    .map(|l| l.layer_id.clone())
            });
            // Legacy fallback for pre-role projects: a same-parent audio lane
            // whose name still equals the CURRENT base's expected name. Only
            // finds the lane if the song hasn't also been renamed in the same
            // step — D8 stops there deliberately (no eager migration pass);
            // the lane is stamped with its role the moment this finds it, so
            // every detect after this one goes through `by_role` instead.
            let existing_lane = by_role.or_else(|| {
                existing_group.as_ref().and_then(|gid| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| {
                            l.parent_layer_id.as_ref() == Some(gid)
                                && l.is_audio()
                                && l.detect_stem_role.is_none()
                                && l.name == lane_name
                        })
                        .map(|l| l.layer_id.clone())
                })
            });

            let lane_id = match existing_lane {
                Some(id) => {
                    let old_name = project
                        .timeline
                        .find_layer_by_id(&id)
                        .map(|(_, l)| l.name.clone())
                        .unwrap_or_default();

                    // Stamp the role on first touch (lazy migration, D8) —
                    // a no-op once the lane already carries it. Not itself
                    // wrapped in a Command: like `detect_group_source`
                    // below, it's one-way bookkeeping the composite's
                    // structural undo doesn't need to reverse.
                    if let Some((_, l)) = project.timeline.find_layer_by_id_mut(&id)
                        && l.detect_stem_role != Some(stem_role)
                    {
                        l.detect_stem_role = Some(stem_role);
                    }

                    // D8 rename-on-reuse: only when the song's base actually
                    // changed AND this lane still carries the exact
                    // auto-generated shape — a hand-renamed lane is left
                    // alone even though it's still the right lane by role.
                    if old_name != lane_name && is_auto_stem_name(&old_name, STEM_DISPLAY[i]) {
                        let mut rename_lane = RenameLayerCommand::new(
                            id.clone(),
                            old_name.clone(),
                            lane_name.clone(),
                        );
                        rename_lane.execute(project);
                        commands.push(Box::new(rename_lane));
                        group_rename_warranted = true;

                        if let Some(send) = project.audio_setup.send_for_layer(&id)
                            && is_auto_stem_name(&send.label, STEM_DISPLAY[i])
                            && send.label != lane_name
                        {
                            let send_id = send.id.clone();
                            let old_label = send.label.clone();
                            let mut rename_send = RenameAudioSendCommand::new(
                                send_id,
                                old_label,
                                lane_name.clone(),
                            );
                            rename_send.execute(project);
                            commands.push(Box::new(rename_send));
                        }
                    }
                    id
                }
                None => {
                    let insert_index = project.timeline.layers.len();
                    let mut add = AddLayerCommand::new(
                        lane_name.clone(),
                        LayerType::Audio,
                        manifold_core::PresetTypeId::NONE,
                        insert_index,
                        existing_group.clone(),
                    );
                    add.execute(project);
                    let Some(id) = project
                        .timeline
                        .layers
                        .get(insert_index)
                        .map(|l| l.layer_id.clone())
                    else {
                        continue;
                    };
                    // Stem lanes default to analysis-only: silent to master,
                    // hot to send. Role is stamped right at creation, so a
                    // freshly built lane is never found by name again.
                    if let Some((_, l)) = project.timeline.find_layer_by_id_mut(&id) {
                        l.analysis_only = true;
                        l.detect_stem_role = Some(stem_role);
                    }
                    commands.push(Box::new(add));
                    new_stem_layers.push(id.clone());
                    id
                }
            };

            // Stem clip mirrors the source clip's placement + warp.
            let mut clip = manifold_core::clip::TimelineClip::new_audio(
                stem_path,
                start_beat,
                duration_beats,
                in_point,
                source_duration,
            );
            clip.detection_source = Some(clip_id.clone());
            if recorded_bpm > 0.0 {
                clip.set_recorded_bpm(recorded_bpm);
            }
            let mut add_clip = AddClipCommand::new(clip, lane_id.clone(), spb);
            add_clip.execute(project);
            commands.push(Box::new(add_clip));

            // One send per stem, reused by source.
            if project.audio_setup.send_for_layer(&lane_id).is_none() {
                let send = AudioSend::new(lane_name.clone());
                let send_id = send.id.clone();
                let mut add_send = AddAudioSendCommand::new(send);
                add_send.execute(project);
                commands.push(Box::new(add_send));
                let mut bind = SetLayerAudioSendCommand::new(lane_id.clone(), Some(send_id));
                bind.execute(project);
                commands.push(Box::new(bind));
            }
        }

        // D8: the group's name follows the song's base too, but only when
        // at least one stem lane above actually followed it (proof the set
        // is still untouched since it was built) AND the group's own name
        // hasn't already drifted from `base` for some other reason — a
        // hand-renamed group is otherwise left alone.
        if group_rename_warranted
            && let Some(gid) = existing_group.as_ref()
            && let Some((_, g)) = project.timeline.find_layer_by_id(gid)
            && g.name != base
        {
            let old_name = g.name.clone();
            let mut rename_group = RenameLayerCommand::new(gid.clone(), old_name, base.clone());
            rename_group.execute(project);
            commands.push(Box::new(rename_group));
        }

        // 5. First detect: group source + stems + trigger lanes, name + mark it.
        if existing_group.is_none() && !new_stem_layers.is_empty() {
            let mut to_group: Vec<LayerId> = vec![source_layer_id.clone()];
            to_group.extend(new_stem_layers.iter().cloned());
            for layer in &project.timeline.layers {
                if layer.layer_type == LayerType::Group || to_group.contains(&layer.layer_id) {
                    continue;
                }
                if layer
                    .clips
                    .iter()
                    .any(|c| c.detection_source.as_ref() == Some(clip_id))
                {
                    to_group.push(layer.layer_id.clone());
                }
            }
            let original_order: Vec<Layer> = project
                .timeline
                .layers
                .iter()
                .filter(|l| to_group.contains(&l.layer_id))
                .cloned()
                .collect();
            let mut group_cmd = GroupLayersCommand::new(to_group, original_order);
            group_cmd.execute(project);
            commands.push(Box::new(group_cmd));
            // Name + mark the new group (the source lane's freshly-set parent).
            if let Some(gid) = project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == source_layer_id)
                .and_then(|l| l.parent_layer_id.clone())
                && let Some((_, g)) = project.timeline.find_layer_by_id_mut(&gid)
            {
                g.name = base.clone();
                g.detect_group_source = Some(source_layer_id.clone());
            }
        }

        // 6. One undo step for the whole set.
        if !commands.is_empty() {
            editing_service.record(Box::new(CompositeCommand::new(
                commands,
                "Detect and Group".to_string(),
            )));
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    /// For each enabled instrument still routed to "Auto", point it at an
    /// existing non-group layer whose name matches the trigger ("Kick", "Snare",
    /// …) if one exists. Routing already falls back to by-name, so this only
    /// pre-populates the dropdown selection; it never creates layers.
    fn auto_route_unset_instruments(clip_id: &ClipId, project: &mut Project) {
        let name_to_id: Vec<(String, manifold_core::id::LayerId)> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.layer_type != LayerType::Group)
            .map(|l| (l.name.clone(), l.layer_id.clone()))
            .collect();
        if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
            let detection = clip
                .audio_detection
                .get_or_insert_with(AudioClipDetection::new);
            for inst in detection.config.instruments.iter_mut() {
                if !inst.enabled || inst.target_layer.is_some() {
                    continue;
                }
                let want = get_trigger_layer_name(inst.trigger_type);
                if let Some((_, id)) = name_to_id.iter().find(|(n, _)| *n == want) {
                    inst.target_layer = Some(id.clone());
                }
            }
        }
    }

    /// Re-place a clip's triggers from its cached analysis, with no backend run.
    /// Drives the live inspector knobs (sensitivity / quantize / onset / routing)
    /// — they all act at plan/apply time, so a re-plan is instant. Caches nothing
    /// new (the analysis is already on the clip from a prior Detect).
    pub fn replan_clip(
        &mut self,
        clip_id: ClipId,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        if self.percussion_import_in_progress {
            self.set_percussion_import_status(
                "Detection running",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let analysis = match project
            .timeline
            .find_clip_by_id_mut(&clip_id)
            .and_then(|c| c.audio_detection.as_ref())
            .and_then(|d| d.analysis.clone())
        {
            Some(a) => a,
            None => {
                self.set_percussion_import_status(
                    "Replan: detect first",
                    COLOR_ORANGE,
                    false,
                    3.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        self.plan_and_apply_for_clip(&clip_id, &analysis, project, editing_service);
    }

    /// Remove every trigger this audio clip produced (tagged `detection_source`),
    /// as one undoable step. Leaves other clips' triggers and hand-placed clips.
    pub fn clear_clip_triggers(
        &mut self,
        clip_id: ClipId,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        for layer in project.timeline.layers.iter_mut() {
            let layer_lid = layer.layer_id.clone();
            let owned: Vec<_> = layer
                .clips
                .iter()
                .filter(|c| c.detection_source.as_ref() == Some(&clip_id))
                .cloned()
                .collect();
            for clip in owned {
                commands.push(Box::new(DeleteClipCommand::new(clip.clone(), layer_lid.clone())));
                layer.remove_clip(&clip.id);
            }
        }

        if commands.is_empty() {
            self.set_percussion_import_status(
                "No triggers to clear",
                COLOR_GREY,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        project.timeline.mark_clip_lookup_dirty();
        let count = commands.len();
        let command: Box<dyn Command> = if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(manifold_editing::command::CompositeCommand::new(
                commands,
                "Clear clip triggers".to_string(),
            ))
        };
        editing_service.record(command);

        self.set_percussion_import_status(
            &format!("Cleared {} clips", count),
            COLOR_GREEN,
            false,
            3.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            false,
        );
    }

    /// Cache `analysis` on the clip, build the clip-anchored options from its
    /// `DetectionConfig`, plan, and place its triggers (tagged with the clip id,
    /// clearing only this clip's prior triggers). Shared by Detect (post-backend)
    /// and Replan (from the cached analysis — no backend run). Returns whether
    /// any clips were placed.
    fn plan_and_apply_for_clip(
        &mut self,
        clip_id: &ClipId,
        analysis: &PercussionAnalysisData,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) -> bool {
        // Cache the analysis on the clip (the clip owns its events), read its
        // detection config, and build the clip-anchored, warp-aware converter
        // from the clip's geometry. The energy envelope rides inside the cached
        // analysis, so no project-global state is written.
        let project_bpm = project.settings.bpm;
        let (config, anchor) = {
            let clip = match project.timeline.find_clip_by_id_mut(clip_id) {
                Some(c) => c,
                None => return false,
            };
            let anchor = ClipDetectionAnchor::new(
                clip.start_beat,
                clip.duration_beats,
                clip.in_point,
                clip.recorded_bpm,
                project_bpm,
            );
            let detection = clip
                .audio_detection
                .get_or_insert_with(AudioClipDetection::new);
            detection.analysis = Some(analysis.clone());
            (detection.config.clone(), anchor)
        };

        let options =
            build_clip_detection_options(project, self.pipeline_settings.as_ref(), &config, anchor);

        let binding_resolver = ProjectPercussionBindingResolver::new(project, &options);
        let plan = {
            let beat_time_converter = ProjectBeatTimeConverter::new(project);
            let mut planner = PercussionTimelinePlanner::new(
                Box::new(beat_time_converter),
                Box::new(binding_resolver),
            );
            planner.build_plan(Some(analysis), Some(&options))
        };

        // Per-instrument trigger counts for the inspector rows ("Kick 64").
        // Recomputed every plan/replan; written even when empty so a tightened
        // sensitivity that drops an instrument to zero clears its stale count.
        let mut counts: HashMap<PercussionTriggerType, u32> = HashMap::new();
        for placement in plan.placements() {
            *counts.entry(placement.trigger_type).or_insert(0) += 1;
        }
        if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id)
            && let Some(detection) = clip.audio_detection.as_mut()
        {
            detection.last_counts = counts;
        }

        if plan.accepted_events() == 0 {
            self.set_percussion_import_status(
                "No accepted triggers",
                COLOR_ORANGE,
                false,
                6.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return false;
        }

        let import_service =
            PercussionImportService::new_with_settings(self.pipeline_settings.as_ref());
        let result =
            import_service.apply_placement_plan(project, Some(&plan), Some(&options), Some(clip_id));

        if result.success && let Some(cmd) = result.undo_command {
            editing_service.record(cmd);
        }

        self.set_percussion_import_status(
            &format!("Placed {} clips", result.added_clips),
            COLOR_GREEN,
            false,
            5.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            false,
        );
        result.success
    }

    /// Port of Unity SetPercussionImportStatus().
    fn set_percussion_import_status(
        &mut self,
        message: &str,
        color: StatusColor,
        animate: bool,
        clear_after_seconds: f32,
        progress01: f32,
        show_progress_bar: bool,
    ) {
        self.percussion_import_status_message = message.to_string();
        self.percussion_import_status_color = color;
        self.percussion_import_status_animate = animate;
        self.percussion_import_status_clear_time = if clear_after_seconds > 0.0 {
            // The host must add current unscaled_time to this; we store as relative here,
            // then the tick() will use it. Unity does `Time.unscaledTime + clearAfterSeconds`.
            // To match, we need current time. But we don't have it here. Store as a delta
            // and tick() adds current time on first check.
            // Actually to match Unity exactly: store it relative and convert in tick().
            // We store the sentinel "pending" as a positive value meaning "set from now".
            // Implementation: store negative of the interval, tick() converts on first use.
            // Simplest: caller always passes unscaled_time to set_status. But this is a private
            // helper. We'll use the pattern of storing clear_time as the interval (negative = use
            // unscaled_time on first tick). See tick() where we compare >= clear_time.
            // For simplicity: we'll use f32::MAX as "unset" and the interval as positive.
            // On each tick, if clear_time > 0 and we haven't started the timer, we can't.
            // Pragmatic solution: set_status stores the clear_time as a raw value the host
            // sets by calling set_clear_time_now(). The tick() method adds current time.
            // Actually the simplest matching approach: store as -(clear_after_seconds).
            // tick() checks: if clear_time < 0, convert to absolute on first tick.
            -(clear_after_seconds)
        } else {
            -1.0
        };
        self.percussion_import_status_progress01 = if progress01 >= 0.0 {
            progress01.clamp(0.0, 1.0)
        } else {
            PERCUSSION_PROGRESS_UNKNOWN
        };
        self.percussion_import_status_show_progress = show_progress_bar;
    }

    /// Port of Unity ClearPercussionImportStatus().
    fn clear_percussion_import_status(&mut self) {
        self.percussion_import_status_message = String::new();
        self.percussion_import_status_animate = false;
        self.percussion_import_status_clear_time = -1.0;
        self.percussion_import_status_progress01 = PERCUSSION_PROGRESS_UNKNOWN;
        self.percussion_import_status_show_progress = false;
    }


    // ──────────────────────────────────────
    // STATIC HELPERS
    // ──────────────────────────────────────

    /// Port of Unity BuildTempPercussionOutputPath().
    pub fn build_temp_percussion_output_path(input_audio_path: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        let file_name = Path::new(input_audio_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("audio");
        let unique = format!("{:016x}{:08x}", ts, seq);
        let output_name = format!("{}.{}.percussion.json", file_name, unique);
        std::env::temp_dir()
            .join(output_name)
            .to_string_lossy()
            .into_owned()
    }

    /// Port of Unity ResolveOutputPathFromArguments().
    pub fn resolve_output_path_from_arguments(arguments: &[String]) -> Option<String> {
        if arguments.is_empty() {
            return None;
        }

        for i in 0..arguments.len().saturating_sub(1) {
            if arguments[i] == "-o" || arguments[i] == "--output" {
                return Some(arguments[i + 1].clone());
            }
        }

        None
    }

    /// Port of Unity EscapeJsonValue().
    pub fn escape_json_value(value: &str) -> String {
        if value.is_empty() {
            return String::new();
        }
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    }
}

// ──────────────────────────────────────
// Pipeline runner helper (drives PipelineRunState)
// ──────────────────────────────────────

/// Drive the pipeline run one tick. Returns (done, ok, last_details, progress).
/// `progress` is the most recent parsed phase (label + 0..1), `None` until the
/// backend emits a recognizable line. This is the Rust equivalent of
/// `RunPercussionPipelineAsync`'s per-frame logic. With the simplified
/// bundled-only backend, there is exactly one invocation to run.
fn drive_pipeline_run(
    pipeline_run: &mut Option<PipelineRunState>,
    invocation: &PercussionPipelineInvocation,
    parser: &PercussionPipelineProgressParser,
) -> (bool, bool, String, Option<PipelineProgress>) {
    // If no active run, start the invocation.
    if pipeline_run.is_none() {
        if invocation.command.trim().is_empty() {
            let details = "No analysis backend invocation candidates were resolved.".to_string();
            return (true, false, details, None);
        }

        log::info!(
            "[PercussionImportOrchestrator] Starting percussion backend: label='{}' cmd='{}'",
            invocation.label,
            invocation.command
        );

        let args_refs: Vec<&str> = invocation.arguments.iter().map(|s| s.as_str()).collect();
        let handle = ProcessRunnerImpl::run_async(&invocation.command, &args_refs);

        if handle.is_finished() && handle.exit_code() == Some(-1) {
            let details = format!(
                "label='{}' cmd='{}' failed to start.",
                invocation.label, invocation.command
            );
            *pipeline_run = Some(PipelineRunState::Done {
                ok: false,
                details: details.clone(),
            });
            return (true, false, details, None);
        }

        *pipeline_run = Some(PipelineRunState::Running {
            handle,
            latest_process_line: String::new(),
            latest_progress: PipelineProgress::default(),
        });
    }

    let (finished, exit_code, new_lines, latest_line, progress) = match pipeline_run.as_mut() {
        Some(PipelineRunState::Running {
            handle,
            latest_process_line,
            latest_progress,
        }) => {
            let new_lines = handle.poll();
            let mut latest = latest_process_line.clone();
            // Parse progress lines; keep the most recent one that carried a phase.
            for line_info in &new_lines {
                if line_info.line.trim().is_empty() {
                    continue;
                }
                latest = line_info.line.trim().to_string();
                let parsed = parser.parse_line(&line_info.line, line_info.is_stderr);
                if parsed.has_progress {
                    *latest_progress = parsed;
                }
            }
            *latest_process_line = latest.clone();
            let finished = handle.is_finished();
            let exit_code = handle.exit_code();
            let progress = if latest_progress.has_progress {
                Some(latest_progress.clone())
            } else {
                None
            };
            (finished, exit_code, new_lines, latest, progress)
        }
        Some(PipelineRunState::Done { ok, details }) => {
            let ok = *ok;
            let details = details.clone();
            return (true, ok, details, None);
        }
        None => {
            return (true, false, "No pipeline run active.".to_string(), None);
        }
    };
    let _ = &new_lines;

    if !finished {
        // Not done yet — return without advancing.
        return (false, false, String::new(), progress);
    }

    // Process finished. Check result.
    let exit_code = exit_code.unwrap_or(-1);

    let output_json =
        PercussionImportOrchestrator::resolve_output_path_from_arguments(&invocation.arguments);
    let output_exists = output_json
        .as_deref()
        .is_some_and(|p| std::path::Path::new(p).exists());

    if exit_code == 0 && output_exists {
        // Success.
        log::info!(
            "[PercussionImportOrchestrator] Percussion pipeline: analysis complete (backend='{}')",
            invocation.label
        );
        *pipeline_run = Some(PipelineRunState::Done {
            ok: true,
            details: String::new(),
        });
        return (true, true, String::new(), None);
    }

    // Failure — single backend, no fallback.
    let stderr_acc = match pipeline_run.as_ref() {
        Some(PipelineRunState::Running { handle, .. }) => handle.stderr().to_string(),
        _ => String::new(),
    };

    let details = format!(
        "label='{}' cmd='{}' exit={}. {}{}",
        invocation.label,
        invocation.command,
        exit_code,
        if !stderr_acc.trim().is_empty() {
            format!("stderr: {} ", stderr_acc.trim())
        } else {
            String::new()
        },
        if !latest_line.is_empty() {
            format!("lastLog: {}", latest_line)
        } else {
            String::new()
        },
    );
    log::warn!(
        "[PercussionImportOrchestrator] Percussion backend failed: {}",
        details
    );

    *pipeline_run = Some(PipelineRunState::Done {
        ok: false,
        details: details.clone(),
    });
    (true, false, details, None)
}

// ──────────────────────────────────────
// MODULE-LEVEL HELPERS
// ──────────────────────────────────────

/// Parse the pipeline's reported persisted stem paths (`stemPaths` in the
/// analysis JSON) into drums/bass/other/vocals order. The pipeline owns the
/// demucs cache layout and reports absolute paths, so the host never has to
/// reconstruct the on-disk location. Returns four slots (a `None` slot means the
/// pipeline did not persist that stem); all `None` when stem caching was off.
fn parse_pipeline_stem_paths(raw_json: &str) -> Vec<Option<String>> {
    #[derive(serde::Deserialize)]
    struct StemPathsDto {
        #[serde(rename = "stemPaths", default)]
        stem_paths: Option<std::collections::HashMap<String, String>>,
    }
    const STEM_NAMES: [&str; STEM_COUNT] = ["drums", "bass", "other", "vocals"];

    let map = serde_json::from_str::<StemPathsDto>(raw_json)
        .ok()
        .and_then(|d| d.stem_paths)
        .unwrap_or_default();

    STEM_NAMES
        .iter()
        .map(|name| {
            map.get(*name)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .collect()
}

/// D8's rename-on-reuse gate: true if `name` still has the exact
/// `"<anything> \u{00b7} {stem}"` shape the Detect-and-Group naming
/// convention produces — the signal that a lane or send name is still
/// auto-generated and safe to follow when the source song's base changes.
/// Any other shape (a fully custom name, or one missing the stem suffix)
/// means it's been hand-edited; the caller leaves those alone.
fn is_auto_stem_name(name: &str, stem_display: &str) -> bool {
    name.ends_with(&format!(" \u{00b7} {stem_display}"))
}



#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::Beats;
    use manifold_core::audio_clip_detection::AudioClipDetection;
    use manifold_core::clip::TimelineClip;
    use manifold_core::percussion_analysis::{PercussionEvent, PercussionTriggerType};
    use manifold_core::types::LayerType;
    use manifold_core::units::{Bpm, Seconds};
    use manifold_core::PresetTypeId;
    use manifold_editing::service::EditingService;

    fn orchestrator() -> PercussionImportOrchestrator {
        PercussionImportOrchestrator::new(None, String::new())
    }

    #[test]
    fn clear_clip_triggers_removes_only_tagged() {
        let mut project = Project::default();
        let idx = project
            .timeline
            .add_layer("Kick", LayerType::Generator, PresetTypeId::from_string("Gen".into()));
        let audio_a = ClipId::new("audioA");
        {
            let layer = &mut project.timeline.layers[idx];
            let mut a = TimelineClip::new_generator(Beats(1.0), Beats(0.25));
            a.detection_source = Some(audio_a.clone());
            layer.restore_clip(a);
            let mut b = TimelineClip::new_generator(Beats(5.0), Beats(0.25));
            b.detection_source = Some(ClipId::new("audioB"));
            layer.restore_clip(b);
            layer.restore_clip(TimelineClip::new_generator(Beats(9.0), Beats(0.25)));
        }
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        orchestrator().clear_clip_triggers(audio_a.clone(), &mut project, &mut es);

        let clips = &project.timeline.layers[idx].clips;
        assert_eq!(clips.len(), 2, "only audioA's trigger removed");
        assert!(clips.iter().all(|c| c.detection_source.as_ref() != Some(&audio_a)));
        assert!(es.undo(&mut project), "clear is one undoable step");
        assert_eq!(project.timeline.layers[idx].clips.len(), 3, "undo restores it");
    }

    #[test]
    fn replan_clip_places_from_cache_without_backend() {
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        let mut clip =
            TimelineClip::new_audio("song.wav".into(), Beats(0.0), Beats(64.0), Seconds(0.0), Seconds(120.0));
        // Seed a cached analysis with one kick (as if a prior Detect ran).
        let analysis = PercussionAnalysisData::new_simple(
            "t",
            Bpm(120.0),
            vec![PercussionEvent::new(PercussionTriggerType::Kick, 0.5, 0.9, 0.0)],
        );
        clip.audio_detection = Some(AudioClipDetection {
            config: Default::default(),
            analysis: Some(analysis),
            ..Default::default()
        });
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        orchestrator().replan_clip(clip_id.clone(), &mut project, &mut es);

        // A kick trigger was placed somewhere, tagged with the source clip — no
        // backend run, no temp file.
        let placed = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .any(|c| c.detection_source.as_ref() == Some(&clip_id));
        assert!(placed, "replan places triggers from the cached analysis");

        // The per-instrument count is recorded on the clip for the inspector.
        let kick_count = project
            .timeline
            .find_clip_by_id(&clip_id)
            .and_then(|c| c.audio_detection.as_ref())
            .map(|d| d.count(PercussionTriggerType::Kick))
            .unwrap_or(0);
        assert_eq!(kick_count, 1, "replan records the placed-kick count");
    }

    #[test]
    fn replan_clip_without_analysis_is_noop() {
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        let clip =
            TimelineClip::new_audio("song.wav".into(), Beats(0.0), Beats(8.0), Seconds(0.0), Seconds(8.0));
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let before: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let mut es = EditingService::new();
        orchestrator().replan_clip(clip_id, &mut project, &mut es);
        let after: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        assert_eq!(before, after, "no cached analysis -> nothing placed");
    }

    #[test]
    fn detect_group_builds_lanes_sends_group_and_reuses_by_lane() {
        // Stems the pipeline "persisted" — real files so the on-disk guard passes.
        let dir = std::env::temp_dir().join(format!("manifold_dg_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stem_for = |name: &str| {
            let p = dir.join(format!("{name}.wav"));
            std::fs::write(&p, b"").unwrap();
            Some(p.to_string_lossy().into_owned())
        };
        let stems = vec![
            stem_for("drums"),
            stem_for("bass"),
            stem_for("other"),
            stem_for("vocals"),
        ];

        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Source", LayerType::Audio, PresetTypeId::NONE);
        let source_lane = project.timeline.layers[aidx].layer_id.clone();
        let clip_a = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(0.0),
            Beats(64.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_a_id = clip_a.id.clone();
        project.timeline.layers[aidx].restore_clip(clip_a);
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        orchestrator().build_detect_group(&clip_a_id, &stems, &mut project, &mut es);

        // 4 analysis-only stem lanes named "<track> · <Stem>", each with the
        // source clip's geometry and its own send.
        for stem in ["Drums", "Bass", "Other", "Vocals"] {
            let want = format!("MyTrack \u{00b7} {stem}");
            let lane = project
                .timeline
                .layers
                .iter()
                .find(|l| l.name == want)
                .unwrap_or_else(|| panic!("missing stem lane {want}"));
            assert!(lane.analysis_only, "{want} is analysis-only");
            assert_eq!(lane.clips.len(), 1, "{want} has one stem clip");
            assert_eq!(
                lane.clips[0].detection_source.as_ref(),
                Some(&clip_a_id),
                "{want} clip tagged with its source"
            );
            assert!(
                project.audio_setup.send_for_layer(&lane.layer_id).is_some(),
                "{want} routed to a send"
            );
        }

        // A group named after the track, keyed to the source lane, with the
        // source lane reparented under it.
        let group = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_type == LayerType::Group)
            .expect("a group was created");
        assert_eq!(group.name, "MyTrack");
        assert_eq!(group.detect_group_source.as_ref(), Some(&source_lane));
        let source_parent = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_id == source_lane)
            .and_then(|l| l.parent_layer_id.clone());
        assert_eq!(
            source_parent.as_ref(),
            Some(&group.layer_id),
            "source lane is inside the group"
        );

        let sends_after_first = project.audio_setup.sends.len();
        let layers_after_first = project.timeline.layers.len();
        assert_eq!(sends_after_first, 4, "one send per stem");

        // Re-detect a second clip on the SAME lane reuses the set: no new lanes,
        // no new group, no new sends — just another stem clip per lane.
        let clip_b = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(64.0),
            Beats(64.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_b_id = clip_b.id.clone();
        // Grouping reordered the layer vector, so `aidx` is stale — address the
        // source lane by id.
        let src_idx = project
            .timeline
            .layers
            .iter()
            .position(|l| l.layer_id == source_lane)
            .expect("source lane still present");
        project.timeline.layers[src_idx].restore_clip(clip_b);
        project.timeline.mark_clip_lookup_dirty();
        orchestrator().build_detect_group(&clip_b_id, &stems, &mut project, &mut es);

        assert_eq!(
            project.timeline.layers.len(),
            layers_after_first,
            "reuse adds no new lanes or group"
        );
        assert_eq!(
            project.audio_setup.sends.len(),
            sends_after_first,
            "reuse adds no new sends"
        );
        let drums_lane = project
            .timeline
            .layers
            .iter()
            .find(|l| l.name == "MyTrack \u{00b7} Drums")
            .unwrap();
        assert_eq!(
            drums_lane.clips.len(),
            2,
            "second detect adds a stem clip to the existing lane"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_group_noops_without_stems() {
        // No persisted stems -> triggers were placed elsewhere, but no lanes,
        // group, or sends get built.
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Source", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(0.0),
            Beats(8.0),
            Seconds(0.0),
            Seconds(8.0),
        );
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let layers_before = project.timeline.layers.len();
        let mut es = EditingService::new();
        let empty: Vec<Option<String>> = vec![None, None, None, None];
        orchestrator().build_detect_group(&clip_id, &empty, &mut project, &mut es);

        assert_eq!(project.timeline.layers.len(), layers_before, "no stems -> no lanes/group");
        assert!(project.audio_setup.sends.is_empty(), "no stems -> no sends");
    }

    #[test]
    fn detect_group_is_one_undo_step() {
        let dir = std::env::temp_dir().join(format!("manifold_dg_undo_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stems: Vec<Option<String>> = ["drums", "bass", "other", "vocals"]
            .iter()
            .map(|n| {
                let p = dir.join(format!("{n}.wav"));
                std::fs::write(&p, b"").unwrap();
                Some(p.to_string_lossy().into_owned())
            })
            .collect();

        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Source", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(0.0),
            Beats(32.0),
            Seconds(0.0),
            Seconds(60.0),
        );
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let layers_before = project.timeline.layers.len();
        let mut es = EditingService::new();
        orchestrator().build_detect_group(&clip_id, &stems, &mut project, &mut es);
        assert!(project.timeline.layers.len() > layers_before, "set was built");
        assert_eq!(project.audio_setup.sends.len(), 4, "four sends created");

        // One undo reverses the whole set — lanes, group, and sends — in a single
        // step, so a misfire on stage is one cmd-Z, not seven.
        assert!(es.undo(&mut project), "detect-and-group is undoable");
        assert_eq!(
            project.timeline.layers.len(),
            layers_before,
            "undo removes the stem lanes + group"
        );
        assert!(project.audio_setup.sends.is_empty(), "undo removes the sends");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_group_reuse_follows_song_rename_by_role() {
        // D8: stem lanes/sends/group are keyed by role, not name — replacing
        // the source clip's file under a different song name (simulated here
        // since P4's ReplaceAudioFileCommand is a separate phase) must reuse
        // the exact same lanes and sends, renamed to the new base, and never
        // spawn a second set.
        let dir = std::env::temp_dir().join(format!("manifold_dg_rename_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stem_for = |name: &str| {
            let p = dir.join(format!("{name}.wav"));
            std::fs::write(&p, b"").unwrap();
            Some(p.to_string_lossy().into_owned())
        };
        let stems = vec![
            stem_for("drums"),
            stem_for("bass"),
            stem_for("other"),
            stem_for("vocals"),
        ];

        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Source", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(0.0),
            Beats(64.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        orchestrator().build_detect_group(&clip_id, &stems, &mut project, &mut es);

        let lane_ids_before: std::collections::HashSet<LayerId> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio() && l.detect_stem_role.is_some())
            .map(|l| l.layer_id.clone())
            .collect();
        assert_eq!(lane_ids_before.len(), 4, "four role-tagged stem lanes exist");
        let send_ids_before: std::collections::HashSet<_> = project
            .audio_setup
            .sends
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(send_ids_before.len(), 4);
        let layers_after_first = project.timeline.layers.len();

        // Simulate P4's ReplaceAudioFileCommand: the clip now points at a
        // differently-named song file.
        project
            .timeline
            .find_clip_by_id_mut(&clip_id)
            .unwrap()
            .audio_file_path = "DifferentSong.wav".to_string();

        // Re-detect the same clip under the new name.
        orchestrator().build_detect_group(&clip_id, &stems, &mut project, &mut es);

        assert_eq!(
            project.timeline.layers.len(),
            layers_after_first,
            "reuse by role adds no new lanes or group"
        );
        let send_ids_after: std::collections::HashSet<_> = project
            .audio_setup
            .sends
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(send_ids_after, send_ids_before, "same sends reused, zero new sends");

        let lane_ids_after: std::collections::HashSet<LayerId> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio() && l.detect_stem_role.is_some())
            .map(|l| l.layer_id.clone())
            .collect();
        assert_eq!(lane_ids_after, lane_ids_before, "same lane IDs reused, zero new lanes");

        // Names followed the rename: lane, its send, and the group.
        for stem in ["Drums", "Bass", "Other", "Vocals"] {
            let want = format!("DifferentSong \u{00b7} {stem}");
            let lane = project
                .timeline
                .layers
                .iter()
                .find(|l| l.name == want)
                .unwrap_or_else(|| panic!("stem lane not renamed to '{want}'"));
            assert_eq!(
                lane.clips.len(),
                1,
                "{want} still carries exactly one stem clip after re-detect"
            );
            let send = project.audio_setup.send_for_layer(&lane.layer_id).unwrap();
            assert_eq!(send.label, want, "send label followed the rename");
        }
        let group = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_type == LayerType::Group)
            .unwrap();
        assert_eq!(group.name, "DifferentSong", "group name followed the rename");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_group_reuse_leaves_hand_renamed_lane_untouched() {
        // D8's other half: a lane the user has renamed away from the
        // "<base> · <Stem>" shape must never be clobbered by a later
        // re-detect under a new song name, even though it's still found (by
        // role) and still reused for the new stem clip.
        let dir = std::env::temp_dir().join(format!("manifold_dg_handname_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let stem_for = |name: &str| {
            let p = dir.join(format!("{name}.wav"));
            std::fs::write(&p, b"").unwrap();
            Some(p.to_string_lossy().into_owned())
        };
        let stems = vec![
            stem_for("drums"),
            stem_for("bass"),
            stem_for("other"),
            stem_for("vocals"),
        ];

        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Source", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "MyTrack.wav".into(),
            Beats(0.0),
            Beats(64.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        orchestrator().build_detect_group(&clip_id, &stems, &mut project, &mut es);

        // Peter hand-renames the Drums lane to something of his own choosing.
        let drums_id = project
            .timeline
            .layers
            .iter()
            .find(|l| l.name == "MyTrack \u{00b7} Drums")
            .unwrap()
            .layer_id
            .clone();
        project
            .timeline
            .find_layer_by_id_mut(&drums_id)
            .unwrap()
            .1
            .name = "Kick Stems".to_string();

        project
            .timeline
            .find_clip_by_id_mut(&clip_id)
            .unwrap()
            .audio_file_path = "DifferentSong.wav".to_string();
        orchestrator().build_detect_group(&clip_id, &stems, &mut project, &mut es);

        let drums = project
            .timeline
            .find_layer_by_id(&drums_id)
            .map(|(_, l)| l)
            .expect("hand-renamed lane is still reused by role, not recreated");
        assert_eq!(drums.name, "Kick Stems", "hand-renamed lane is never clobbered");
        assert_eq!(
            drums.clips.len(),
            1,
            "still reused for the new stem clip despite the custom name"
        );

        // The other three lanes, never hand-renamed, still follow the song.
        let bass = project
            .timeline
            .layers
            .iter()
            .find(|l| l.name == "DifferentSong \u{00b7} Bass")
            .expect("untouched sibling lane still follows the rename");
        assert_eq!(bass.clips.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
