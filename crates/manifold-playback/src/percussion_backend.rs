// Percussion pipeline backend resolver — bundled-only.
//
// Finds the bundled Python runtime (created by tools/audio_analysis/stage_runtime_mac.sh)
// and builds CLI invocations for the percussion analysis pipeline.
//
// Env var escape hatches for development:
//   MANIFOLD_PYTHON_PATH  — override bundled Python binary
//   FFMPEG_PATH           — override bundled ffmpeg binary
//   MANIFOLD_DEMUCS_*     — override demucs settings (model, device, shifts, etc.)

use std::path::Path;

use manifold_core::percussion_settings::{PercussionPipelineSettings, StemMode};

// ─── Constants ───

const SCRIPT_FILE_NAME: &str = "percussion_json_pipeline.py";
const SHIMS_FILE_NAME: &str = "lameenc.py";
const DEFAULT_DEMUCS_MODEL: &str = "htdemucs";
const DEFAULT_DEMUCS_SHIFTS: &str = "1";
const DEFAULT_DEMUCS_OVERLAP: &str = "0.25";
const DEFAULT_DEMUCS_JOBS: &str = "0";

// ─── PercussionPipelineInvocation ───

/// A resolved, ready-to-execute pipeline invocation.
#[derive(Debug, Clone)]
pub struct PercussionPipelineInvocation {
    pub command: String,
    pub arguments: Vec<String>,
    pub label: String,
}

// ─── PercussionPipelineBackendResolver ───

pub struct PercussionPipelineBackendResolver;

impl PercussionPipelineBackendResolver {
    pub const BUNDLED_RUNTIME_FOLDER_NAME: &'static str = "AudioAnalysisRuntime";

    /// Build a pipeline invocation with default settings (no PercussionPipelineSettings).
    pub fn build_invocation(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
    ) -> Option<PercussionPipelineInvocation> {
        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return None;
        }

        let runtime_root = Self::resolve_bundled_runtime_root(application_data_path)?;
        let python = Self::resolve_python(&runtime_root)?;
        let script = Self::resolve_script(&runtime_root)?;
        let ffmpeg = Self::resolve_ffmpeg(&runtime_root);

        let track_id = extract_track_id(input_audio_path);
        let args = build_default_arguments(
            &script,
            input_audio_path,
            output_json_path,
            &track_id,
            ffmpeg.as_deref(),
        );

        Some(PercussionPipelineInvocation {
            command: python,
            arguments: args,
            label: "bundled-runtime".to_string(),
        })
    }

    /// Build a pipeline invocation with full settings.
    pub fn build_invocation_with_settings(
        application_data_path: &str,
        input_audio_path: &str,
        output_json_path: &str,
        settings: &PercussionPipelineSettings,
        instruments: Option<&str>,
    ) -> Option<PercussionPipelineInvocation> {
        if input_audio_path.trim().is_empty() || output_json_path.trim().is_empty() {
            return None;
        }

        let runtime_root = Self::resolve_bundled_runtime_root(application_data_path)?;
        let python = Self::resolve_python(&runtime_root)?;
        let script = Self::resolve_script(&runtime_root)?;
        let ffmpeg = Self::resolve_ffmpeg(&runtime_root);

        let track_id = extract_track_id(input_audio_path);
        let args = build_settings_arguments(
            &script,
            input_audio_path,
            output_json_path,
            &track_id,
            ffmpeg.as_deref(),
            settings,
            instruments,
        );

        Some(PercussionPipelineInvocation {
            command: python,
            arguments: args,
            label: "bundled-runtime".to_string(),
        })
    }

    // ─── Resolution helpers ───

    /// Find the bundled Python interpreter.
    /// Checks MANIFOLD_PYTHON_PATH env var first, then bundled runtime paths.
    fn resolve_python(runtime_root: &str) -> Option<String> {
        // Env var escape hatch for development.
        if let Ok(p) = std::env::var("MANIFOLD_PYTHON_PATH")
            && !p.trim().is_empty() && Path::new(&p).exists() {
                return Some(p);
            }

        let candidates = [
            Path::new(runtime_root).join("python").join("bin").join("python3"),
            Path::new(runtime_root).join("python").join("bin").join("python"),
            Path::new(runtime_root).join("bin").join("python3"),
            Path::new(runtime_root).join("python3"),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }

        None
    }

    /// Find the pipeline entry script in the bundled runtime.
    fn resolve_script(runtime_root: &str) -> Option<String> {
        let script = Path::new(runtime_root).join(SCRIPT_FILE_NAME);
        if script.exists() {
            Some(script.to_string_lossy().into_owned())
        } else {
            None
        }
    }

    /// Find ffmpeg in the bundled runtime or system paths.
    fn resolve_ffmpeg(runtime_root: &str) -> Option<String> {
        // Env var escape hatch.
        if let Ok(p) = std::env::var("FFMPEG_PATH")
            && !p.trim().is_empty() && Path::new(&p).exists() {
                return Some(p);
            }

        let candidates = [
            Path::new(runtime_root).join("bin").join("ffmpeg"),
            Path::new(runtime_root).join("ffmpeg"),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }

        // System fallback (ffmpeg is commonly installed separately).
        let system = [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ];
        for candidate in &system {
            if Path::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        }

        None
    }

    /// Locate the bundled runtime directory by searching known paths.
    /// The runtime is produced by `tools/audio_analysis/stage_runtime_mac.sh`.
    pub fn resolve_bundled_runtime_root(application_data_path: &str) -> Option<String> {
        let platform = platform_folder();

        // Walk up from application_data_path looking for the tools directory
        // or the bundled runtime in standard app bundle locations.
        let mut candidates: Vec<std::path::PathBuf> = Vec::with_capacity(8);

        // 1. Repo development path: {repo}/tools/audio_analysis/BundledRuntime/{platform}/
        let mut dir = Path::new(application_data_path).to_path_buf();
        for _ in 0..4 {
            let candidate = dir.join("tools").join("audio_analysis")
                .join("BundledRuntime").join(platform);
            candidates.push(candidate);
            if !dir.pop() { break; }
        }

        // 2. App bundle paths (macOS .app/Contents/Resources/AudioAnalysisRuntime/)
        let app_data = Path::new(application_data_path);
        candidates.push(app_data.join(Self::BUNDLED_RUNTIME_FOLDER_NAME));
        candidates.push(app_data.join("Resources").join(Self::BUNDLED_RUNTIME_FOLDER_NAME));
        if let Some(parent) = app_data.parent() {
            candidates.push(parent.join(Self::BUNDLED_RUNTIME_FOLDER_NAME));
            candidates.push(parent.join("Resources").join(Self::BUNDLED_RUNTIME_FOLDER_NAME));
        }

        for candidate in &candidates {
            if candidate.is_dir() && has_bundled_pipeline_files(candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }

        None
    }
}

// ─── Argument building ───

fn build_default_arguments(
    script_path: &str,
    input_audio_path: &str,
    output_json_path: &str,
    track_id: &str,
    ffmpeg_bin: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::with_capacity(32);
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
    args.push("--use-vocal-stem".to_string());
    args.push("on".to_string());
    args.push("--bass-profile".to_string());
    args.push("electronic".to_string());

    append_demucs_defaults(&mut args);

    if let Some(bin) = ffmpeg_bin
        && !bin.trim().is_empty() {
            args.push("--ffmpeg-bin".to_string());
            args.push(bin.to_string());
        }

    args
}

#[allow(clippy::too_many_arguments)]
fn build_settings_arguments(
    script_path: &str,
    input_audio_path: &str,
    output_json_path: &str,
    track_id: &str,
    ffmpeg_bin: Option<&str>,
    settings: &PercussionPipelineSettings,
    instruments: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::with_capacity(50);
    args.push(script_path.to_string());
    args.push(input_audio_path.to_string());
    args.push("-o".to_string());
    args.push(output_json_path.to_string());
    args.push("--track-id".to_string());
    args.push(track_id.to_string());
    args.push("--use-drum-stem".to_string());
    args.push(stem_mode_str(settings.demucs.drum_stem_mode).to_string());
    args.push("--profile".to_string());
    args.push("electronic".to_string());
    args.push("--emit-bass".to_string());
    args.push(if settings.demucs.emit_bass { "on" } else { "off" }.to_string());
    args.push("--use-bass-stem".to_string());
    args.push(stem_mode_str(settings.demucs.bass_stem_mode).to_string());
    args.push("--use-vocal-stem".to_string());
    args.push(stem_mode_str(settings.demucs.vocal_stem_mode).to_string());
    args.push("--bass-profile".to_string());
    args.push("electronic".to_string());
    args.push("--min-bpm".to_string());
    args.push(format!("{:.1}", settings.global.min_bpm));
    args.push("--max-bpm".to_string());
    args.push(format!("{:.1}", settings.global.max_bpm));

    // Demucs settings (env var overrides take precedence).
    args.push("--demucs-model".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_MODEL", &settings.demucs.model));
    args.push("--demucs-shifts".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_SHIFTS", &settings.demucs.shifts.to_string()));
    args.push("--demucs-overlap".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_OVERLAP", &settings.demucs.overlap.to_string()));
    args.push("--demucs-device".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_DEVICE", default_demucs_device()));
    args.push("--demucs-jobs".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_JOBS", &settings.demucs.jobs.to_string()));

    if let Some(segment) = env_opt("MANIFOLD_DEMUCS_SEGMENT") {
        args.push("--demucs-segment".to_string());
        args.push(segment);
    }

    let no_split = settings.demucs.no_split || env_bool("MANIFOLD_DEMUCS_NO_SPLIT", false);
    if no_split {
        args.push("--demucs-no-split".to_string());
        args.push("on".to_string());
    }

    if let Some(bin) = ffmpeg_bin
        && !bin.trim().is_empty() {
            args.push("--ffmpeg-bin".to_string());
            args.push(bin.to_string());
        }

    if let Some(instr) = instruments
        && !instr.trim().is_empty() {
            args.push("--instruments".to_string());
            args.push(instr.to_string());
        }

    // Demucs stem caching (only if explicitly enabled via env var).
    append_demucs_cache_arguments(&mut args);

    // Detection config JSON (algorithm tuning parameters).
    let config_json = settings.serialize_to_detection_config_json();
    if !config_json.is_empty() {
        let config_path = build_temp_config_path();
        if std::fs::write(&config_path, config_json.as_bytes()).is_ok() {
            args.push("--config-file".to_string());
            args.push(config_path);
        }
    }

    args
}

fn append_demucs_defaults(args: &mut Vec<String>) {
    args.push("--demucs-model".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_MODEL", DEFAULT_DEMUCS_MODEL));
    args.push("--demucs-shifts".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_SHIFTS", DEFAULT_DEMUCS_SHIFTS));
    args.push("--demucs-overlap".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_OVERLAP", DEFAULT_DEMUCS_OVERLAP));
    args.push("--demucs-device".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_DEVICE", default_demucs_device()));
    args.push("--demucs-jobs".to_string());
    args.push(env_or("MANIFOLD_DEMUCS_JOBS", DEFAULT_DEMUCS_JOBS));

    if let Some(segment) = env_opt("MANIFOLD_DEMUCS_SEGMENT") {
        args.push("--demucs-segment".to_string());
        args.push(segment);
    }

    if env_bool("MANIFOLD_DEMUCS_NO_SPLIT", false) {
        args.push("--demucs-no-split".to_string());
        args.push("on".to_string());
    }
}

// ─── Helpers ───

fn extract_track_id(input_audio_path: &str) -> String {
    Path::new(input_audio_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("ImportedTrack")
        .to_string()
}

fn has_bundled_pipeline_files(runtime_root: &Path) -> bool {
    runtime_root.join(SCRIPT_FILE_NAME).exists()
        && runtime_root.join(SHIMS_FILE_NAME).exists()
}

fn platform_folder() -> &'static str {
    #[cfg(target_os = "windows")] { "windows" }
    #[cfg(target_os = "linux")] { "linux" }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))] { "macOS" }
}

fn default_demucs_device() -> &'static str {
    #[cfg(target_os = "macos")] { "mps" }
    #[cfg(not(target_os = "macos"))] { "cpu" }
}

fn stem_mode_str(mode: StemMode) -> &'static str {
    match mode {
        StemMode::On => "on",
        StemMode::Off => "off",
        StemMode::Auto => "auto",
    }
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(v.trim().to_lowercase().as_str(), "1" | "on" | "true" | "yes"),
        Err(_) => default,
    }
}

fn append_demucs_cache_arguments(args: &mut Vec<String>) {
    if !env_bool("MANIFOLD_DEMUCS_CACHE", false) {
        return;
    }

    let cache_dir = match env_opt("MANIFOLD_DEMUCS_CACHE_DIR") {
        Some(d) => d,
        None => return,
    };

    if std::fs::create_dir_all(&cache_dir).is_err() {
        return;
    }

    args.push("--demucs-cache-dir".to_string());
    args.push(cache_dir);
    args.push("--reuse-demucs-cache".to_string());
    args.push("on".to_string());
}

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
