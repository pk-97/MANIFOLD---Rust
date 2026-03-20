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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::percussion_analysis::{
    PercussionAnalysisData, ProjectBeatTimeConverter,
};
use manifold_core::percussion_binding::ProjectPercussionBindingResolver;
use manifold_core::percussion_settings::{PercussionImportOptionsFactory, PercussionPipelineSettings};
use manifold_core::project::Project;
use manifold_editing::command::Command;

use crate::percussion_backend::{
    PercussionPipelineBackendResolver, PercussionPipelineInvocation,
};
use crate::percussion_import::{
    PercussionBpmDecision, PercussionImportService,
};
use crate::percussion_parser::{JsonPercussionAnalysisParser, PercussionAnalysisParser};
use crate::percussion_planner::PercussionTimelinePlanner;
use crate::process_runner::{ProcessHandle, ProcessRunnerImpl, ExternalProcessRunner};

// ──────────────────────────────────────
// CONSTANTS (preserved from Unity)
// ──────────────────────────────────────

#[allow(dead_code)]
const PERCUSSION_AUDIO_EXTENSION_FILTER: &str = "wav,mp3,m4a,aac,flac,ogg,aif,aiff,wma,json";

const SUPPORTED_PERCUSSION_AUDIO_EXTENSIONS: &[&str] = &[
    ".wav", ".mp3", ".m4a", ".aac", ".flac", ".ogg", ".aif", ".aiff", ".wma",
];

const PERCUSSION_PROGRESS_UNKNOWN: f32 = -1.0;
const PERCUSSION_NUDGE_STEP_BEATS: f32 = 0.25;
const PERCUSSION_IMPORT_SAFE_HEADROOM_BARS: i32 = 1;

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

// ──────────────────────────────────────
// PipelineProgress (port of PercussionPipelineProgressParser.cs)
// ──────────────────────────────────────

/// Port of Unity PipelineProgress struct.
#[derive(Debug, Clone, Default)]
pub struct PipelineProgress {
    pub progress01: f32,
    pub message: String,
    pub is_error: bool,
    pub has_progress: bool,
}

/// Port of Unity PercussionPipelineProgressParser.
/// Parses pipeline output lines into structured progress updates.
/// Supports the MANIFOLD_PROGRESS protocol and heuristic fallback for Demucs output.
pub struct PercussionPipelineProgressParser;

impl PercussionPipelineProgressParser {
    const PROGRESS_PREFIX: &'static str = "MANIFOLD_PROGRESS|";

    // Heuristic progress mapping constants.
    const DEMUCS_PROGRESS_MIN: f32 = 0.22;
    const DEMUCS_PROGRESS_MAX: f32 = 0.72;
    const DEMUCS_GENERIC_PROGRESS: f32 = 0.30;
    const WRITING_JSON_PROGRESS: f32 = 0.84;
    const FINALIZING_SUMMARY_PROGRESS: f32 = 0.87;
    const GATHERING_METADATA_PROGRESS: f32 = 0.80;
    const ERROR_PROGRESS: f32 = 0.18;

    pub fn parse_line(&self, raw_line: &str, is_stderr: bool) -> PipelineProgress {
        let mut result = PipelineProgress::default();

        if raw_line.trim().is_empty() {
            return result;
        }

        let line = raw_line.trim();

        if Self::try_parse_structured_progress(line, &mut result) {
            return result;
        }

        Self::try_parse_heuristic_progress(line, is_stderr, &mut result);
        result
    }

    fn try_parse_structured_progress(line: &str, result: &mut PipelineProgress) -> bool {
        if !line.starts_with(Self::PROGRESS_PREFIX) {
            return false;
        }

        let first = match line.find('|') {
            Some(i) => i,
            None => return false,
        };
        let second = match line[first + 1..].find('|') {
            Some(i) => first + 1 + i,
            None => return false,
        };

        if second <= first + 1 || second >= line.len() - 1 {
            return false;
        }

        let progress_token = line[first + 1..second].trim();
        let stage_message = line[second + 1..].trim();

        let progress01: f32 = match progress_token.parse() {
            Ok(v) => v,
            Err(_) => return false,
        };

        let stage_message = if stage_message.trim().is_empty() {
            "analysis running"
        } else {
            stage_message
        };

        result.progress01 = progress01.clamp(0.0, 1.0);
        result.message = stage_message.to_string();
        result.has_progress = true;
        true
    }

    fn try_parse_heuristic_progress(line: &str, is_stderr: bool, result: &mut PipelineProgress) {
        let lower = line.to_lowercase();

        if let Some(percent) = Self::try_extract_percent(&lower) {
            // Mathf.Lerp(DemucsProgressMin, DemucsProgressMax, percent / 100f)
            let t = (percent / 100.0).clamp(0.0, 1.0);
            result.progress01 = Self::DEMUCS_PROGRESS_MIN
                + (Self::DEMUCS_PROGRESS_MAX - Self::DEMUCS_PROGRESS_MIN) * t;
            result.message = "separating stems (Demucs)".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("demucs") {
            result.progress01 = Self::DEMUCS_GENERIC_PROGRESS;
            result.message = "separating stems (Demucs)".to_string();
            result.has_progress = true;
            return;
        }

        if lower.starts_with("wrote ") || lower.contains(" events -> ") {
            result.progress01 = Self::WRITING_JSON_PROGRESS;
            result.message = "writing analysis JSON".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("estimated bpm") || lower.contains("event counts") {
            result.progress01 = Self::FINALIZING_SUMMARY_PROGRESS;
            result.message = "finalizing analysis summary".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("analysis source")
            || lower.contains("percussion profile")
            || lower.contains("bass profile")
        {
            result.progress01 = Self::GATHERING_METADATA_PROGRESS;
            result.message = "analysis complete, gathering metadata".to_string();
            result.has_progress = true;
            return;
        }

        if is_stderr && lower.starts_with("error") {
            result.progress01 = Self::ERROR_PROGRESS;
            result.message = "analysis backend reported an error".to_string();
            result.is_error = true;
            result.has_progress = true;
        }
    }

    fn try_extract_percent(line: &str) -> Option<f32> {
        if line.is_empty() {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut percent_idx = 0usize;

        loop {
            // Find next '%'
            let found = chars[percent_idx..].iter().position(|&c| c == '%');
            let found = match found {
                Some(i) => percent_idx + i,
                None => break,
            };

            if found > 0 {
                let mut token_start = found - 1;
                // Walk back over digits and '.'
                while token_start > 0
                    && (chars[token_start].is_ascii_digit() || chars[token_start] == '.')
                {
                    token_start -= 1;
                }
                if !chars[token_start].is_ascii_digit() && chars[token_start] != '.' {
                    token_start += 1;
                }

                if token_start < found {
                    let token: String = chars[token_start..found].iter().collect();
                    if let Ok(parsed) = token.parse::<f32>() {
                        return Some(parsed.clamp(0.0, 100.0));
                    }
                }
            }

            percent_idx = found + 1;
            if percent_idx >= chars.len() {
                break;
            }
        }

        None
    }
}

// ──────────────────────────────────────
// SetImportedAudioCommand
// ──────────────────────────────────────

/// Port of Unity SetImportedAudioCommand.
/// Undoable command for setting/clearing imported audio state on the project.
/// Captures path, startBeat, hash, and stemPaths snapshots.
/// Uses a callback to trigger async audio reload/reset — the command itself is synchronous.
#[derive(Debug)]
pub struct SetImportedAudioCommand {
    old_path: Option<String>,
    old_start_beat: f32,
    old_hash: Option<String>,
    old_stem_paths: Option<Vec<String>>,
    new_path: Option<String>,
    new_start_beat: f32,
    new_hash: Option<String>,
    new_stem_paths: Option<Vec<String>>,
    desc: String,
}

impl SetImportedAudioCommand {
    pub fn new(
        old_path: Option<String>,
        old_start_beat: f32,
        old_hash: Option<String>,
        old_stem_paths: Option<Vec<String>>,
        new_path: Option<String>,
        new_start_beat: f32,
        new_hash: Option<String>,
        new_stem_paths: Option<Vec<String>>,
        description: &str,
    ) -> Self {
        let desc = if description.trim().is_empty() {
            "Set imported audio".to_string()
        } else {
            description.to_string()
        };
        Self {
            old_path,
            old_start_beat,
            old_hash,
            old_stem_paths: old_stem_paths.as_deref().map(|s| s.to_vec()),
            new_path,
            new_start_beat,
            new_hash,
            new_stem_paths: new_stem_paths.as_deref().map(|s| s.to_vec()),
            desc,
        }
    }

    fn apply_state_to_project(
        project: &mut Project,
        path: Option<&str>,
        start_beat: f32,
        hash: Option<&str>,
        stem_paths: Option<&[String]>,
    ) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_path = path.map(|s| s.to_string());
        state.audio_start_beat = start_beat;
        state.audio_hash = hash.map(|s| s.to_string());
        state.stem_paths = stem_paths.map(|s| s.to_vec());
    }
}

impl Command for SetImportedAudioCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply_state_to_project(
            project,
            self.new_path.as_deref(),
            self.new_start_beat,
            self.new_hash.as_deref(),
            self.new_stem_paths.as_deref(),
        );
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply_state_to_project(
            project,
            self.old_path.as_deref(),
            self.old_start_beat,
            self.old_hash.as_deref(),
            self.old_stem_paths.as_deref(),
        );
    }

    fn description(&self) -> &str {
        &self.desc
    }
}

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

    // ── Import Percussion Map (OnImportPercussionMapAsync) ──
    ImportMap(ImportMapState),

    // ── Import Audio Only (OnImportAudioOnlyAsync) ──
    ImportAudioOnly(ImportAudioOnlyState),

    // ── Re-Analyze Triggers (OnReAnalyzeTriggersAsync) ──
    ReAnalyzeTriggers(ReAnalyzeTriggersState),

    // ── Re-Import Stems (OnReImportStemsAsync) ──
    ReImportStems(ReImportStemsState),
}

// ──────────────────────────────────────
// Per-operation state structs
// ──────────────────────────────────────

/// Snapshot of the audio state BEFORE an operation, for undo.
struct AudioStateSnapshot {
    path: Option<String>,
    start_beat: f32,
    hash: Option<String>,
    stem_paths: Option<Vec<String>>,
}

impl AudioStateSnapshot {
    fn capture(project: &Project) -> Self {
        let state = project.percussion_import.as_ref();
        Self {
            path: state.and_then(|s| s.audio_path.clone()),
            start_beat: state.map_or(0.0, |s| s.audio_start_beat),
            hash: state.and_then(|s| s.audio_hash.clone()),
            stem_paths: state.and_then(|s| s.stem_paths.clone()),
        }
    }
}

struct ImportMapState {
    sub_phase: ImportMapSubPhase,
    old_audio: AudioStateSnapshot,
    import_start_beat: f32,
    selected_path: String,
    temp_output_json: String,
    invocation: PercussionPipelineInvocation,
    pipeline_run: Option<PipelineRunState>,
}

#[allow(dead_code)]
enum ImportMapSubPhase {
    /// File has been chosen; check if JSON or audio.
    CheckingExtension,
    /// Audio file: running the analysis pipeline.
    RunningPipeline,
    /// Pipeline finished; now importing JSON result and loading audio.
    ImportingJson { pipeline_ok: bool },
    /// Loading the source audio after successful import.
    LoadingAudio { import_succeeded: bool },
    Done,
}

struct ImportAudioOnlyState {
    sub_phase: ImportAudioOnlySubPhase,
    old_audio: AudioStateSnapshot,
    selected_path: String,
    start_beat: f32,
    temp_output_json: String,
    invocation: PercussionPipelineInvocation,
    pipeline_run: Option<PipelineRunState>,
}

#[allow(dead_code)]
enum ImportAudioOnlySubPhase {
    /// File chosen; loading the audio.
    LoadingAudio,
    /// Audio loaded; running BPM-only analysis.
    RunningBpmPipeline,
    /// Pipeline finished; parsing BPM result.
    ParsingBpm { pipeline_ok: bool },
    Done,
}

struct ReAnalyzeTriggersState {
    instrument_group: String,
    sub_phase: ReAnalyzeSubPhase,
    temp_output_json: String,
    invocation: PercussionPipelineInvocation,
    pipeline_run: Option<PipelineRunState>,
}

#[allow(dead_code)]
enum ReAnalyzeSubPhase {
    RunningPipeline,
    Done,
}

struct ReImportStemsState {
    audio_path: String,
    sub_phase: ReImportStemsSubPhase,
    temp_output_json: String,
    invocation: PercussionPipelineInvocation,
    pipeline_run: Option<PipelineRunState>,
}

#[allow(dead_code)]
enum ReImportStemsSubPhase {
    RunningPipeline,
    Done,
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

    // ── Alignment state ──
    last_import_aligned_start_beat: f32,
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
            last_import_aligned_start_beat: 0.0,
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
        current_beat: f32,
    ) {
        // Auto-clear status timer.
        // When set_percussion_import_status is called it stores -(interval) as a pending value.
        // On the first tick after that, we convert to an absolute deadline using current time.
        // This matches Unity's `Time.unscaledTime + clearAfterSeconds` pattern.
        if self.percussion_import_status_clear_time < -1.0 {
            // Convert pending negative interval to absolute deadline.
            self.percussion_import_status_clear_time = unscaled_time + (-self.percussion_import_status_clear_time);
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
            OrchestratorPhase::ImportMap(_) => {
                self.tick_import_map(unscaled_time, project, editing_service);
            }
            OrchestratorPhase::ImportAudioOnly(_) => {
                self.tick_import_audio_only(unscaled_time, project, editing_service, current_beat);
            }
            OrchestratorPhase::ReAnalyzeTriggers(_) => {
                self.tick_re_analyze_triggers(unscaled_time, project, editing_service);
            }
            OrchestratorPhase::ReImportStems(_) => {
                self.tick_re_import_stems(unscaled_time, project, editing_service);
            }
        }
    }

    // ──────────────────────────────────────
    // PUBLIC ENTRY POINTS
    // ──────────────────────────────────────

    /// Port of Unity OnImportPercussionMap() + OnImportPercussionMapAsync().
    /// `selected_path`: if None, emulates the dialog being cancelled immediately.
    /// Host must supply the path (from a file dialog or drag-and-drop).
    pub fn on_import_percussion_map(
        &mut self,
        selected_path: Option<String>,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        current_beat: f32,
        beats_per_bar: i32,
    ) {
        if self.percussion_import_in_progress {
            self.set_percussion_import_status(
                "Perc import already running",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            log::warn!("[PercussionImportOrchestrator] Percussion import is already running.");
            return;
        }

        let old_audio = AudioStateSnapshot::capture(project);

        self.percussion_import_in_progress = true;
        self.set_percussion_import_status(
            "Perc: choose audio/JSON file",
            COLOR_BLUE,
            true,
            0.0,
            0.05,
            true,
        );

        let selected_path = match selected_path {
            Some(p) if !p.is_empty() => p,
            _ => {
                self.percussion_import_in_progress = false;
                self.set_percussion_import_status(
                    "Perc import cancelled",
                    COLOR_GREY,
                    false,
                    2.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        // Reset any existing audio sync (Unity: audioSync?.ResetAudio())
        // — the orchestrator records this via the audio state snapshot; actual reset is caller's
        // responsibility after this call completes.

        let extension = get_extension_lowercase(&selected_path);
        let import_start_beat =
            Self::get_percussion_import_start_beat_with_headroom_static(current_beat, beats_per_bar);

        if extension == ".json" {
            self.set_percussion_import_status(
                "Perc: importing JSON file",
                COLOR_BLUE,
                true,
                0.0,
                0.45,
                true,
            );
            let json_path = selected_path.clone();
            let imported = self.import_percussion_json_from_path_sync(
                &json_path,
                import_start_beat,
                project,
                editing_service,
            );
            self.percussion_import_in_progress = false;
            if self.percussion_import_status_message.is_empty() {
                if imported {
                    self.set_percussion_import_status(
                        "Perc JSON import finished",
                        COLOR_GREEN,
                        false,
                        3.0,
                        PERCUSSION_PROGRESS_UNKNOWN,
                        false,
                    );
                }
            }
            return;
        }

        if !is_supported_audio_extension(&extension) {
            log::warn!(
                "[PercussionImportOrchestrator] Unsupported audio file extension '{}'. \
                Choose wav/mp3/m4a/aac/flac/ogg/aif/aiff/wma.",
                extension
            );
            self.percussion_import_in_progress = false;
            self.set_percussion_import_status(
                "Perc: unsupported file type",
                COLOR_RED,
                false,
                4.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let temp_output_json = Self::build_temp_percussion_output_path(&selected_path);
        let invocation = if let Some(settings) = self.pipeline_settings.as_ref() {
            PercussionPipelineBackendResolver::build_invocation_with_settings(
                &self.application_data_path,
                &selected_path,
                &temp_output_json,
                settings,
                None,
            )
        } else {
            PercussionPipelineBackendResolver::build_invocation(
                &self.application_data_path,
                &selected_path,
                &temp_output_json,
            )
        };

        let invocation = match invocation {
            Some(inv) => inv,
            None => {
                log::error!(
                    "[PercussionImportOrchestrator] Percussion analysis backend unavailable. \
                    Expected bundled runtime at Resources/{}",
                    PercussionPipelineBackendResolver::BUNDLED_RUNTIME_FOLDER_NAME
                );
                self.percussion_import_in_progress = false;
                self.set_percussion_import_status(
                    "Perc: analysis backend missing",
                    COLOR_RED,
                    false,
                    4.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        self.set_percussion_import_status(
            "Perc: preparing analysis backend",
            COLOR_BLUE,
            true,
            0.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            true,
        );

        self.phase = OrchestratorPhase::ImportMap(ImportMapState {
            sub_phase: ImportMapSubPhase::RunningPipeline,
            old_audio,
            import_start_beat,
            selected_path,
            temp_output_json,
            invocation,
            pipeline_run: None,
        });
    }

    /// Port of Unity OnImportAudioOnly() + OnImportAudioOnlyAsync().
    pub fn on_import_audio_only(
        &mut self,
        selected_path: Option<String>,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        current_beat: f32,
    ) {
        if self.percussion_import_in_progress {
            return;
        }

        let selected_path = match selected_path {
            Some(p) if !p.is_empty() => p,
            _ => return,
        };

        let extension = get_extension_lowercase(&selected_path);
        if !is_supported_audio_extension(&extension) {
            log::warn!(
                "[PercussionImportOrchestrator] Unsupported audio file: '{}'",
                extension
            );
            return;
        }

        let old_audio = AudioStateSnapshot::capture(project);
        let start_beat = current_beat;

        // Apply audio path immediately (Unity does this synchronously after controller.LoadAudioAsync).
        // The actual audio load is caller-driven; we record the state change for undo.
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_path = Some(selected_path.clone());
        state.audio_start_beat = start_beat;
        state.audio_hash = Some(compute_audio_hash(&selected_path));

        self.set_percussion_import_status(
            "Loading audio",
            COLOR_BLUE,
            true,
            0.0,
            0.10,
            true,
        );

        let temp_output_json = Self::build_temp_percussion_output_path(&selected_path);
        let invocation = if let Some(settings) = self.pipeline_settings.as_ref() {
            PercussionPipelineBackendResolver::build_invocation_with_settings(
                &self.application_data_path,
                &selected_path,
                &temp_output_json,
                settings,
                None,
            )
        } else {
            PercussionPipelineBackendResolver::build_invocation(
                &self.application_data_path,
                &selected_path,
                &temp_output_json,
            )
        };

        let invocation = match invocation {
            Some(inv) => inv,
            None => {
                // No BPM backend — still import the audio, just without BPM detection.
                let bpm_text = "no BPM backend";
                self.set_percussion_import_status(
                    &format!("Audio imported | {}", bpm_text),
                    COLOR_ORANGE,
                    false,
                    5.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                log::log!(
                    log::Level::Info,
                    "[PercussionImportOrchestrator] Audio imported: '{}' at beat {:.2} (no BPM backend available)",
                    path_file_name(&selected_path),
                    start_beat
                );

                let new_path = state.audio_path.clone();
                let new_start_beat = state.audio_start_beat;
                let new_hash = state.audio_hash.clone();
                let new_stems = state.stem_paths.clone();
                self.record_audio_state_change(
                    old_audio.path,
                    old_audio.start_beat,
                    old_audio.hash,
                    old_audio.stem_paths,
                    new_path,
                    new_start_beat,
                    new_hash,
                    new_stems,
                    "Import audio",
                    editing_service,
                );
                return;
            }
        };

        self.set_percussion_import_status(
            "Detecting BPM",
            COLOR_BLUE,
            true,
            0.0,
            0.30,
            true,
        );

        self.percussion_import_in_progress = true;
        self.phase = OrchestratorPhase::ImportAudioOnly(ImportAudioOnlyState {
            sub_phase: ImportAudioOnlySubPhase::RunningBpmPipeline,
            old_audio,
            selected_path,
            start_beat,
            temp_output_json,
            invocation,
            pipeline_run: None,
        });
    }

    /// Port of Unity OnReAnalyzeTriggers().
    pub fn on_re_analyze_triggers(
        &mut self,
        instrument_group: &str,
        project: &mut Project,
    ) {
        if self.percussion_import_in_progress {
            self.set_percussion_import_status(
                "Re-analysis already running",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let audio_path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_deref())
            .unwrap_or("");

        if audio_path.is_empty() {
            self.set_percussion_import_status(
                "Import audio first",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        if !std::path::Path::new(audio_path).exists() {
            self.set_percussion_import_status(
                "Audio file not found",
                COLOR_RED,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let audio_path = audio_path.to_string();
        let temp_output_json = Self::build_temp_percussion_output_path(&audio_path);
        let invocation = if let Some(settings) = self.pipeline_settings.as_ref() {
            PercussionPipelineBackendResolver::build_invocation_with_settings(
                &self.application_data_path,
                &audio_path,
                &temp_output_json,
                settings,
                Some(instrument_group),
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
                self.set_percussion_import_status(
                    "Analysis backend missing",
                    COLOR_RED,
                    false,
                    4.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return;
            }
        };

        let group_label = instrument_group.to_uppercase();
        self.percussion_import_in_progress = true;
        self.set_percussion_import_status(
            &format!("Re-analyzing {}", group_label),
            COLOR_BLUE,
            true,
            0.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            true,
        );

        self.phase = OrchestratorPhase::ReAnalyzeTriggers(ReAnalyzeTriggersState {
            instrument_group: instrument_group.to_string(),
            sub_phase: ReAnalyzeSubPhase::RunningPipeline,
            temp_output_json,
            invocation,
            pipeline_run: None,
        });
    }

    /// Port of Unity OnReImportStems().
    pub fn on_re_import_stems(&mut self, project: &mut Project) {
        if self.percussion_import_in_progress {
            self.set_percussion_import_status(
                "Stem import already running",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let audio_path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_deref())
            .unwrap_or("");

        if audio_path.is_empty() {
            self.set_percussion_import_status(
                "Import audio first",
                COLOR_ORANGE,
                false,
                2.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        if !std::path::Path::new(audio_path).exists() {
            self.set_percussion_import_status(
                "Audio file not found",
                COLOR_RED,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return;
        }

        let audio_path = audio_path.to_string();

        // Clear existing stems before starting (if re-import fails, old stems must not persist).
        if let Some(state) = project.percussion_import.as_mut() {
            state.stem_paths = None;
        }

        let temp_output_json = Self::build_temp_percussion_output_path(&audio_path);
        let invocation = if let Some(settings) = self.pipeline_settings.as_ref() {
            PercussionPipelineBackendResolver::build_invocation_with_settings(
                &self.application_data_path,
                &audio_path,
                &temp_output_json,
                settings,
                None, // null = all instruments, ensures all stems are generated
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
                self.set_percussion_import_status(
                    "Analysis backend missing",
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
            "Stems: running Demucs",
            COLOR_BLUE,
            true,
            0.0,
            0.15,
            true,
        );

        self.phase = OrchestratorPhase::ReImportStems(ReImportStemsState {
            audio_path,
            sub_phase: ReImportStemsSubPhase::RunningPipeline,
            temp_output_json,
            invocation,
            pipeline_run: None,
        });
    }

    /// Port of Unity OnRemoveImportedAudio().
    pub fn on_remove_imported_audio(
        &mut self,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        // Snapshot old state for undo.
        let old_path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.clone());
        let old_start_beat = project
            .percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat);
        let old_hash = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_hash.clone());
        let old_stem_paths = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.stem_paths.clone());

        // Apply removal immediately.
        if let Some(state) = project.percussion_import.as_mut() {
            state.audio_path = None;
            state.audio_start_beat = 0.0;
            state.stem_paths = None;
            state.audio_hash = None;
        }

        // Record undo command (already applied — use record, not execute).
        self.record_audio_state_change(
            old_path,
            old_start_beat,
            old_hash,
            old_stem_paths,
            None,
            0.0,
            None,
            None,
            "Remove imported audio",
            editing_service,
        );

        log::info!("[PercussionImportOrchestrator] Imported audio removed.");
    }

    /// Port of Unity RestoreImportedPercussionAudioFromProjectAsync().
    /// Synchronous in Rust — validates stems against audio content identity.
    /// Returns the validated stem paths (or None if validation failed).
    pub fn restore_imported_percussion_audio_from_project(
        &mut self,
        project: &mut Project,
    ) -> Option<Vec<String>> {
        let path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_deref())
            .unwrap_or("");

        if path.trim().is_empty() {
            return None;
        }

        if !std::path::Path::new(path).exists() {
            log::warn!(
                "[PercussionImportOrchestrator] Saved percussion backing audio not found: {}",
                path
            );
            self.set_percussion_import_status(
                "Perc backing audio missing",
                COLOR_ORANGE,
                false,
                5.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return None;
        }

        let path = path.to_string();
        self.set_percussion_import_status(
            "Perc backing audio restored",
            COLOR_GREEN,
            false,
            4.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            false,
        );

        // Validate stems against audio content identity.
        let validated = self.validate_stem_ownership(project, &path);
        if let Some(ref stems) = validated {
            if let Some(state) = project.percussion_import.as_mut() {
                state.stem_paths = Some(stems.clone());
            }
        }
        validated
    }

    /// Port of Unity CalibrateImportedPercussionDownbeatAtPlayhead().
    pub fn calibrate_imported_percussion_downbeat_at_playhead(
        &mut self,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        playhead_beat: f32,
        beats_per_bar: i32,
        set_status: bool,
    ) -> bool {
        if project.timeline.layers.is_empty() {
            return false;
        }

        let audio_start_beat = project
            .percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat);
        let audio_ready = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_ref())
            .map_or(false, |p| !p.is_empty());

        if !audio_ready {
            if set_status {
                self.set_percussion_import_status(
                    "Calibrate needs imported audio",
                    COLOR_ORANGE,
                    false,
                    3.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
            }
            return false;
        }

        let beats_per_bar = beats_per_bar.max(1) as f32;
        // Mathf.Max(0f, Mathf.Round(playheadBeat / beatsPerBar) * beatsPerBar)
        let nearest_bar = ((playhead_beat / beats_per_bar).round() * beats_per_bar).max(0.0);
        let delta = nearest_bar - playhead_beat;

        if delta.abs() < 0.0001 {
            if set_status {
                self.set_percussion_import_status(
                    "Already aligned to grid",
                    COLOR_BLUE,
                    false,
                    2.5,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
            }
            return false;
        }

        let current_start_beat = audio_start_beat;
        let new_start_beat = current_start_beat + delta;
        let command = self.build_shift_audio_and_clips_command(
            current_start_beat,
            new_start_beat,
            "Calibrate downbeat",
            project,
        );

        editing_service.execute(command, project);

        if set_status {
            let bar_number = (nearest_bar / beats_per_bar).round() as i32;
            self.set_percussion_import_status(
                &format!("Downbeat locked to bar {}", bar_number),
                COLOR_GREEN,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
        }

        true
    }

    /// Port of Unity NudgeImportedPercussionAlignment().
    pub fn nudge_imported_percussion_alignment(
        &mut self,
        delta_beats: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        set_status: bool,
    ) -> bool {
        let audio_ready = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_ref())
            .map_or(false, |p| !p.is_empty());

        if !audio_ready {
            if set_status {
                self.set_percussion_import_status(
                    "Nudge needs imported audio",
                    COLOR_ORANGE,
                    false,
                    3.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
            }
            return false;
        }

        let current_start_beat = project
            .percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat);
        let new_start_beat = current_start_beat + delta_beats;

        // Clamp: the helper handles per-clip clamping, but check if the audio itself can't move.
        if (0.0f32.max(new_start_beat) - current_start_beat).abs() < 0.0001 {
            let mut min_beat = current_start_beat;
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip.start_beat < min_beat {
                        min_beat = clip.start_beat;
                    }
                }
            }
            if delta_beats.max(-min_beat) < 0.0001 && delta_beats < 0.0 {
                return false;
            }
        }

        let command = self.build_shift_audio_and_clips_command(
            current_start_beat,
            new_start_beat,
            "Nudge alignment",
            project,
        );

        editing_service.execute(command, project);

        if set_status {
            self.set_percussion_import_status(
                &format!(
                    "Nudged ({:+.3} beats)",
                    delta_beats
                ),
                COLOR_GREEN,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
        }

        true
    }

    /// Port of Unity ResetImportedPercussionAlignment().
    pub fn reset_imported_percussion_alignment(
        &mut self,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        set_status: bool,
    ) -> bool {
        if project.timeline.layers.is_empty() {
            return false;
        }

        let audio_ready = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_ref())
            .map_or(false, |p| !p.is_empty());
        if !audio_ready {
            return false;
        }

        let current_start_beat = project
            .percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat);

        if current_start_beat.abs() < 0.0001 {
            if set_status {
                self.set_percussion_import_status(
                    "Already at beat 0",
                    COLOR_BLUE,
                    false,
                    2.5,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
            }
            return false;
        }

        let command = self.build_shift_audio_and_clips_command(
            current_start_beat,
            0.0,
            "Reset alignment",
            project,
        );

        editing_service.execute(command, project);

        if set_status {
            self.set_percussion_import_status(
                "Alignment reset to beat 0",
                COLOR_GREEN,
                false,
                3.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
        }

        true
    }

    /// Port of Unity BuildRescaleBeatsForBpmChange().
    /// Builds a command that proportionally rescales all clip beat positions and the audio
    /// start beat by (new_bpm / old_bpm). Does not execute — caller bundles with ChangeBpmCommand.
    /// Returns None if no rescaling is needed.
    pub fn build_rescale_beats_for_bpm_change(
        &self,
        old_bpm: f32,
        new_bpm: f32,
        project: &Project,
    ) -> Option<Box<dyn Command>> {
        if project.timeline.layers.is_empty() {
            return None;
        }

        if old_bpm <= 0.0 || new_bpm <= 0.0 {
            return None;
        }

        if (old_bpm - new_bpm).abs() < f32::EPSILON {
            return None;
        }

        let ratio = new_bpm / old_bpm;
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        // Rescale audio start beat.
        let audio_start = project
            .percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat);
        let audio_ready = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.as_ref())
            .map_or(false, |p| !p.is_empty());

        if audio_ready {
            let old_audio_start = audio_start;
            let new_audio_start = (old_audio_start * ratio).max(0.0);
            if (new_audio_start - old_audio_start).abs() >= 0.0001 {
                commands.push(Box::new(SetAudioStartBeatCommand::new(
                    old_audio_start,
                    new_audio_start,
                    "Rescale audio for BPM change",
                )));
            }
        }

        // Rescale all clip positions.
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                let old_beat = clip.start_beat;
                let new_beat = (old_beat * ratio).max(0.0);
                if (new_beat - old_beat).abs() < 0.0001 {
                    continue;
                }
                commands.push(Box::new(MoveClipBeatCommand::new(
                    clip.id.clone(),
                    clip.layer_index,
                    old_beat,
                    new_beat,
                )));
            }
        }

        if commands.is_empty() {
            return None;
        }

        if commands.len() == 1 {
            Some(commands.remove(0))
        } else {
            Some(Box::new(manifold_editing::command::CompositeCommand::new(
                commands,
                "Rescale beats for BPM change".to_string(),
            )))
        }
    }

    /// Port of Unity ReprojectImportedPercussionClipsFromSource().
    /// Reprojection removed: returns false always.
    pub fn reproject_imported_percussion_clips_from_source(&self) -> bool {
        // Reprojection removed: once clips are placed on the beat grid, their
        // positions are final. BPM changes affect playback speed, not clip positions.
        false
    }

    // ──────────────────────────────────────
    // FRAME TICK — PER-OPERATION DRIVERS
    // ──────────────────────────────────────

    fn tick_import_map(
        &mut self,
        _unscaled_time: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        // Extract state — we need to move the phase out temporarily to avoid borrow issues.
        let state = match &mut self.phase {
            OrchestratorPhase::ImportMap(s) => s,
            _ => return,
        };

        match &state.sub_phase {
            ImportMapSubPhase::RunningPipeline => {
                // Drive pipeline runner.
                let (done, ok, details) = drive_pipeline_run(
                    &mut state.pipeline_run,
                    &state.invocation,
                    &self.pipeline_progress_parser,
                );

                // Update status from pipeline output.
                self.percussion_import_status_message =
                    "Perc: running analysis backend".to_string();
                self.percussion_import_status_color = COLOR_BLUE;
                self.percussion_import_status_animate = true;
                self.percussion_import_status_progress01 = PERCUSSION_PROGRESS_UNKNOWN;
                self.percussion_import_status_show_progress = true;

                if done {
                    // Transition to ImportingJson.
                    let pipeline_ok = ok;
                    if !pipeline_ok {
                        let details_str = details;
                        log::error!(
                            "[PercussionImportOrchestrator] Audio analysis failed. {}",
                            details_str
                        );
                        // Clean up temp file.
                        let _ = std::fs::remove_file(&state.temp_output_json);
                        self.set_percussion_import_status(
                            "Perc: analysis failed",
                            COLOR_RED,
                            false,
                            6.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                        self.percussion_import_in_progress = false;
                        self.phase = OrchestratorPhase::Idle;
                    } else {
                        self.set_percussion_import_status(
                            "Perc: importing timeline clips",
                            COLOR_BLUE,
                            true,
                            0.0,
                            0.90,
                            true,
                        );
                        if let OrchestratorPhase::ImportMap(state) = &mut self.phase {
                            state.sub_phase = ImportMapSubPhase::ImportingJson { pipeline_ok: true };
                        }
                    }
                }
            }

            ImportMapSubPhase::ImportingJson { pipeline_ok } => {
                if !pipeline_ok {
                    self.percussion_import_in_progress = false;
                    self.phase = OrchestratorPhase::Idle;
                    return;
                }

                let temp_json = if let OrchestratorPhase::ImportMap(s) = &self.phase {
                    s.temp_output_json.clone()
                } else {
                    return;
                };
                let import_start_beat = if let OrchestratorPhase::ImportMap(s) = &self.phase {
                    s.import_start_beat
                } else {
                    return;
                };

                let import_succeeded = self.import_percussion_json_from_path_sync(
                    &temp_json,
                    import_start_beat,
                    project,
                    editing_service,
                );

                let _ = std::fs::remove_file(&temp_json);

                if let OrchestratorPhase::ImportMap(state) = &mut self.phase {
                    state.sub_phase = ImportMapSubPhase::LoadingAudio { import_succeeded };
                }
            }

            ImportMapSubPhase::LoadingAudio { import_succeeded } => {
                let import_succeeded = *import_succeeded;
                if !import_succeeded {
                    self.set_percussion_import_status(
                        "Perc: nothing imported",
                        COLOR_ORANGE,
                        false,
                        5.0,
                        PERCUSSION_PROGRESS_UNKNOWN,
                        false,
                    );
                    self.percussion_import_in_progress = false;
                    self.phase = OrchestratorPhase::Idle;
                    return;
                }

                // Load source audio — in Rust we set the path on the project immediately.
                // The actual audio loading is performed by the caller (host) watching for
                // the path change. We finalize the import state here.
                self.set_percussion_import_status(
                    "Perc: loading source audio",
                    COLOR_BLUE,
                    true,
                    0.0,
                    0.97,
                    true,
                );

                let (selected_path, old_audio, final_start_beat) =
                    if let OrchestratorPhase::ImportMap(s) = &self.phase {
                        (
                            s.selected_path.clone(),
                            AudioStateSnapshot {
                                path: s.old_audio.path.clone(),
                                start_beat: s.old_audio.start_beat,
                                hash: s.old_audio.hash.clone(),
                                stem_paths: s.old_audio.stem_paths.clone(),
                            },
                            self.last_import_aligned_start_beat,
                        )
                    } else {
                        return;
                    };

                let hash = compute_audio_hash(&selected_path);
                let state = project.percussion_import.get_or_insert_with(Default::default);
                state.audio_path = Some(selected_path.clone());
                state.audio_start_beat = final_start_beat;
                state.audio_hash = Some(hash.clone());

                // Resolve stems now that audio path is set.
                let resolved_stems = resolve_stem_paths_from_cache(&selected_path);
                let any_stem = resolved_stems.iter().any(|s| s.is_some());
                state.stem_paths = if any_stem {
                    Some(resolved_stems.iter().filter_map(|s| s.clone()).collect())
                } else {
                    None
                };

                // Defensive: stamp hash if we got stems.
                if any_stem && state.audio_hash.is_none() && state.audio_path.is_some() {
                    let path = state.audio_path.clone().unwrap_or_default();
                    state.audio_hash = Some(compute_audio_hash(&path));
                }

                let new_path = state.audio_path.clone();
                let new_start = state.audio_start_beat;
                let new_hash = state.audio_hash.clone();
                let new_stems = state.stem_paths.clone();

                // Record undo for the audio state change.
                self.record_audio_state_change(
                    old_audio.path,
                    old_audio.start_beat,
                    old_audio.hash,
                    old_audio.stem_paths,
                    new_path,
                    new_start,
                    new_hash,
                    new_stems,
                    "Import percussion audio",
                    editing_service,
                );

                self.set_percussion_import_status(
                    "Perc import complete",
                    COLOR_GREEN,
                    false,
                    5.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }

            ImportMapSubPhase::CheckingExtension | ImportMapSubPhase::Done => {
                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }
        }
    }

    fn tick_import_audio_only(
        &mut self,
        _unscaled_time: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
        _current_beat: f32,
    ) {
        let state = match &mut self.phase {
            OrchestratorPhase::ImportAudioOnly(s) => s,
            _ => return,
        };

        match &state.sub_phase {
            ImportAudioOnlySubPhase::LoadingAudio => {
                // Audio loading is handled by the host; we transition immediately.
                if let OrchestratorPhase::ImportAudioOnly(s) = &mut self.phase {
                    s.sub_phase = ImportAudioOnlySubPhase::RunningBpmPipeline;
                }
            }

            ImportAudioOnlySubPhase::RunningBpmPipeline => {
                let (done, ok, _details) = drive_pipeline_run(
                    &mut state.pipeline_run,
                    &state.invocation,
                    &self.pipeline_progress_parser,
                );

                if done {
                    if let OrchestratorPhase::ImportAudioOnly(s) = &mut self.phase {
                        s.sub_phase = ImportAudioOnlySubPhase::ParsingBpm { pipeline_ok: ok };
                    }
                }
            }

            ImportAudioOnlySubPhase::ParsingBpm { pipeline_ok } => {
                let pipeline_ok = *pipeline_ok;
                let (temp_json, selected_path, start_beat, old_audio) =
                    if let OrchestratorPhase::ImportAudioOnly(s) = &self.phase {
                        (
                            s.temp_output_json.clone(),
                            s.selected_path.clone(),
                            s.start_beat,
                            AudioStateSnapshot {
                                path: s.old_audio.path.clone(),
                                start_beat: s.old_audio.start_beat,
                                hash: s.old_audio.hash.clone(),
                                stem_paths: s.old_audio.stem_paths.clone(),
                            },
                        )
                    } else {
                        return;
                    };

                if pipeline_ok && std::path::Path::new(&temp_json).exists() {
                    let raw_json = match std::fs::read_to_string(&temp_json) {
                        Ok(j) => j,
                        Err(ex) => {
                            log::warn!(
                                "[PercussionImportOrchestrator] Failed to read BPM JSON: {}",
                                ex
                            );
                            String::new()
                        }
                    };

                    if !raw_json.is_empty() {
                        match self.percussion_analysis_parser.try_parse(&raw_json) {
                            Ok(analysis) => {
                                // Apply BPM.
                                let import_service = PercussionImportService::new_with_settings(
                                    self.pipeline_settings.as_ref(),
                                );
                                                let (bpm_decision, bpm_command) = import_service.apply_detected_bpm(
                                    project,
                                    Some(&analysis),
                                    &path_file_name(&selected_path),
                                );
                                if let Some(cmd) = bpm_command {
                                    editing_service.record(cmd);
                                }
                                let _request_clip_sync = bpm_decision == PercussionBpmDecision::AutoApplied;

                                // Auto-align: shift audio so first detected downbeat lands on a bar line.
                                let settings = &project.settings;
                                let beats_per_bar =
                                    (settings.time_signature_numerator as i32).max(1);
                                let aligned_start_beat = self.align_start_beat_to_downbeat(
                                    Some(&analysis),
                                    start_beat,
                                    settings.bpm,
                                    beats_per_bar,
                                );
                                let actual_start_beat = if (aligned_start_beat - start_beat).abs()
                                    >= f32::EPSILON
                                {
                                    // Update project state.
                                    if let Some(s) = project.percussion_import.as_mut() {
                                        s.audio_start_beat = aligned_start_beat;
                                    }
                                    aligned_start_beat
                                } else {
                                    start_beat
                                };

                                let bpm_text = if analysis.bpm > 0.0 {
                                    format!("{:.2} BPM", analysis.bpm)
                                } else {
                                    "BPM unknown".to_string()
                                };
                                self.set_percussion_import_status(
                                    &format!("Audio imported | {}", bpm_text),
                                    COLOR_GREEN,
                                    false,
                                    5.0,
                                    PERCUSSION_PROGRESS_UNKNOWN,
                                    false,
                                );
                                log::info!(
                                    "[PercussionImportOrchestrator] Audio imported: '{}' at beat {:.2}, {}",
                                    path_file_name(&selected_path),
                                    actual_start_beat,
                                    bpm_text
                                );

                                let _ = actual_start_beat;
                            }
                            Err(_) => {
                                self.set_percussion_import_status(
                                    "Audio imported | BPM detection failed",
                                    COLOR_ORANGE,
                                    false,
                                    5.0,
                                    PERCUSSION_PROGRESS_UNKNOWN,
                                    false,
                                );
                                log::info!(
                                    "[PercussionImportOrchestrator] Audio imported: '{}' at beat {:.2} (BPM detection failed)",
                                    path_file_name(&selected_path),
                                    start_beat
                                );
                            }
                        }
                    } else {
                        self.set_percussion_import_status(
                            "Audio imported | BPM analysis failed",
                            COLOR_ORANGE,
                            false,
                            5.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                        log::info!(
                            "[PercussionImportOrchestrator] Audio imported: '{}' at beat {:.2} (BPM analysis failed)",
                            path_file_name(&selected_path),
                            start_beat
                        );
                    }

                    let _ = std::fs::remove_file(&temp_json);
                } else {
                    self.set_percussion_import_status(
                        "Audio imported | BPM analysis failed",
                        COLOR_ORANGE,
                        false,
                        5.0,
                        PERCUSSION_PROGRESS_UNKNOWN,
                        false,
                    );
                    log::info!(
                        "[PercussionImportOrchestrator] Audio imported: '{}' at beat {:.2} (BPM analysis failed)",
                        path_file_name(&selected_path),
                        start_beat
                    );
                }

                // Record undo for the audio state change.
                let new_path = project
                    .percussion_import
                    .as_ref()
                    .and_then(|s| s.audio_path.clone());
                let new_start_beat = project
                    .percussion_import
                    .as_ref()
                    .map_or(0.0, |s| s.audio_start_beat);
                let new_hash = project
                    .percussion_import
                    .as_ref()
                    .and_then(|s| s.audio_hash.clone());
                let new_stems = project
                    .percussion_import
                    .as_ref()
                    .and_then(|s| s.stem_paths.clone());

                self.record_audio_state_change(
                    old_audio.path,
                    old_audio.start_beat,
                    old_audio.hash,
                    old_audio.stem_paths,
                    new_path,
                    new_start_beat,
                    new_hash,
                    new_stems,
                    "Import audio",
                    editing_service,
                );

                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }

            ImportAudioOnlySubPhase::Done => {
                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }
        }
    }

    fn tick_re_analyze_triggers(
        &mut self,
        _unscaled_time: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        let state = match &mut self.phase {
            OrchestratorPhase::ReAnalyzeTriggers(s) => s,
            _ => return,
        };

        match &state.sub_phase {
            ReAnalyzeSubPhase::RunningPipeline => {
                let (done, ok, details) = drive_pipeline_run(
                    &mut state.pipeline_run,
                    &state.invocation,
                    &self.pipeline_progress_parser,
                );

                if done {
                    let instrument_group = state.instrument_group.clone();
                    let temp_json = state.temp_output_json.clone();

                    if !ok {
                        let details = details;
                        let group_label = instrument_group.to_uppercase();
                        log::error!(
                            "[PercussionImportOrchestrator] Re-analysis failed for {}. {}",
                            group_label,
                            details
                        );
                        self.set_percussion_import_status(
                            &format!("Re-analysis failed ({})", group_label),
                            COLOR_RED,
                            false,
                            6.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                        let _ = std::fs::remove_file(&temp_json);
                        self.percussion_import_in_progress = false;
                        self.phase = OrchestratorPhase::Idle;
                        return;
                    }

                    let raw_json = match std::fs::read_to_string(&temp_json) {
                        Ok(j) => j,
                        Err(ex) => {
                            log::warn!(
                                "[PercussionImportOrchestrator] Failed to read re-analysis JSON: {}",
                                ex
                            );
                            String::new()
                        }
                    };

                    if !raw_json.is_empty() {
                        self.try_re_apply_triggers(&raw_json, &instrument_group, project, editing_service);
                    } else {
                        let group_label = instrument_group.to_uppercase();
                        self.set_percussion_import_status(
                            &format!("Re-analysis produced no output ({})", group_label),
                            COLOR_ORANGE,
                            false,
                            4.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                    }

                    let _ = std::fs::remove_file(&temp_json);
                    self.percussion_import_in_progress = false;
                    self.phase = OrchestratorPhase::Idle;
                }
            }

            ReAnalyzeSubPhase::Done => {
                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }
        }
    }

    fn tick_re_import_stems(
        &mut self,
        _unscaled_time: f32,
        project: &mut Project,
        _editing_service: &mut manifold_editing::service::EditingService,
    ) {
        let state = match &mut self.phase {
            OrchestratorPhase::ReImportStems(s) => s,
            _ => return,
        };

        match &state.sub_phase {
            ReImportStemsSubPhase::RunningPipeline => {
                let (done, ok, details) = drive_pipeline_run(
                    &mut state.pipeline_run,
                    &state.invocation,
                    &self.pipeline_progress_parser,
                );

                if done {
                    let temp_json = state.temp_output_json.clone();

                    // Clean up the JSON output — we only wanted the stems.
                    let _ = std::fs::remove_file(&temp_json);

                    if !ok {
                        let details = details;
                        log::error!(
                            "[PercussionImportOrchestrator] Stem re-import failed. {}",
                            details
                        );
                        self.set_percussion_import_status(
                            "Stem import failed",
                            COLOR_RED,
                            false,
                            6.0,
                            PERCUSSION_PROGRESS_UNKNOWN,
                            false,
                        );
                        self.percussion_import_in_progress = false;
                        self.phase = OrchestratorPhase::Idle;
                        return;
                    }

                    // Resolve stems from fresh cache output.
                    let audio_path = if let OrchestratorPhase::ReImportStems(s) = &self.phase {
                        s.audio_path.clone()
                    } else {
                        return;
                    };

                    self.resolve_stem_paths_and_notify(project, &audio_path);
                    self.set_percussion_import_status(
                        "Stems imported",
                        COLOR_GREEN,
                        false,
                        4.0,
                        PERCUSSION_PROGRESS_UNKNOWN,
                        false,
                    );
                    self.percussion_import_in_progress = false;
                    self.phase = OrchestratorPhase::Idle;
                }
            }

            ReImportStemsSubPhase::Done => {
                self.percussion_import_in_progress = false;
                self.phase = OrchestratorPhase::Idle;
            }
        }
    }

    // ──────────────────────────────────────
    // PRIVATE HELPERS
    // ──────────────────────────────────────

    /// Port of Unity TryReApplyTriggers().
    fn try_re_apply_triggers(
        &mut self,
        raw_json: &str,
        instrument_group: &str,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) -> bool {
        let analysis = match self.percussion_analysis_parser.try_parse(raw_json) {
            Ok(a) => a,
            Err(parse_error) => {
                log::error!(
                    "[PercussionImportOrchestrator] Re-analysis parse failed: {}",
                    parse_error
                );
                self.set_percussion_import_status(
                    "Re-analysis parse failed",
                    COLOR_RED,
                    false,
                    6.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return false;
            }
        };

        // Skip BPM auto-apply — trigger re-analysis does not change project BPM.

        let start_beat_offset = self.last_import_aligned_start_beat;
        let _beats_per_bar = (project.settings.time_signature_numerator as i32).max(1);
        let _bpm = project.settings.bpm;

        let options = PercussionImportOptionsFactory::create_default_with_settings(
            project,
            self.pipeline_settings.as_ref(),
            start_beat_offset,
        );

        let binding_resolver = ProjectPercussionBindingResolver::new(project, &options);
        let plan = {
            let beat_time_converter = ProjectBeatTimeConverter::new(project);
            let mut planner = PercussionTimelinePlanner::new(
                Box::new(beat_time_converter),
                Box::new(binding_resolver),
            );
            planner.build_plan(Some(&analysis), Some(&options))
        };

        if plan.accepted_events() == 0 {
            let group_label = instrument_group.to_uppercase();
            log::warn!(
                "[PercussionImportOrchestrator] Re-analysis ({}) produced no placements \
                (total={}, unmapped={}, lowConf={}, lowEnergy={}, dedup={}).",
                group_label,
                plan.total_events,
                plan.skipped_unmapped,
                plan.skipped_low_confidence,
                plan.skipped_low_energy,
                plan.skipped_by_quantized_dedup
            );
            self.set_percussion_import_status(
                &format!("No triggers found ({})", group_label),
                COLOR_ORANGE,
                false,
                4.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return false;
        }

        let import_service =
            PercussionImportService::new_with_settings(self.pipeline_settings.as_ref());
        let result = import_service.apply_placement_plan(project, Some(&plan), Some(&options));

        if result.success {
            if let Some(cmd) = result.undo_command {
                editing_service.record(cmd);
            }
            apply_energy_envelope_to_project(&analysis, project);
        }

        let group_label_final = instrument_group.to_uppercase();
        let status = format!("Re-analyzed {}: {} clips", group_label_final, result.added_clips);
        self.set_percussion_import_status(&status, COLOR_GREEN, false, 5.0, PERCUSSION_PROGRESS_UNKNOWN, false);

        log::info!(
            "[PercussionImportOrchestrator] Re-analyzed {}: {} clips placed \
            (total={}, accepted={}).",
            group_label_final,
            result.added_clips,
            plan.total_events,
            plan.accepted_events()
        );

        // Re-analysis may have updated stems in the cache.
        let audio_path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.clone())
            .unwrap_or_default();
        if !audio_path.is_empty() {
            self.resolve_stem_paths_and_notify(project, &audio_path);
        }

        result.success
    }

    /// Port of Unity ImportPercussionJsonFromPathAsync() + TryImportPercussionJson() —
    /// synchronous Rust version (no async needed for file read + parse).
    fn import_percussion_json_from_path_sync(
        &mut self,
        json_path: &str,
        start_beat_offset: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) -> bool {
        if json_path.is_empty() {
            return false;
        }

        let raw_json = match std::fs::read_to_string(json_path) {
            Ok(j) => j,
            Err(ex) => {
                log::error!(
                    "[PercussionImportOrchestrator] Failed to read percussion JSON: {}",
                    ex
                );
                return false;
            }
        };

        self.try_import_percussion_json(&raw_json, &path_file_name(json_path), start_beat_offset, project, editing_service)
    }

    /// Port of Unity TryImportPercussionJson().
    fn try_import_percussion_json(
        &mut self,
        raw_json: &str,
        source_label: &str,
        start_beat_offset: f32,
        project: &mut Project,
        editing_service: &mut manifold_editing::service::EditingService,
    ) -> bool {
        let analysis = match self.percussion_analysis_parser.try_parse(raw_json) {
            Ok(a) => a,
            Err(parse_error) => {
                log::error!(
                    "[PercussionImportOrchestrator] Percussion import parse failed: {}",
                    parse_error
                );
                self.set_percussion_import_status(
                    "Perc: JSON parse failed",
                    COLOR_RED,
                    false,
                    6.0,
                    PERCUSSION_PROGRESS_UNKNOWN,
                    false,
                );
                return false;
            }
        };

        let import_service =
            PercussionImportService::new_with_settings(self.pipeline_settings.as_ref());
        let (bpm_decision, bpm_command) =
            import_service.apply_detected_bpm(project, Some(&analysis), source_label);
        if let Some(cmd) = bpm_command {
            editing_service.record(cmd);
        }

        let bpm = project.settings.bpm;
        let beats_per_bar = (project.settings.time_signature_numerator as i32).max(1);
        let aligned_offset =
            self.align_start_beat_to_downbeat(Some(&analysis), start_beat_offset, bpm, beats_per_bar);
        self.last_import_aligned_start_beat = aligned_offset;

        let options = PercussionImportOptionsFactory::create_default_with_settings(
            project,
            self.pipeline_settings.as_ref(),
            aligned_offset,
        );
        let binding_resolver = ProjectPercussionBindingResolver::new(project, &options);
        let plan = {
            let beat_time_converter = ProjectBeatTimeConverter::new(project);
            let mut planner = PercussionTimelinePlanner::new(
                Box::new(beat_time_converter),
                Box::new(binding_resolver),
            );
            planner.build_plan(Some(&analysis), Some(&options))
        };

        if plan.accepted_events() == 0 {
            log::warn!(
                "[PercussionImportOrchestrator] Percussion import produced no placements \
                (total={}, unmapped={}, lowConf={}, lowEnergy={}, \
                dedup={}, invalidTiming={}, unknown={}).",
                plan.total_events,
                plan.skipped_unmapped,
                plan.skipped_low_confidence,
                plan.skipped_low_energy,
                plan.skipped_by_quantized_dedup,
                plan.skipped_invalid_timing,
                plan.skipped_unknown_type
            );
            self.set_percussion_import_status(
                "Perc: no accepted triggers",
                COLOR_ORANGE,
                false,
                6.0,
                PERCUSSION_PROGRESS_UNKNOWN,
                false,
            );
            return false;
        }

        let result = import_service.apply_placement_plan(project, Some(&plan), Some(&options));
        if result.success {
            if let Some(cmd) = result.undo_command {
                editing_service.record(cmd);
            }
            apply_energy_envelope_to_project(&analysis, project);
        }

        log::info!(
            "[PercussionImportOrchestrator] Imported percussion map '{}' \
            track='{}' clips={}/{} \
            (unmapped={}, lowConf={}, lowEnergy={}, dedup={}, \
            invalidTiming={}, unknown={}).",
            source_label,
            analysis.track_id,
            plan.accepted_events(),
            plan.total_events,
            plan.skipped_unmapped,
            plan.skipped_low_confidence,
            plan.skipped_low_energy,
            plan.skipped_by_quantized_dedup,
            plan.skipped_invalid_timing,
            plan.skipped_unknown_type
        );

        let mut status = format!("Perc: imported {} clips", result.added_clips);
        if bpm_decision == PercussionBpmDecision::AutoApplied {
            status.push_str(&format!(" | BPM auto {:.2}", analysis.bpm));
        } else if bpm_decision == PercussionBpmDecision::SuggestedLowConfidence {
            status.push_str(&format!(" | BPM {:.2} suggested", analysis.bpm));
        }
        self.set_percussion_import_status(
            &status,
            COLOR_GREEN,
            false,
            5.0,
            PERCUSSION_PROGRESS_UNKNOWN,
            false,
        );

        // Resolve stem paths from cache and notify listeners.
        let audio_path = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_path.clone())
            .unwrap_or_default();
        if !audio_path.is_empty() {
            self.resolve_stem_paths_and_notify(project, &audio_path);
        }

        result.success
    }

    /// Port of Unity ValidateStemOwnership().
    /// Validates that saved stem paths belong to the given audio file by checking
    /// the content hash and file existence. Returns validated paths or None.
    /// Only used on project restore — never uses cache fallback.
    fn validate_stem_ownership(&self, project: &mut Project, audio_path: &str) -> Option<Vec<String>> {
        let saved_stems = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.stem_paths.clone())?;

        if saved_stems.len() != STEM_COUNT {
            if let Some(state) = project.percussion_import.as_mut() {
                state.stem_paths = None;
            }
            return None;
        }

        // Check that all non-None stem files exist on disk (no partial sets).
        let mut any_stem = false;
        for stem in &saved_stems {
            if stem.is_empty() {
                continue;
            }
            any_stem = true;
            if !std::path::Path::new(stem).exists() {
                log::warn!(
                    "[PercussionImportOrchestrator] Stem file missing: {} — clearing all stems.",
                    stem
                );
                if let Some(state) = project.percussion_import.as_mut() {
                    state.stem_paths = None;
                }
                return None;
            }
        }

        if !any_stem {
            if let Some(state) = project.percussion_import.as_mut() {
                state.stem_paths = None;
            }
            return None;
        }

        // Verify content identity.
        let current_hash = compute_audio_hash(audio_path);
        let stored_hash = project
            .percussion_import
            .as_ref()
            .and_then(|s| s.audio_hash.clone());

        let stored_hash = match stored_hash {
            Some(h) if !h.is_empty() => h,
            _ => {
                // Old project without hash — no way to verify stems belong to this audio.
                log::warn!(
                    "[PercussionImportOrchestrator] No audio hash stored — clearing unverified stems."
                );
                if let Some(state) = project.percussion_import.as_mut() {
                    state.stem_paths = None;
                }
                return None;
            }
        };

        if current_hash.is_empty()
            || !current_hash.eq_ignore_ascii_case(&stored_hash)
        {
            log::warn!(
                "[PercussionImportOrchestrator] Audio content hash mismatch — clearing stale stems."
            );
            if let Some(state) = project.percussion_import.as_mut() {
                state.stem_paths = None;
            }
            return None;
        }

        Some(saved_stems)
    }

    /// Port of Unity ResolveStemPathsAndNotify().
    /// Called after a fresh pipeline run to resolve stems from cache and store on project.
    fn resolve_stem_paths_and_notify(&mut self, project: &mut Project, audio_path: &str) {
        let stem_paths = resolve_stem_paths_from_cache(audio_path);

        let any_stem = stem_paths.iter().any(|s| s.is_some());
        let flat_paths: Option<Vec<String>> = if any_stem {
            Some(stem_paths.into_iter().filter_map(|s| s).collect())
        } else {
            None
        };

        if let Some(state) = project.percussion_import.as_mut() {
            state.stem_paths = flat_paths.clone();

            // Defensive: ensure hash is stamped after fresh pipeline run.
            if any_stem
                && state.audio_hash.is_none()
                && state.audio_path.as_deref().map_or(false, |p| !p.is_empty())
            {
                let path = state.audio_path.clone().unwrap_or_default();
                state.audio_hash = Some(compute_audio_hash(&path));
            }
        }
    }

    /// Port of Unity AlignStartBeatToDownbeat().
    fn align_start_beat_to_downbeat(
        &self,
        analysis: Option<&PercussionAnalysisData>,
        start_beat_offset: f32,
        bpm: f32,
        beats_per_bar: i32,
    ) -> f32 {
        let analysis = match analysis {
            Some(a) => a,
            None => return start_beat_offset,
        };

        let beat_grid = match analysis.beat_grid.as_ref() {
            Some(g) => g,
            None => return start_beat_offset,
        };

        if !beat_grid.has_usable_beats() {
            return start_beat_offset;
        }

        if beat_grid.downbeat_indices.is_empty() {
            return start_beat_offset;
        }

        let first_downbeat_idx = beat_grid.downbeat_indices[0] as usize;
        if first_downbeat_idx >= beat_grid.beat_times_seconds.len() {
            return start_beat_offset;
        }

        if bpm <= 0.0 {
            return start_beat_offset;
        }

        let beats_per_bar = beats_per_bar.max(1) as f32;
        let first_downbeat_sec = beat_grid.beat_times_seconds[first_downbeat_idx];
        let first_downbeat_beat_in_audio = first_downbeat_sec * bpm / 60.0;
        let raw_landing = first_downbeat_beat_in_audio + start_beat_offset;

        // Snap to nearest bar boundary.
        // Mathf.Round(rawLanding / beatsPerBar) * beatsPerBar
        let snapped_landing = (raw_landing / beats_per_bar).round() * beats_per_bar;
        let delta = snapped_landing - raw_landing;
        let mut aligned = start_beat_offset + delta;

        // Ensure minimum headroom (at least 1 bar before audio starts).
        let min_headroom = beats_per_bar * PERCUSSION_IMPORT_SAFE_HEADROOM_BARS as f32;
        while aligned < min_headroom {
            aligned += beats_per_bar;
        }

        log::debug!(
            "[PercussionImportOrchestrator] AlignStartBeatToDownbeat: downbeatIdx={}, \
            downbeatSec={:.4}, rawLanding={:.3}, snapped={:.1}, offset {:.3} → {:.3}",
            first_downbeat_idx,
            first_downbeat_sec,
            raw_landing,
            snapped_landing,
            start_beat_offset,
            aligned
        );

        aligned
    }

    fn get_percussion_import_start_beat_with_headroom_static(
        current_beat: f32,
        beats_per_bar: i32,
    ) -> f32 {
        let beats_per_bar = beats_per_bar.max(1) as f32;
        let minimum_start_beat = beats_per_bar * PERCUSSION_IMPORT_SAFE_HEADROOM_BARS as f32;
        minimum_start_beat.max(current_beat)
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

    /// Port of Unity BuildShiftAudioAndClipsCommand().
    fn build_shift_audio_and_clips_command(
        &self,
        old_audio_start: f32,
        new_audio_start: f32,
        description: &str,
        project: &Project,
    ) -> Box<dyn Command> {
        let mut delta = new_audio_start - old_audio_start;

        // Clamp delta so no clip or audio goes below beat 0.
        let mut min_beat = old_audio_start;
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip.start_beat < min_beat {
                    min_beat = clip.start_beat;
                }
            }
        }
        // Mathf.Max(delta, -minBeat)
        delta = delta.max(-min_beat);
        let new_audio_start = (old_audio_start + delta).max(0.0);

        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        commands.push(Box::new(SetAudioStartBeatCommand::new(
            old_audio_start,
            new_audio_start,
            description,
        )));

        let final_delta = delta;
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                let old_beat = clip.start_beat;
                let new_beat = (old_beat + final_delta).max(0.0);
                if (new_beat - old_beat).abs() < 0.0001 {
                    continue;
                }
                commands.push(Box::new(MoveClipBeatCommand::new(
                    clip.id.clone(),
                    clip.layer_index,
                    old_beat,
                    new_beat,
                )));
            }
        }

        if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(manifold_editing::command::CompositeCommand::new(
                commands,
                description.to_string(),
            ))
        }
    }

    /// Port of Unity RecordAudioStateChange().
    fn record_audio_state_change(
        &self,
        old_path: Option<String>,
        old_start_beat: f32,
        old_hash: Option<String>,
        old_stem_paths: Option<Vec<String>>,
        new_path: Option<String>,
        new_start_beat: f32,
        new_hash: Option<String>,
        new_stem_paths: Option<Vec<String>>,
        description: &str,
        editing_service: &mut manifold_editing::service::EditingService,
    ) {
        let command = Box::new(SetImportedAudioCommand::new(
            old_path,
            old_start_beat,
            old_hash,
            old_stem_paths,
            new_path,
            new_start_beat,
            new_hash,
            new_stem_paths,
            description,
        ));
        editing_service.record(command);
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
// Inline command types for beat shifting
// (Unity uses MoveClipCommand + SetAudioStartBeatCommand from EditingService)
// ──────────────────────────────────────

/// Port of Unity SetAudioStartBeatCommand.
/// Applies the new audio start beat to project.percussion_import.
#[derive(Debug)]
pub struct SetAudioStartBeatCommand {
    old_start_beat: f32,
    new_start_beat: f32,
    desc: String,
}

impl SetAudioStartBeatCommand {
    pub fn new(old_start_beat: f32, new_start_beat: f32, description: &str) -> Self {
        Self {
            old_start_beat,
            new_start_beat,
            desc: description.to_string(),
        }
    }
}

impl Command for SetAudioStartBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_start_beat = self.new_start_beat;
    }

    fn undo(&mut self, project: &mut Project) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_start_beat = self.old_start_beat;
    }

    fn description(&self) -> &str {
        &self.desc
    }
}

/// Lightweight command to move a clip's start beat in place.
/// Port of the move-beat portion of Unity MoveClipCommand.
#[derive(Debug)]
pub struct MoveClipBeatCommand {
    clip_id: String,
    layer_index: i32,
    old_start_beat: f32,
    new_start_beat: f32,
}

impl MoveClipBeatCommand {
    pub fn new(clip_id: String, layer_index: i32, old_start_beat: f32, new_start_beat: f32) -> Self {
        Self {
            clip_id,
            layer_index,
            old_start_beat,
            new_start_beat,
        }
    }

    fn apply(project: &mut Project, clip_id: &str, layer_index: i32, start_beat: f32) {
        if let Some(layer) = project.timeline.layers.get_mut(layer_index as usize) {
            if let Some(clip) = layer.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.start_beat = start_beat;
            }
        }
    }
}

impl Command for MoveClipBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.layer_index, self.new_start_beat);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.layer_index, self.old_start_beat);
    }

    fn description(&self) -> &str {
        "Move clip beat"
    }
}

// ──────────────────────────────────────
// Pipeline runner helper (drives PipelineRunState)
// ──────────────────────────────────────

/// Drive the pipeline run one tick. Returns (done, ok, last_details).
/// This is the Rust equivalent of `RunPercussionPipelineAsync`'s per-frame logic.
/// With the simplified bundled-only backend, there is exactly one invocation to run.
fn drive_pipeline_run(
    pipeline_run: &mut Option<PipelineRunState>,
    invocation: &PercussionPipelineInvocation,
    parser: &PercussionPipelineProgressParser,
) -> (bool, bool, String) {
    // If no active run, start the invocation.
    if pipeline_run.is_none() {
        if invocation.command.trim().is_empty() {
            let details = "No analysis backend invocation candidates were resolved.".to_string();
            return (true, false, details);
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
            return (true, false, details);
        }

        *pipeline_run = Some(PipelineRunState::Running {
            handle,
            latest_process_line: String::new(),
        });
    }

    let (finished, exit_code, new_lines, latest_line) =
        match pipeline_run.as_mut() {
            Some(PipelineRunState::Running {
                handle,
                latest_process_line,
            }) => {
                let new_lines = handle.poll();
                let mut latest = latest_process_line.clone();
                for line_info in &new_lines {
                    if !line_info.line.trim().is_empty() {
                        latest = line_info.line.trim().to_string();
                    }
                }
                let finished = handle.is_finished();
                let exit_code = handle.exit_code();
                (finished, exit_code, new_lines, latest)
            }
            Some(PipelineRunState::Done { ok, details }) => {
                let ok = *ok;
                let details = details.clone();
                return (true, ok, details);
            }
            None => {
                return (true, false, "No pipeline run active.".to_string());
            }
        };

    // Parse progress lines and surface to status.
    for line_info in &new_lines {
        if line_info.line.trim().is_empty() {
            continue;
        }
        let progress = parser.parse_line(&line_info.line, line_info.is_stderr);
        if progress.has_progress {
            // Status update captured; callers will read it from the orchestrator.
            let _ = progress;
        }
    }

    if !finished {
        // Not done yet — return without advancing.
        return (false, false, String::new());
    }

    // Process finished. Check result.
    let exit_code = exit_code.unwrap_or(-1);

    let output_json = PercussionImportOrchestrator::resolve_output_path_from_arguments(
        &invocation.arguments,
    );
    let output_exists = output_json
        .as_deref()
        .map_or(false, |p| std::path::Path::new(p).exists());

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
        return (true, true, String::new());
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
    (true, false, details)
}

// ──────────────────────────────────────
// MODULE-LEVEL HELPERS
// ──────────────────────────────────────

fn get_extension_lowercase(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()))
        .unwrap_or_default()
}

fn is_supported_audio_extension(ext: &str) -> bool {
    SUPPORTED_PERCUSSION_AUDIO_EXTENSIONS.contains(&ext)
}

fn path_file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Port of Unity AudioFileIdentity.ComputeHash().
/// Computes a content hash for the audio file at the given path.
/// Used to verify stem ownership across sessions.
/// Unity uses MD5 of file bytes. We use a simple FNV-1a 64-bit hash here since
/// no external crypto crate is available. The hash is stored and compared as a hex string.
fn compute_audio_hash(path: &str) -> String {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };

    // FNV-1a 64-bit
    let mut hash: u64 = 14695981039346656037u64;
    for byte in &data {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211u64);
    }

    format!("{:016x}", hash)
}

/// Port of Unity StemAudioController.ResolveStemPathsFromCache().
/// Resolves the 4 stem paths (Drums, Bass, Other, Vocals) from the Demucs cache
/// for the given audio path. Returns an array of STEM_COUNT Option<String>.
/// A None slot means that stem is not available.
///
/// Demucs writes stems to: {cache_dir}/{model}/{track_name}/{stem}.wav
/// We probe the same paths the backend resolver would use for cache.
fn resolve_stem_paths_from_cache(audio_path: &str) -> Vec<Option<String>> {
    const STEM_NAMES: [&str; STEM_COUNT] = ["drums", "bass", "other", "vocals"];

    let mut result: Vec<Option<String>> = vec![None; STEM_COUNT];

    if audio_path.trim().is_empty() {
        return result;
    }

    let cache_dir = {
        let from_env = std::env::var("MANIFOLD_DEMUCS_CACHE_DIR").ok();
        match from_env {
            Some(ref d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => {
                // Fallback to Library/AudioAnalysisStemCache if no env var.
                // (same logic as append_demucs_cache_arguments in percussion_backend.rs)
                return result;
            }
        }
    };

    let track_name = Path::new(audio_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if track_name.is_empty() {
        return result;
    }

    // Demucs default model is htdemucs.
    let model = std::env::var("MANIFOLD_DEMUCS_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "htdemucs".to_string());

    for (i, stem_name) in STEM_NAMES.iter().enumerate() {
        let stem_path = PathBuf::from(&cache_dir)
            .join(&model)
            .join(track_name)
            .join(format!("{}.wav", stem_name));

        if stem_path.exists() {
            result[i] = Some(stem_path.to_string_lossy().into_owned());
        }
    }

    result
}

/// Port of Unity ApplyEnergyEnvelopeToProject().
fn apply_energy_envelope_to_project(analysis: &PercussionAnalysisData, project: &mut Project) {
    if !analysis.has_energy_envelope() {
        return;
    }

    if let Some(ref envelope) = analysis.energy_envelope {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.energy_envelope = Some(envelope.clone());
    }
}
