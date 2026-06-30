//! Audio decode utility: load an audio file into a kira [`StaticSoundData`] plus
//! its encoder-delay probe. Shared by per-clip audio-layer playback
//! ([`crate::audio_layer_playback`]) and the offline export mixdown
//! ([`crate::audio_mixdown`]). The old project-global imported-audio playback
//! controller lived here too; it was removed with the legacy percussion feature
//! (audio is per-clip now). Only the decode helpers remain.

use kira::sound::static_sound::StaticSoundData;
use manifold_core::{Beats, Seconds};
use std::path::Path;
use std::process::Command;

const MAX_ENCODER_DELAY_SECONDS: f32 = 0.5;

/// Pre-decoded audio data: the kira sound plus the probed encoder delay.
/// The heavy I/O (file read + decode + ffprobe) is safe to run off-thread.
pub struct PreloadedAudioData {
    pub sound_data: StaticSoundData,
    pub encoder_delay: Seconds,
    pub clip_duration: Seconds,
    pub path: String,
    pub start_beat: Beats,
}

/// Decode `path` into a [`StaticSoundData`] and probe its encoder delay. Safe to
/// call from any thread. `start_beat_offset` is carried through for callers that
/// place the clip on a timeline (clamped to ≥ 0).
pub fn preload_audio(path: &str, start_beat_offset: Beats) -> Result<PreloadedAudioData, String> {
    let sound_data =
        StaticSoundData::from_file(path).map_err(|e| format!("Failed to load audio: {}", e))?;

    let clip_duration = sound_data.duration().as_secs_f64();
    if clip_duration <= 0.0 {
        return Err("Decoded audio clip has zero duration".to_string());
    }

    let encoder_delay = probe_encoder_delay_seconds(path);

    Ok(PreloadedAudioData {
        sound_data,
        encoder_delay,
        clip_duration: Seconds(clip_duration),
        path: path.to_string(),
        start_beat: start_beat_offset.max(Beats::ZERO),
    })
}

// ─── ffprobe encoder delay probing ───

/// Returns the encoder priming delay in seconds (lossy formats), or 0 for
/// lossless formats / when ffprobe is unavailable.
fn probe_encoder_delay_seconds(audio_path: &str) -> Seconds {
    if audio_path.is_empty() {
        return Seconds::ZERO;
    }

    let ext = Path::new(audio_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    // Lossless formats have no encoder delay.
    if ext == "wav" || ext == "aif" || ext == "aiff" {
        return Seconds::ZERO;
    }

    let ffprobe = match resolve_ffprobe_binary() {
        Some(p) => p,
        None => return Seconds::ZERO,
    };

    let output = match run_ffprobe_query(&ffprobe, audio_path) {
        Some(o) => o,
        None => return Seconds::ZERO,
    };

    let trimmed = output.trim();
    if let Ok(start_time) = trimmed.parse::<f32>()
        && start_time > 0.0001
        && start_time <= MAX_ENCODER_DELAY_SECONDS
    {
        return Seconds(start_time as f64);
    }

    Seconds::ZERO
}

fn run_ffprobe_query(ffprobe_path: &str, audio_path: &str) -> Option<String> {
    let output = Command::new(ffprobe_path)
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "format=start_time",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            audio_path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let result = String::from_utf8_lossy(&output.stdout).to_string();
    if result.is_empty() { None } else { Some(result) }
}

fn resolve_ffprobe_binary() -> Option<String> {
    // 1. Explicit env var.
    if let Ok(env_path) = std::env::var("FFPROBE_PATH")
        && !env_path.is_empty()
        && Path::new(&env_path).exists()
    {
        return Some(env_path);
    }

    // 2. Derive from FFMPEG_PATH by replacing the binary name.
    if let Ok(ffmpeg_env) = std::env::var("FFMPEG_PATH")
        && !ffmpeg_env.is_empty()
        && let Some(derived) = derive_ffprobe_from_ffmpeg_path(&ffmpeg_env)
        && Path::new(&derived).exists()
    {
        return Some(derived);
    }

    // 3. Common system paths.
    let candidates = [
        "/opt/homebrew/bin/ffprobe",
        "/usr/local/bin/ffprobe",
        "/usr/bin/ffprobe",
    ];
    for candidate in &candidates {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }

    None
}

fn derive_ffprobe_from_ffmpeg_path(ffmpeg_path: &str) -> Option<String> {
    if ffmpeg_path.is_empty() {
        return None;
    }

    let path = Path::new(ffmpeg_path);
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;

    // Replace "ffmpeg" with "ffprobe" in the binary name.
    let probe_name = name.replace("ffmpeg", "ffprobe");
    if probe_name == name {
        return None;
    }

    Some(dir.join(&probe_name).to_string_lossy().to_string())
}
