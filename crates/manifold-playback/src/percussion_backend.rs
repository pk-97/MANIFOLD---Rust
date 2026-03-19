// Port of Unity PercussionPipelineBackendResolver.cs (1176 lines).
// Resolves Python/ffmpeg/demucs binaries and builds CLI invocations for the
// percussion analysis pipeline.
//
// Unity `#if UNITY_EDITOR` blocks set preferProjectPython = true. The Rust port
// is always the runtime (player) build, so prefer_project_python = false always.
// The isEditor flag in AppendDemucsCacheArguments is similarly always false here.

use std::collections::HashSet;
use std::path::Path;

use manifold_core::percussion_settings::{PercussionPipelineSettings, StemMode};

// ─── PercussionPipelineBackendType ───

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercussionPipelineBackendType {
    BundledRuntime,
    ProjectPython,
}

impl std::fmt::Display for PercussionPipelineBackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PercussionPipelineBackendType::BundledRuntime => write!(f, "BundledRuntime"),
            PercussionPipelineBackendType::ProjectPython => write!(f, "ProjectPython"),
        }
    }
}

// ─── PercussionPipelineInvocation ───

#[derive(Debug, Clone)]
pub struct PercussionPipelineInvocation {
    pub backend_type: PercussionPipelineBackendType,
    pub backend_label: String,
    pub command: String,
    pub arguments: Vec<String>,
}

impl PercussionPipelineInvocation {
    pub fn new(
        backend_type: PercussionPipelineBackendType,
        backend_label: &str,
        command: &str,
        arguments: Vec<String>,
    ) -> Self {
        let backend_label = if backend_label.trim().is_empty() {
            backend_type.to_string()
        } else {
            backend_label.to_string()
        };
        let command = command.to_string();
        PercussionPipelineInvocation {
            backend_type,
            backend_label,
            command,
            arguments,
        }
    }
}

// Module-level constants (private in Unity, module-private here).
const SCRIPT_FILE_NAME: &str = "percussion_json_pipeline.py";
const SHIMS_FILE_NAME: &str = "lameenc.py";
const DEFAULT_DEMUCS_MODEL: &str = "htdemucs";
const DEFAULT_DEMUCS_SHIFTS: &str = "1";
const DEFAULT_DEMUCS_OVERLAP: &str = "0.25";
const DEFAULT_DEMUCS_JOBS: &str = "0";

// ─── PercussionPipelineBackendResolver ───

pub struct PercussionPipelineBackendResolver;

impl PercussionPipelineBackendResolver {
    pub const BUNDLED_RUNTIME_FOLDER_NAME: &'static str = "AudioAnalysisRuntime";

    pub fn build_default_import_invocations_no_settings(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
    ) -> Vec<PercussionPipelineInvocation> {
        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return Vec::new();
        }

        let mut invocations: Vec<PercussionPipelineInvocation> = Vec::with_capacity(8);
        let mut dedupe: HashSet<String> = HashSet::new();

        let bundled_runtime_root = Self::resolve_bundled_runtime_root(application_data_path);
        let project_root = Self::resolve_project_root_path(application_data_path);

        let ffmpeg_bin = Self::resolve_ffmpeg_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );
        let demucs_bin = Self::resolve_demucs_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );

        // Runtime build: prefer_project_python = false
        let prefer_project_python = false;

        if prefer_project_python {
            Self::append_project_python_no_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
            );
            Self::append_bundled_runtime_no_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
            );
        } else {
            Self::append_bundled_runtime_no_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
            );
            Self::append_project_python_no_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
            );
        }

        invocations
    }

    pub fn build_default_import_invocations(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        settings: Option<&PercussionPipelineSettings>,
    ) -> Vec<PercussionPipelineInvocation> {
        let Some(settings) = settings else {
            return Self::build_default_import_invocations_no_settings(
                application_data_path,
                input_audio_path,
                output_json_path,
            );
        };

        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return Vec::new();
        }

        let mut invocations: Vec<PercussionPipelineInvocation> = Vec::with_capacity(8);
        let mut dedupe: HashSet<String> = HashSet::new();

        let bundled_runtime_root = Self::resolve_bundled_runtime_root(application_data_path);
        let project_root = Self::resolve_project_root_path(application_data_path);

        let ffmpeg_bin = Self::resolve_ffmpeg_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );
        let demucs_bin = Self::resolve_demucs_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );

        // Runtime build: prefer_project_python = false
        let prefer_project_python = false;

        if prefer_project_python {
            Self::append_project_python_with_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
                project_root.as_deref(),
                settings,
                None,
            );
            Self::append_bundled_runtime_with_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                settings,
                None,
            );
        } else {
            Self::append_bundled_runtime_with_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                settings,
                None,
            );
            Self::append_project_python_with_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
                project_root.as_deref(),
                settings,
                None,
            );
        }

        invocations
    }

    pub fn build_trigger_re_analysis_invocations(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        settings: Option<&PercussionPipelineSettings>,
        instruments: Option<&str>,
    ) -> Vec<PercussionPipelineInvocation> {
        let Some(settings) = settings else {
            return Self::build_default_import_invocations_no_settings(
                application_data_path,
                input_audio_path,
                output_json_path,
            );
        };

        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return Vec::new();
        }

        let mut invocations: Vec<PercussionPipelineInvocation> = Vec::with_capacity(8);
        let mut dedupe: HashSet<String> = HashSet::new();

        let bundled_runtime_root = Self::resolve_bundled_runtime_root(application_data_path);
        let project_root = Self::resolve_project_root_path(application_data_path);

        let ffmpeg_bin = Self::resolve_ffmpeg_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );
        let demucs_bin = Self::resolve_demucs_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );

        // Runtime build: prefer_project_python = false
        let prefer_project_python = false;

        if prefer_project_python {
            Self::append_project_python_with_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
                project_root.as_deref(),
                settings,
                instruments,
            );
            Self::append_bundled_runtime_with_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                settings,
                instruments,
            );
        } else {
            Self::append_bundled_runtime_with_settings(
                &mut invocations,
                &mut dedupe,
                bundled_runtime_root.as_deref(),
                project_root.as_deref(),
                prefer_project_python,
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                settings,
                instruments,
            );
            Self::append_project_python_with_settings(
                &mut invocations,
                &mut dedupe,
                project_root.as_deref(),
                input_audio_path,
                output_json_path,
                ffmpeg_bin.as_deref(),
                demucs_bin.as_deref(),
                project_root.as_deref(),
                settings,
                instruments,
            );
        }

        invocations
    }

    pub fn build_bpm_only_invocations(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        settings: Option<&PercussionPipelineSettings>,
    ) -> Vec<PercussionPipelineInvocation> {
        let Some(settings) = settings else {
            return Self::build_bpm_only_invocations_no_settings(
                application_data_path,
                input_audio_path,
                output_json_path,
            );
        };

        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return Vec::new();
        }

        let mut invocations: Vec<PercussionPipelineInvocation> = Vec::with_capacity(4);
        let mut dedupe: HashSet<String> = HashSet::new();

        let bundled_runtime_root = Self::resolve_bundled_runtime_root(application_data_path);
        let project_root = Self::resolve_project_root_path(application_data_path);
        let ffmpeg_bin = Self::resolve_ffmpeg_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );

        let track_id = {
            let stem = Path::new(input_audio_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if stem.trim().is_empty() {
                "ImportedTrack".to_string()
            } else {
                stem.to_string()
            }
        };

        // Runtime build: prefer_project_python = false
        let prefer_project_python = false;

        let append_bpm = |invocations: &mut Vec<PercussionPipelineInvocation>,
                          dedupe: &mut HashSet<String>,
                          backend_type: PercussionPipelineBackendType,
                          backend_label: &str,
                          script_path: &str,
                          python_commands: &[String]| {
            if script_path.trim().is_empty() || !Path::new(script_path).exists() {
                return;
            }
            if python_commands.is_empty() {
                return;
            }

            let mut args: Vec<String> = Vec::with_capacity(20);
            args.push(script_path.to_string());
            args.push(input_audio_path.to_string());
            args.push("-o".to_string());
            args.push(output_json_path.to_string());
            args.push("--track-id".to_string());
            args.push(track_id.clone());
            args.push("--bpm-only".to_string());
            args.push("on".to_string());
            args.push("--use-drum-stem".to_string());
            args.push("off".to_string());
            args.push("--emit-bass".to_string());
            args.push("off".to_string());
            args.push("--use-vocal-stem".to_string());
            args.push("off".to_string());
            args.push("--min-bpm".to_string());
            args.push(format!("{:.1}", settings.global.min_bpm));
            args.push("--max-bpm".to_string());
            args.push(format!("{:.1}", settings.global.max_bpm));

            if let Some(ref bin) = ffmpeg_bin {
                if !bin.trim().is_empty() {
                    args.push("--ffmpeg-bin".to_string());
                    args.push(bin.clone());
                }
            }

            for python_cmd in python_commands {
                if python_cmd.trim().is_empty() {
                    continue;
                }
                let key = format!(
                    "{}|{}|{}",
                    backend_label.to_lowercase(),
                    python_cmd.to_lowercase(),
                    script_path.to_lowercase()
                );
                if !dedupe.insert(key) {
                    continue;
                }
                invocations.push(PercussionPipelineInvocation::new(
                    backend_type,
                    backend_label,
                    python_cmd,
                    args.clone(),
                ));
            }
        };

        if prefer_project_python {
            if let Some(ref root) = project_root {
                let project_script = Path::new(root)
                    .join("Tools")
                    .join("AudioAnalysis")
                    .join(SCRIPT_FILE_NAME)
                    .to_string_lossy()
                    .into_owned();
                let project_python =
                    Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::ProjectPython,
                    "project-python-bpm",
                    &project_script,
                    &project_python,
                );
            }
            if let Some(ref brt) = bundled_runtime_root {
                let bundled_script = Self::resolve_bundled_backend_script_path(
                    brt,
                    project_root.as_deref(),
                    /*prefer_project_script_in_editor=*/ true,
                );
                let all_bundled_python =
                    Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
                let bundled_python: Vec<String> = if !all_bundled_python.is_empty() {
                    vec![all_bundled_python[0].clone()]
                } else {
                    Vec::new()
                };
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::BundledRuntime,
                    "bundled-runtime-bpm",
                    &bundled_script,
                    &bundled_python,
                );
            }
        } else {
            if let Some(ref brt) = bundled_runtime_root {
                let bundled_script = Self::resolve_bundled_backend_script_path(
                    brt,
                    project_root.as_deref(),
                    /*prefer_project_script_in_editor=*/ false,
                );
                let all_bundled_python =
                    Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
                let bundled_python: Vec<String> = if !all_bundled_python.is_empty() {
                    vec![all_bundled_python[0].clone()]
                } else {
                    Vec::new()
                };
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::BundledRuntime,
                    "bundled-runtime-bpm",
                    &bundled_script,
                    &bundled_python,
                );
            }
            if let Some(ref root) = project_root {
                let project_script = Path::new(root)
                    .join("Tools")
                    .join("AudioAnalysis")
                    .join(SCRIPT_FILE_NAME)
                    .to_string_lossy()
                    .into_owned();
                let project_python =
                    Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::ProjectPython,
                    "project-python-bpm",
                    &project_script,
                    &project_python,
                );
            }
        }

        invocations
    }

    pub fn build_bpm_only_invocations_no_settings(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
    ) -> Vec<PercussionPipelineInvocation> {
        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return Vec::new();
        }

        let mut invocations: Vec<PercussionPipelineInvocation> = Vec::with_capacity(4);
        let mut dedupe: HashSet<String> = HashSet::new();

        let bundled_runtime_root = Self::resolve_bundled_runtime_root(application_data_path);
        let project_root = Self::resolve_project_root_path(application_data_path);
        let ffmpeg_bin = Self::resolve_ffmpeg_binary(
            bundled_runtime_root.as_deref(),
            project_root.as_deref(),
        );

        let track_id = {
            let stem = Path::new(input_audio_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if stem.trim().is_empty() {
                "ImportedTrack".to_string()
            } else {
                stem.to_string()
            }
        };

        // Runtime build: prefer_project_python = false
        let prefer_project_python = false;

        let append_bpm = |invocations: &mut Vec<PercussionPipelineInvocation>,
                          dedupe: &mut HashSet<String>,
                          backend_type: PercussionPipelineBackendType,
                          backend_label: &str,
                          script_path: &str,
                          python_commands: &[String]| {
            if script_path.trim().is_empty() || !Path::new(script_path).exists() {
                return;
            }
            if python_commands.is_empty() {
                return;
            }

            let mut args: Vec<String> = Vec::with_capacity(16);
            args.push(script_path.to_string());
            args.push(input_audio_path.to_string());
            args.push("-o".to_string());
            args.push(output_json_path.to_string());
            args.push("--track-id".to_string());
            args.push(track_id.clone());
            args.push("--bpm-only".to_string());
            args.push("on".to_string());
            args.push("--use-drum-stem".to_string());
            args.push("off".to_string());
            args.push("--emit-bass".to_string());
            args.push("off".to_string());
            args.push("--use-vocal-stem".to_string());
            args.push("off".to_string());

            if let Some(ref bin) = ffmpeg_bin {
                if !bin.trim().is_empty() {
                    args.push("--ffmpeg-bin".to_string());
                    args.push(bin.clone());
                }
            }

            for python_cmd in python_commands {
                if python_cmd.trim().is_empty() {
                    continue;
                }
                let key = format!(
                    "{}|{}|{}",
                    backend_label.to_lowercase(),
                    python_cmd.to_lowercase(),
                    script_path.to_lowercase()
                );
                if !dedupe.insert(key) {
                    continue;
                }
                invocations.push(PercussionPipelineInvocation::new(
                    backend_type,
                    backend_label,
                    python_cmd,
                    args.clone(),
                ));
            }
        };

        if prefer_project_python {
            if let Some(ref root) = project_root {
                let project_script = Path::new(root)
                    .join("Tools")
                    .join("AudioAnalysis")
                    .join(SCRIPT_FILE_NAME)
                    .to_string_lossy()
                    .into_owned();
                let project_python =
                    Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::ProjectPython,
                    "project-python-bpm",
                    &project_script,
                    &project_python,
                );
            }
            if let Some(ref brt) = bundled_runtime_root {
                let bundled_script = Self::resolve_bundled_backend_script_path(
                    brt,
                    project_root.as_deref(),
                    /*prefer_project_script_in_editor=*/ true,
                );
                let all_bundled_python =
                    Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
                let bundled_python: Vec<String> = if !all_bundled_python.is_empty() {
                    vec![all_bundled_python[0].clone()]
                } else {
                    Vec::new()
                };
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::BundledRuntime,
                    "bundled-runtime-bpm",
                    &bundled_script,
                    &bundled_python,
                );
            }
        } else {
            if let Some(ref brt) = bundled_runtime_root {
                let bundled_script = Self::resolve_bundled_backend_script_path(
                    brt,
                    project_root.as_deref(),
                    /*prefer_project_script_in_editor=*/ false,
                );
                let all_bundled_python =
                    Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
                let bundled_python: Vec<String> = if !all_bundled_python.is_empty() {
                    vec![all_bundled_python[0].clone()]
                } else {
                    Vec::new()
                };
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::BundledRuntime,
                    "bundled-runtime-bpm",
                    &bundled_script,
                    &bundled_python,
                );
            }
            if let Some(ref root) = project_root {
                let project_script = Path::new(root)
                    .join("Tools")
                    .join("AudioAnalysis")
                    .join(SCRIPT_FILE_NAME)
                    .to_string_lossy()
                    .into_owned();
                let project_python =
                    Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
                append_bpm(
                    &mut invocations,
                    &mut dedupe,
                    PercussionPipelineBackendType::ProjectPython,
                    "project-python-bpm",
                    &project_script,
                    &project_python,
                );
            }
        }

        invocations
    }

    pub fn resolve_bundled_runtime_root(application_data_path: &str) -> Option<String> {
        let project_root = Self::resolve_project_root_path(application_data_path);

        let mut candidates: Vec<String> = Vec::with_capacity(12);
        let mut seen: HashSet<String> = HashSet::new();

        let mut add = |candidate: &str| {
            if let Some(normalized) = normalize_path(candidate) {
                if seen.insert(normalized.to_lowercase()) {
                    candidates.push(normalized);
                }
            }
        };

        if let Some(ref root) = project_root {
            add(&Path::new(root)
                .join("Tools")
                .join("AudioAnalysis")
                .join("BundledRuntime")
                .join(get_bundled_runtime_platform_folder())
                .to_string_lossy()
                .into_owned());
        }

        add(&Path::new(application_data_path)
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());
        add(&Path::new(application_data_path)
            .join("Resources")
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());

        let parent = get_parent_path(application_data_path);
        add(&Path::new(parent.as_deref().unwrap_or(""))
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());
        add(&Path::new(parent.as_deref().unwrap_or(""))
            .join("Resources")
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());

        let grand_parent = get_parent_path(parent.as_deref().unwrap_or(""));
        add(&Path::new(grand_parent.as_deref().unwrap_or(""))
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());
        add(&Path::new(grand_parent.as_deref().unwrap_or(""))
            .join("Resources")
            .join(Self::BUNDLED_RUNTIME_FOLDER_NAME)
            .to_string_lossy()
            .into_owned());

        for candidate in &candidates {
            if Path::new(candidate).is_dir() && has_bundled_pipeline_files(candidate) {
                return Some(candidate.clone());
            }
        }

        None
    }

    pub fn resolve_project_root_path(application_data_path: &str) -> Option<String> {
        let mut candidates: Vec<String> = Vec::with_capacity(6);
        let mut seen: HashSet<String> = HashSet::new();

        let mut add = |candidate: &str| {
            if let Some(normalized) = normalize_path(candidate) {
                if seen.insert(normalized.to_lowercase()) {
                    candidates.push(normalized);
                }
            }
        };

        add(application_data_path);
        let p1 = get_parent_path(application_data_path);
        add(p1.as_deref().unwrap_or(""));
        let p2 = get_parent_path(p1.as_deref().unwrap_or(""));
        add(p2.as_deref().unwrap_or(""));
        let p3 = get_parent_path(p2.as_deref().unwrap_or(""));
        add(p3.as_deref().unwrap_or(""));

        for root in &candidates {
            let script = Path::new(root)
                .join("Tools")
                .join("AudioAnalysis")
                .join(SCRIPT_FILE_NAME);
            if script.exists() {
                return Some(root.clone());
            }
        }

        None
    }

    // ─── Private helpers (mirrored as associated fns) ───

    fn append_bundled_runtime_no_settings(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        bundled_runtime_root: Option<&str>,
        project_root: Option<&str>,
        prefer_project_python: bool,
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
    ) {
        let Some(brt) = bundled_runtime_root else {
            return;
        };
        if brt.is_empty() {
            return;
        }

        let bundled_script_path = Self::resolve_bundled_backend_script_path(
            brt,
            project_root,
            prefer_project_python,
        );
        let all_bundled_python =
            Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
        // Use only the first (highest-priority) Python for bundled runtime.
        let bundled_python_commands: Vec<String> = if !all_bundled_python.is_empty() {
            vec![all_bundled_python[0].clone()]
        } else {
            Vec::new()
        };
        // Don't pass --demucs-bin for bundled runtime: the demucs script's shebang
        // won't resolve to the bundled Python after relocation.
        // The Python pipeline uses sys.executable -m demucs.separate instead.
        Self::append_python_invocations(
            invocations,
            dedupe,
            PercussionPipelineBackendType::BundledRuntime,
            "bundled-runtime",
            &bundled_script_path,
            &bundled_python_commands,
            input_audio_path,
            output_json_path,
            ffmpeg_bin,
            None,
            project_root,
        );
    }

    fn append_project_python_no_settings(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        project_root: Option<&str>,
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
    ) {
        let Some(root) = project_root else {
            return;
        };
        if root.is_empty() {
            return;
        }

        let project_script_path = Path::new(root)
            .join("Tools")
            .join("AudioAnalysis")
            .join(SCRIPT_FILE_NAME)
            .to_string_lossy()
            .into_owned();
        let project_python_commands =
            Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
        Self::append_python_invocations(
            invocations,
            dedupe,
            PercussionPipelineBackendType::ProjectPython,
            "project-python",
            &project_script_path,
            &project_python_commands,
            input_audio_path,
            output_json_path,
            ffmpeg_bin,
            demucs_bin,
            project_root,
        );
    }

    fn append_bundled_runtime_with_settings(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        bundled_runtime_root: Option<&str>,
        project_root: Option<&str>,
        prefer_project_python: bool,
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
        settings: &PercussionPipelineSettings,
        instruments: Option<&str>,
    ) {
        let Some(brt) = bundled_runtime_root else {
            return;
        };
        if brt.is_empty() {
            return;
        }

        let bundled_script_path = Self::resolve_bundled_backend_script_path(
            brt,
            project_root,
            prefer_project_python,
        );
        let all_bundled_python =
            Self::resolve_python_commands(brt, /*include_system_fallback=*/ false);
        let bundled_python_commands: Vec<String> = if !all_bundled_python.is_empty() {
            vec![all_bundled_python[0].clone()]
        } else {
            Vec::new()
        };
        Self::append_python_invocations_with_settings(
            invocations,
            dedupe,
            PercussionPipelineBackendType::BundledRuntime,
            "bundled-runtime",
            &bundled_script_path,
            &bundled_python_commands,
            input_audio_path,
            output_json_path,
            ffmpeg_bin,
            None,
            project_root,
            settings,
            instruments,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn append_project_python_with_settings(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        project_root: Option<&str>,
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
        project_root_for_cache: Option<&str>,
        settings: &PercussionPipelineSettings,
        instruments: Option<&str>,
    ) {
        let Some(root) = project_root else {
            return;
        };
        if root.is_empty() {
            return;
        }

        let project_script_path = Path::new(root)
            .join("Tools")
            .join("AudioAnalysis")
            .join(SCRIPT_FILE_NAME)
            .to_string_lossy()
            .into_owned();
        let project_python_commands =
            Self::resolve_python_commands(root, /*include_system_fallback=*/ true);
        Self::append_python_invocations_with_settings(
            invocations,
            dedupe,
            PercussionPipelineBackendType::ProjectPython,
            "project-python",
            &project_script_path,
            &project_python_commands,
            input_audio_path,
            output_json_path,
            ffmpeg_bin,
            demucs_bin,
            project_root_for_cache,
            settings,
            instruments,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn append_python_invocations(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        backend_type: PercussionPipelineBackendType,
        backend_label: &str,
        script_path: &str,
        python_commands: &[String],
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
        project_root: Option<&str>,
    ) {
        if script_path.trim().is_empty() || !Path::new(script_path).exists() {
            return;
        }
        if python_commands.is_empty() {
            return;
        }

        let track_id = {
            let stem = Path::new(input_audio_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if stem.trim().is_empty() {
                "ImportedTrack".to_string()
            } else {
                stem.to_string()
            }
        };

        for python_cmd in python_commands {
            if python_cmd.trim().is_empty() {
                continue;
            }

            let key = format!(
                "{}|{}|{}",
                backend_label.to_lowercase(),
                python_cmd.to_lowercase(),
                script_path.to_lowercase()
            );
            if !dedupe.insert(key) {
                continue;
            }

            let args = Self::build_default_pipeline_arguments(
                script_path,
                input_audio_path,
                output_json_path,
                &track_id,
                ffmpeg_bin,
                demucs_bin,
                project_root,
            );

            invocations.push(PercussionPipelineInvocation::new(
                backend_type,
                backend_label,
                python_cmd,
                args,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_python_invocations_with_settings(
        invocations: &mut Vec<PercussionPipelineInvocation>,
        dedupe: &mut HashSet<String>,
        backend_type: PercussionPipelineBackendType,
        backend_label: &str,
        script_path: &str,
        python_commands: &[String],
        input_audio_path: &str,
        output_json_path: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
        project_root: Option<&str>,
        settings: &PercussionPipelineSettings,
        instruments: Option<&str>,
    ) {
        if script_path.trim().is_empty() || !Path::new(script_path).exists() {
            return;
        }
        if python_commands.is_empty() {
            return;
        }

        let track_id = {
            let stem = Path::new(input_audio_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if stem.trim().is_empty() {
                "ImportedTrack".to_string()
            } else {
                stem.to_string()
            }
        };

        for python_cmd in python_commands {
            if python_cmd.trim().is_empty() {
                continue;
            }

            let key = format!(
                "{}|{}|{}",
                backend_label.to_lowercase(),
                python_cmd.to_lowercase(),
                script_path.to_lowercase()
            );
            if !dedupe.insert(key) {
                continue;
            }

            let args = Self::build_pipeline_arguments_from_settings(
                script_path,
                input_audio_path,
                output_json_path,
                &track_id,
                ffmpeg_bin,
                demucs_bin,
                project_root,
                settings,
                instruments,
            );

            invocations.push(PercussionPipelineInvocation::new(
                backend_type,
                backend_label,
                python_cmd,
                args,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pipeline_arguments_from_settings(
        script_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        track_id: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
        project_root: Option<&str>,
        settings: &PercussionPipelineSettings,
        instruments: Option<&str>,
    ) -> Vec<String> {
        let demucs_model = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_MODEL",
            &settings.demucs.model,
        );
        let demucs_shifts = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_SHIFTS",
            &settings.demucs.shifts.to_string(),
        );
        let demucs_overlap = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_OVERLAP",
            &settings.demucs.overlap.to_string(),
        );
        let demucs_device = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_DEVICE",
            resolve_default_demucs_device(),
        );
        let demucs_jobs = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_JOBS",
            &settings.demucs.jobs.to_string(),
        );
        let demucs_segment = resolve_demucs_optional_string_setting("MANIFOLD_DEMUCS_SEGMENT");
        let demucs_no_split = settings.demucs.no_split
            || resolve_demucs_bool_setting("MANIFOLD_DEMUCS_NO_SPLIT", false);

        let mut args: Vec<String> = Vec::with_capacity(50);
        args.push(script_path.to_string());
        args.push(input_audio_path.to_string());
        args.push("-o".to_string());
        args.push(output_json_path.to_string());
        args.push("--track-id".to_string());
        args.push(track_id.to_string());
        args.push("--use-drum-stem".to_string());
        args.push(stem_mode_to_string(settings.demucs.drum_stem_mode).to_string());
        args.push("--profile".to_string());
        args.push("electronic".to_string());
        args.push("--emit-bass".to_string());
        args.push(if settings.demucs.emit_bass { "on" } else { "off" }.to_string());
        args.push("--use-bass-stem".to_string());
        args.push(stem_mode_to_string(settings.demucs.bass_stem_mode).to_string());
        args.push("--demucs-model".to_string());
        args.push(demucs_model);
        args.push("--demucs-shifts".to_string());
        args.push(demucs_shifts);
        args.push("--demucs-overlap".to_string());
        args.push(demucs_overlap);
        args.push("--demucs-device".to_string());
        args.push(demucs_device);
        args.push("--demucs-jobs".to_string());
        args.push(demucs_jobs);
        args.push("--bass-profile".to_string());
        args.push("electronic".to_string());
        args.push("--use-vocal-stem".to_string());
        args.push(stem_mode_to_string(settings.demucs.vocal_stem_mode).to_string());
        args.push("--min-bpm".to_string());
        args.push(format!("{:.1}", settings.global.min_bpm));
        args.push("--max-bpm".to_string());
        args.push(format!("{:.1}", settings.global.max_bpm));

        if let Some(segment) = demucs_segment {
            if !segment.trim().is_empty() {
                args.push("--demucs-segment".to_string());
                args.push(segment);
            }
        }

        if demucs_no_split {
            args.push("--demucs-no-split".to_string());
            args.push("on".to_string());
        }

        if let Some(bin) = ffmpeg_bin {
            if !bin.trim().is_empty() {
                args.push("--ffmpeg-bin".to_string());
                args.push(bin.to_string());
            }
        }

        if let Some(bin) = demucs_bin {
            if !bin.trim().is_empty() {
                args.push("--demucs-bin".to_string());
                args.push(bin.to_string());
            }
        }

        if let Some(instr) = instruments {
            if !instr.trim().is_empty() {
                args.push("--instruments".to_string());
                args.push(instr.to_string());
            }
        }

        append_demucs_cache_arguments(&mut args, project_root);

        let config_json = settings.serialize_to_detection_config_json();
        if !config_json.is_empty() {
            let config_path = build_temp_config_path();
            match std::fs::write(&config_path, config_json.as_bytes()) {
                Ok(()) => {
                    args.push("--config-file".to_string());
                    args.push(config_path);
                }
                Err(_) => {}
            }
        }

        args
    }

    fn build_default_pipeline_arguments(
        script_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        track_id: &str,
        ffmpeg_bin: Option<&str>,
        demucs_bin: Option<&str>,
        project_root: Option<&str>,
    ) -> Vec<String> {
        let demucs_model = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_MODEL",
            DEFAULT_DEMUCS_MODEL,
        );
        let demucs_shifts = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_SHIFTS",
            DEFAULT_DEMUCS_SHIFTS,
        );
        let demucs_overlap = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_OVERLAP",
            DEFAULT_DEMUCS_OVERLAP,
        );
        let demucs_device = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_DEVICE",
            resolve_default_demucs_device(),
        );
        let demucs_jobs = resolve_demucs_string_setting(
            "MANIFOLD_DEMUCS_JOBS",
            DEFAULT_DEMUCS_JOBS,
        );
        let demucs_segment = resolve_demucs_optional_string_setting("MANIFOLD_DEMUCS_SEGMENT");
        // --no-split crashes on full songs (htdemucs training length ~8s).
        // Only enable via env var for short clips where it avoids segment overhead.
        let demucs_no_split = resolve_demucs_bool_setting("MANIFOLD_DEMUCS_NO_SPLIT", false);

        let mut args: Vec<String> = Vec::with_capacity(44);
        args.push(script_path.to_string());
        args.push(input_audio_path.to_string());
        args.push("-o".to_string());
        args.push(output_json_path.to_string());
        args.push("--track-id".to_string());
        args.push(track_id.to_string());
        args.push("--use-drum-stem".to_string());
        args.push("on".to_string());
        args.push("--profile".to_string());
        args.push("electronic".to_string());
        args.push("--emit-bass".to_string());
        args.push("on".to_string());
        args.push("--use-bass-stem".to_string());
        args.push("auto".to_string());
        args.push("--demucs-model".to_string());
        args.push(demucs_model);
        args.push("--demucs-shifts".to_string());
        args.push(demucs_shifts);
        args.push("--demucs-overlap".to_string());
        args.push(demucs_overlap);
        args.push("--demucs-device".to_string());
        args.push(demucs_device);
        args.push("--demucs-jobs".to_string());
        args.push(demucs_jobs);
        args.push("--bass-profile".to_string());
        args.push("electronic".to_string());
        args.push("--use-vocal-stem".to_string());
        args.push("on".to_string());

        if let Some(segment) = demucs_segment {
            if !segment.trim().is_empty() {
                args.push("--demucs-segment".to_string());
                args.push(segment);
            }
        }

        if demucs_no_split {
            args.push("--demucs-no-split".to_string());
            args.push("on".to_string());
        }

        if let Some(bin) = ffmpeg_bin {
            if !bin.trim().is_empty() {
                args.push("--ffmpeg-bin".to_string());
                args.push(bin.to_string());
            }
        }

        if let Some(bin) = demucs_bin {
            if !bin.trim().is_empty() {
                args.push("--demucs-bin".to_string());
                args.push(bin.to_string());
            }
        }

        append_demucs_cache_arguments(&mut args, project_root);

        args
    }

    fn resolve_python_commands(root: &str, include_system_fallback: bool) -> Vec<String> {
        let mut commands: Vec<String> = Vec::with_capacity(10);
        let mut seen: HashSet<String> = HashSet::new();

        if !root.is_empty() {
            // Tools/AudioAnalysis/.venv (modular package venv, preferred for madmom support).
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("Tools").join("AudioAnalysis").join(".venv").join("bin").join("python3").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("Tools").join("AudioAnalysis").join(".venv").join("bin").join("python").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("Tools").join("AudioAnalysis").join(".venv").join("Scripts").join("python.exe").to_string_lossy());
            // Root-level .venv fallback.
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join(".venv").join("bin").join("python3").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join(".venv").join("bin").join("python").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join(".venv").join("Scripts").join("python.exe").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("python").join("bin").join("python3").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("python").join("bin").join("python").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("bin").join("python3").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("bin").join("python").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("python3").to_string_lossy());
            py_add_if_file(&mut commands, &mut seen, &Path::new(root).join("python").to_string_lossy());
        }

        if include_system_fallback {
            py_add(&mut commands, &mut seen, "python3");
            py_add(&mut commands, &mut seen, "python");
        }

        commands
    }

    fn resolve_ffmpeg_binary(
        bundled_runtime_root: Option<&str>,
        project_root: Option<&str>,
    ) -> Option<String> {
        let env_path = std::env::var("FFMPEG_PATH").ok();
        if let Some(ref p) = env_path {
            if !p.trim().is_empty() && Path::new(p).exists() {
                return env_path;
            }
        }

        let brt = bundled_runtime_root.unwrap_or("");
        let pr = project_root.unwrap_or("");

        let local_candidates = [
            Path::new(pr)
                .join("Tools")
                .join("AudioAnalysis")
                .join(".venv")
                .join("bin")
                .join("ffmpeg")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt).join("ffmpeg").to_string_lossy().into_owned(),
            Path::new(brt)
                .join("bin")
                .join("ffmpeg")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join(".venv")
                .join("bin")
                .join("ffmpeg")
                .to_string_lossy()
                .into_owned(),
            Path::new(pr)
                .join(".venv")
                .join("bin")
                .join("ffmpeg")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join("ffmpeg.exe")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join("bin")
                .join("ffmpeg.exe")
                .to_string_lossy()
                .into_owned(),
        ];

        for candidate in &local_candidates {
            if Path::new(candidate).exists() {
                return Some(candidate.clone());
            }
        }

        let system_candidates = [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ];

        for candidate in &system_candidates {
            if Path::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        }

        None
    }

    fn resolve_demucs_binary(
        bundled_runtime_root: Option<&str>,
        project_root: Option<&str>,
    ) -> Option<String> {
        let env_path = std::env::var("DEMUCS_PATH").ok();
        if let Some(ref p) = env_path {
            if !p.trim().is_empty() && Path::new(p).exists() {
                return env_path;
            }
        }

        let brt = bundled_runtime_root.unwrap_or("");
        let pr = project_root.unwrap_or("");

        let local_candidates = [
            Path::new(pr)
                .join("Tools")
                .join("AudioAnalysis")
                .join(".venv")
                .join("bin")
                .join("demucs")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join("python")
                .join("bin")
                .join("demucs")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt).join("demucs").to_string_lossy().into_owned(),
            Path::new(brt)
                .join("bin")
                .join("demucs")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join(".venv")
                .join("bin")
                .join("demucs")
                .to_string_lossy()
                .into_owned(),
            Path::new(pr)
                .join(".venv")
                .join("bin")
                .join("demucs")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join("demucs.exe")
                .to_string_lossy()
                .into_owned(),
            Path::new(brt)
                .join("bin")
                .join("demucs.exe")
                .to_string_lossy()
                .into_owned(),
        ];

        for candidate in &local_candidates {
            if Path::new(candidate).exists() {
                return Some(candidate.clone());
            }
        }

        let system_candidates = [
            "/opt/homebrew/bin/demucs",
            "/usr/local/bin/demucs",
            "/usr/bin/demucs",
        ];

        for candidate in &system_candidates {
            if Path::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        }

        None
    }

    fn resolve_bundled_backend_script_path(
        bundled_runtime_root: &str,
        project_root: Option<&str>,
        prefer_project_script_in_editor: bool,
    ) -> String {
        let bundled_script_path = Path::new(bundled_runtime_root)
            .join(SCRIPT_FILE_NAME)
            .to_string_lossy()
            .into_owned();

        if !prefer_project_script_in_editor {
            return bundled_script_path;
        }
        let Some(root) = project_root else {
            return bundled_script_path;
        };
        if root.is_empty() {
            return bundled_script_path;
        }

        let project_script_path = Path::new(root)
            .join("Tools")
            .join("AudioAnalysis")
            .join(SCRIPT_FILE_NAME)
            .to_string_lossy()
            .into_owned();
        if Path::new(&project_script_path).exists() {
            return project_script_path;
        }

        bundled_script_path
    }
}

// ─── Module-level helpers ───

/// Port of Unity AddIfFile — adds candidate to collection only if it exists on disk.
fn py_add_if_file(commands: &mut Vec<String>, seen: &mut HashSet<String>, candidate: &str) {
    if candidate.trim().is_empty() || !Path::new(candidate).exists() {
        return;
    }
    if seen.insert(candidate.to_lowercase()) {
        commands.push(candidate.to_string());
    }
}

/// Port of Unity Add — adds command name (system command, not file path) if not already seen.
fn py_add(commands: &mut Vec<String>, seen: &mut HashSet<String>, path: &str) {
    if path.trim().is_empty() {
        return;
    }
    if seen.insert(path.to_lowercase()) {
        commands.push(path.to_string());
    }
}

fn has_bundled_pipeline_files(runtime_root: &str) -> bool {
    if runtime_root.trim().is_empty() {
        return false;
    }

    let script_path = Path::new(runtime_root).join(SCRIPT_FILE_NAME);
    if !script_path.exists() {
        return false;
    }

    let shims_path = Path::new(runtime_root).join(SHIMS_FILE_NAME);
    shims_path.exists()
}

fn get_parent_path(path: &str) -> Option<String> {
    if path.trim().is_empty() {
        return None;
    }
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
}

fn normalize_path(path: &str) -> Option<String> {
    if path.trim().is_empty() {
        return None;
    }
    match std::fs::canonicalize(path) {
        Ok(p) => Some(p.to_string_lossy().into_owned()),
        Err(_) => {
            // canonicalize requires the path to exist; fall back to the raw path
            // so non-existent candidates can still be deduplicated by string.
            Some(path.to_string())
        }
    }
}

fn get_bundled_runtime_platform_folder() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        "macOS"
    }
}

fn resolve_demucs_string_setting<'a>(key: &str, fallback: &'a str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => fallback.to_string(),
    }
}

fn resolve_demucs_optional_string_setting(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

fn resolve_demucs_bool_setting(key: &str, default_value: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let normalized = v.trim().to_lowercase();
            if normalized == "1" || normalized == "on" || normalized == "true" || normalized == "yes" {
                return true;
            }
            if normalized == "0" || normalized == "off" || normalized == "false" || normalized == "no" {
                return false;
            }
            default_value
        }
        Err(_) => default_value,
    }
}

fn resolve_default_demucs_device() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "mps"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "cpu"
    }
}

fn stem_mode_to_string(mode: StemMode) -> &'static str {
    match mode {
        StemMode::On => "on",
        StemMode::Off => "off",
        StemMode::Auto => "auto",
    }
}

fn append_demucs_cache_arguments(args: &mut Vec<String>, project_root: Option<&str>) {
    // In Unity editor, stems are always cached so the expand/audition UI works.
    // In the runtime build (Rust), only cache if explicitly enabled via env var.
    // is_editor = false always in Rust port.
    let is_editor = false;

    let enable_cache = std::env::var("MANIFOLD_DEMUCS_CACHE").ok();
    let enable_cache_str = enable_cache.as_deref().unwrap_or("").trim().to_lowercase();

    let enabled_by_env = enable_cache_str == "1"
        || enable_cache_str == "on"
        || enable_cache_str == "true";

    let disabled_by_env = enable_cache_str == "0"
        || enable_cache_str == "off"
        || enable_cache_str == "false";

    let should_enable = enabled_by_env || (is_editor && !disabled_by_env);
    if !should_enable {
        return;
    }

    let cache_dir = {
        let from_env = std::env::var("MANIFOLD_DEMUCS_CACHE_DIR").ok();
        match from_env {
            Some(ref d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => match project_root {
                Some(root) if !root.trim().is_empty() => Path::new(root)
                    .join("Library")
                    .join("AudioAnalysisStemCache")
                    .to_string_lossy()
                    .into_owned(),
                _ => return,
            },
        }
    };

    if cache_dir.trim().is_empty() {
        return;
    }

    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }

    args.push("--demucs-cache-dir".to_string());
    args.push(cache_dir);
    args.push("--reuse-demucs-cache".to_string());
    args.push("on".to_string());
}

/// Builds a unique temp-file path for the detection config JSON.
/// Unity used Guid.NewGuid().ToString("N") — here we use a counter + time combo
/// since no uuid crate is available, which is sufficient for a temp file name.
fn build_temp_config_path() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("manifold_detection_config_{:016x}{:08x}.json", ts, seq);
    std::env::temp_dir()
        .join(name)
        .to_string_lossy()
        .into_owned()
}
