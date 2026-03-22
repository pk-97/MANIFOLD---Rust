// Port of Unity ImportedAudioSyncController.cs (473 lines).
// Manages playback sync of an imported audio file against the timeline.
// Uses kira for audio decoding + playback (replacing Unity AudioSource + AudioClip).

use kira::{
    manager::{AudioManager, AudioManagerSettings, backend::DefaultBackend},
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    sound::PlaybackState as KiraPlaybackState,
    tween::Tween,
};
use manifold_core::types::PlaybackState;
use crate::engine::PlaybackEngine;
use std::path::Path;
use std::process::Command;

const SEEK_TOLERANCE_SECONDS: f32 = 0.06;
const HARD_RESYNC_SECONDS: f32 = 0.20;
const MAX_ENCODER_DELAY_SECONDS: f32 = 0.5;

/// Port of Unity ImportedAudioSyncController : MonoBehaviour.
/// Owns a kira AudioManager (equivalent to Unity AudioSource lifecycle).
pub struct ImportedAudioSyncController {
    audio_manager: AudioManager<DefaultBackend>,
    sound_handle: Option<StaticSoundHandle>,
    sound_data: Option<StaticSoundData>,
    clip_duration_seconds: f32,
    audio_path: Option<String>,
    start_beat: f32,
    start_time_seconds: f32,
    encoder_delay_seconds: f32,
    is_ready: bool,
    on_clip_changed: Option<Box<dyn FnMut(bool) + Send>>,
}

impl ImportedAudioSyncController {
    /// Port of Awake(). Creates the audio manager (equivalent to AddComponent<AudioSource>).
    pub fn new() -> Result<Self, String> {
        let audio_manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|e| format!("Failed to create audio manager: {}", e))?;

        Ok(Self {
            audio_manager,
            sound_handle: None,
            sound_data: None,
            clip_duration_seconds: 0.0,
            audio_path: None,
            start_beat: 0.0,
            start_time_seconds: 0.0,
            encoder_delay_seconds: 0.0,
            is_ready: false,
            on_clip_changed: None,
        })
    }

    // ─── Properties (port of C# public properties) ───

    pub fn is_ready(&self) -> bool { self.is_ready }
    pub fn start_beat(&self) -> f32 { self.start_beat }
    pub fn encoder_delay_seconds(&self) -> f32 { self.encoder_delay_seconds }
    pub fn audio_path(&self) -> Option<&str> { self.audio_path.as_deref() }
    pub fn clip_duration_seconds(&self) -> f32 { self.clip_duration_seconds }

    pub fn set_on_clip_changed(&mut self, callback: Option<Box<dyn FnMut(bool) + Send>>) {
        self.on_clip_changed = callback;
    }

    /// Set the master audio volume. Used by StemAudioController to mute/unmute
    /// the master when expanding/collapsing stems.
    /// Port of C# masterController.Source.volume = value.
    pub fn set_volume(&mut self, volume: f32) {
        if let Some(ref mut handle) = self.sound_handle {
            handle.set_volume(volume as f64, Tween::default());
        }
    }

    // ─── LoadAudioAsync (port of C# IEnumerator LoadAudioAsync) ───

    /// Loads and decodes an audio file synchronously (kira decodes into memory).
    /// Port of Unity LoadAudioAsync(string path, float startBeatOffset).
    pub fn load_audio(&mut self, path: &str, start_beat_offset: f32) -> Result<(), String> {
        if path.is_empty() {
            return Ok(());
        }

        self.is_ready = false;

        // Load and decode the audio file (equivalent to UnityWebRequestMultimedia.GetAudioClip).
        let sound_data = StaticSoundData::from_file(path)
            .map_err(|e| {
                log::warn!("[ImportedAudioSyncController] Failed to load imported audio for playback: {}", e);
                format!("Failed to load audio: {}", e)
            })?;

        let clip_duration = sound_data.duration().as_secs_f32();
        if clip_duration <= 0.0 {
            log::warn!("[ImportedAudioSyncController] Failed to decode imported audio clip.");
            return Err("Decoded audio clip has zero duration".to_string());
        }

        // Stop and discard previous sound handle.
        if let Some(ref mut handle) = self.sound_handle {
            handle.stop(Tween::default());
        }
        self.sound_handle = None;

        self.clip_duration_seconds = clip_duration;
        self.audio_path = Some(path.to_string());
        self.start_beat = start_beat_offset.max(0.0);
        // startTimeSeconds will be recalculated on first UpdateSync call.
        self.start_time_seconds = 0.0;
        self.encoder_delay_seconds = probe_encoder_delay_seconds(path);

        // Play the sound immediately paused (equivalent to audioSource.clip = audioClip).
        let data_clone = sound_data.clone();
        let mut handle = self.audio_manager.play(data_clone)
            .map_err(|e| format!("Failed to play audio: {}", e))?;
        handle.pause(Tween::default());
        handle.seek_to(0.0);
        self.sound_handle = Some(handle);
        self.sound_data = Some(sound_data);

        self.is_ready = true;
        if let Some(ref mut cb) = self.on_clip_changed {
            cb(true);
        }

        let file_name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let delay_info = if self.encoder_delay_seconds > 0.0 {
            format!(" encoderDelay={:.1}ms", self.encoder_delay_seconds * 1000.0)
        } else {
            String::new()
        };
        log::info!(
            "[ImportedAudioSyncController] Imported audio attached for sync: '{}' startBeat={:.2}{}",
            file_name, self.start_beat, delay_info
        );

        Ok(())
    }

    // ─── SetStartBeat ───

    /// Port of C# SetStartBeat(float beat, PlaybackController playbackController).
    pub fn set_start_beat(&mut self, beat: f32, engine: &mut PlaybackEngine) {
        self.start_beat = beat.max(0.0);
        self.start_time_seconds = engine.beat_to_timeline_time(self.start_beat);
    }

    // ─── GetDurationBeats ───

    /// Port of C# GetDurationBeats(PlaybackController playbackController).
    pub fn get_duration_beats(&mut self, engine: &mut PlaybackEngine) -> f32 {
        if !self.is_ready || self.clip_duration_seconds <= 0.0 {
            return 0.0;
        }

        self.start_time_seconds = engine.beat_to_timeline_time(self.start_beat);
        let end_time = self.start_time_seconds + self.clip_duration_seconds;
        let end_beat = engine.time_to_timeline_beat(end_time);
        if !end_beat.is_finite() {
            return 0.0;
        }

        (end_beat - self.start_beat).max(0.0)
    }

    // ─── GetEndBeat ───

    /// Port of C# GetEndBeat(PlaybackController playbackController).
    pub fn get_end_beat(&mut self, engine: &mut PlaybackEngine) -> f32 {
        self.start_beat + self.get_duration_beats(engine)
    }

    // ─── UpdateSync (main sync loop) ───

    /// Port of C# UpdateSync(PlaybackController playbackController).
    /// Called every frame from the app tick loop.
    pub fn update_sync(&mut self, engine: &mut PlaybackEngine) {
        if !self.is_ready || self.clip_duration_seconds <= 0.0 {
            return;
        }
        if self.sound_handle.is_none() {
            return;
        }

        // Sync start_beat from the project's authoritative audio_start_beat.
        // MutateProject (waveform drag) can change the project value without
        // calling set_start_beat(), so we always read the latest here.
        if let Some(project) = engine.project()
            && let Some(perc) = &project.percussion_import
        {
            self.start_beat = perc.audio_start_beat;
        }

        let clip_length = self.clip_duration_seconds;

        // Keep beat anchor aligned with any transport/tempo timing changes.
        self.start_time_seconds = engine.beat_to_timeline_time(self.start_beat);
        // Offset by encoder delay so playback cursor skips past the
        // MP3 padding that ffmpeg strips during analysis decoding.
        let expected_time = engine.current_time() - self.start_time_seconds + self.encoder_delay_seconds;
        let in_range = expected_time >= 0.0 && expected_time < clip_length;
        let clamped_expected = expected_time.clamp(0.0, (clip_length - 0.001).max(0.0));

        let handle_state = self.sound_handle.as_ref().unwrap().state();
        let is_source_playing = handle_state == KiraPlaybackState::Playing;

        match engine.current_state() {
            PlaybackState::Playing => {
                if !in_range {
                    if is_source_playing {
                        self.sound_handle.as_mut().unwrap().pause(Tween::default());
                    }
                    return;
                }

                if !is_source_playing {
                    // Kira auto-transitions to Stopped when audio reaches its
                    // natural end. resume() is a no-op on stopped handles, so
                    // we must re-play from sound_data to get a fresh handle.
                    // Unity's audioSource.Play() works from any state — this
                    // matches that semantic.
                    if handle_state == KiraPlaybackState::Stopped {
                        if let Some(ref data) = self.sound_data {
                            match self.audio_manager.play(data.clone()) {
                                Ok(mut new_handle) => {
                                    new_handle.seek_to(clamped_expected as f64);
                                    self.sound_handle = Some(new_handle);
                                }
                                Err(e) => {
                                    log::warn!("[ImportedAudioSyncController] Failed to restart stopped audio: {}", e);
                                }
                            }
                        }
                    } else {
                        let handle = self.sound_handle.as_mut().unwrap();
                        handle.seek_to(clamped_expected as f64);
                        handle.resume(Tween::default());
                    }
                    return;
                }

                let handle = self.sound_handle.as_mut().unwrap();
                let current_pos = handle.position() as f32;
                if (current_pos - clamped_expected).abs() > HARD_RESYNC_SECONDS {
                    handle.seek_to(clamped_expected as f64);
                }
            }
            PlaybackState::Paused => {
                let handle = self.sound_handle.as_mut().unwrap();
                if is_source_playing {
                    handle.pause(Tween::default());
                }

                if in_range {
                    let current_pos = handle.position() as f32;
                    if (current_pos - clamped_expected).abs() > SEEK_TOLERANCE_SECONDS {
                        handle.seek_to(clamped_expected as f64);
                    }
                }
            }
            _ => {
                // Stopped
                let handle = self.sound_handle.as_mut().unwrap();
                if is_source_playing {
                    handle.pause(Tween::default());
                }

                let current_pos = handle.position() as f32;
                if current_pos > 0.0 {
                    handle.seek_to(0.0);
                }
            }
        }
    }

    // ─── TryGetSourceSecondsAtPlayhead ───

    /// Port of C# TryGetSourceSecondsAtPlayhead(PlaybackController, out float, out float).
    /// Returns Some((source_seconds, playhead_beat)) or None.
    pub fn try_get_source_seconds_at_playhead(
        &mut self,
        engine: &mut PlaybackEngine,
    ) -> Option<(f32, f32)> {
        let playhead_beat = engine.current_beat();
        self.start_time_seconds = engine.beat_to_timeline_time(self.start_beat);
        let source_seconds = engine.current_time() - self.start_time_seconds;
        if !source_seconds.is_finite() {
            return None;
        }

        if self.clip_duration_seconds > 0.0 {
            if source_seconds < -0.0001 || source_seconds > self.clip_duration_seconds + 0.0001 {
                return None;
            }
            let clamped = source_seconds.clamp(0.0, self.clip_duration_seconds);
            return Some((clamped, playhead_beat));
        }

        if source_seconds >= 0.0 {
            Some((source_seconds, playhead_beat))
        } else {
            None
        }
    }

    // ─── ResetAudio ───

    /// Port of C# ResetAudio().
    pub fn reset_audio(&mut self) {
        self.is_ready = false;
        self.audio_path = None;
        self.start_beat = 0.0;
        self.start_time_seconds = 0.0;
        self.encoder_delay_seconds = 0.0;
        if let Some(ref mut cb) = self.on_clip_changed {
            cb(false);
        }

        if let Some(ref mut handle) = self.sound_handle {
            if handle.state() == KiraPlaybackState::Playing {
                handle.pause(Tween::default());
            }
            handle.stop(Tween::default());
        }
        self.sound_handle = None;
        self.sound_data = None;
        self.clip_duration_seconds = 0.0;
    }

    /// Applies pre-loaded audio data to the controller. Fast — only does
    /// AudioManager::play (no file I/O). Must be called on the main thread.
    pub fn apply_preloaded(&mut self, preloaded: PreloadedAudioData) -> Result<(), String> {
        // Stop and discard previous sound handle.
        if let Some(ref mut handle) = self.sound_handle {
            handle.stop(Tween::default());
        }
        self.sound_handle = None;

        self.clip_duration_seconds = preloaded.clip_duration;
        self.audio_path = Some(preloaded.path.clone());
        self.start_beat = preloaded.start_beat;
        self.start_time_seconds = 0.0;
        self.encoder_delay_seconds = preloaded.encoder_delay;

        // Play the sound immediately paused (equivalent to audioSource.clip = audioClip).
        let data_clone = preloaded.sound_data.clone();
        let mut handle = self.audio_manager.play(data_clone)
            .map_err(|e| format!("Failed to play audio: {}", e))?;
        handle.pause(Tween::default());
        handle.seek_to(0.0);
        self.sound_handle = Some(handle);
        self.sound_data = Some(preloaded.sound_data);

        self.is_ready = true;
        if let Some(ref mut cb) = self.on_clip_changed {
            cb(true);
        }

        let file_name = Path::new(&preloaded.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let delay_info = if self.encoder_delay_seconds > 0.0 {
            format!(" encoderDelay={:.1}ms", self.encoder_delay_seconds * 1000.0)
        } else {
            String::new()
        };
        log::info!(
            "[ImportedAudioSyncController] Imported audio attached for sync: '{}' startBeat={:.2}{}",
            file_name, self.start_beat, delay_info
        );

        Ok(())
    }
}

// ─── Async preloading (runs on background thread) ───

/// Pre-decoded audio data ready to be applied to the controller on the main thread.
/// The heavy I/O (file read + decode + ffprobe) happens off-thread; only the fast
/// AudioManager::play call happens on the main thread via `apply_preloaded`.
pub struct PreloadedAudioData {
    pub sound_data: StaticSoundData,
    pub encoder_delay: f32,
    pub clip_duration: f32,
    pub path: String,
    pub start_beat: f32,
}

/// Performs the expensive audio loading work (file I/O + decode + ffprobe).
/// Safe to call from any thread — returns data to be applied on main thread.
pub fn preload_audio(path: &str, start_beat_offset: f32) -> Result<PreloadedAudioData, String> {
    let sound_data = StaticSoundData::from_file(path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;

    let clip_duration = sound_data.duration().as_secs_f32();
    if clip_duration <= 0.0 {
        return Err("Decoded audio clip has zero duration".to_string());
    }

    let encoder_delay = probe_encoder_delay_seconds(path);

    Ok(PreloadedAudioData {
        sound_data,
        encoder_delay,
        clip_duration,
        path: path.to_string(),
        start_beat: start_beat_offset.max(0.0),
    })
}

// ─── ffprobe encoder delay probing (module-level functions) ───

/// Port of C# ProbeEncoderDelaySeconds(string audioPath).
/// Returns 0 for lossless formats or when ffprobe is unavailable.
fn probe_encoder_delay_seconds(audio_path: &str) -> f32 {
    if audio_path.is_empty() {
        return 0.0;
    }

    let ext = Path::new(audio_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    // Lossless formats have no encoder delay.
    if ext == "wav" || ext == "aif" || ext == "aiff" {
        return 0.0;
    }

    let ffprobe = match resolve_ffprobe_binary() {
        Some(p) => p,
        None => return 0.0,
    };

    let output = match run_ffprobe_query(&ffprobe, audio_path) {
        Some(o) => o,
        None => return 0.0,
    };

    let trimmed = output.trim();
    if let Ok(start_time) = trimmed.parse::<f32>()
        && start_time > 0.0001 && start_time <= MAX_ENCODER_DELAY_SECONDS {
            return start_time;
        }

    0.0
}

/// Port of C# RunFfprobeQuery(string ffprobePath, string audioPath).
/// Rust uses std::process::Command (no IL2CPP popen workaround needed).
fn run_ffprobe_query(ffprobe_path: &str, audio_path: &str) -> Option<String> {
    let output = Command::new(ffprobe_path)
        .args([
            "-v", "quiet",
            "-show_entries", "format=start_time",
            "-of", "default=noprint_wrappers=1:nokey=1",
            audio_path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let result = String::from_utf8_lossy(&output.stdout).to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Port of C# ResolveFfprobeBinary().
/// Searches standard locations for the ffprobe binary.
fn resolve_ffprobe_binary() -> Option<String> {
    // 1. Explicit env var.
    if let Ok(env_path) = std::env::var("FFPROBE_PATH")
        && !env_path.is_empty() && Path::new(&env_path).exists() {
            return Some(env_path);
        }

    // 2. Derive from FFMPEG_PATH by replacing the binary name.
    if let Ok(ffmpeg_env) = std::env::var("FFMPEG_PATH")
        && !ffmpeg_env.is_empty()
            && let Some(derived) = derive_ffprobe_from_ffmpeg_path(&ffmpeg_env)
                && Path::new(&derived).exists() {
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

/// Port of C# DeriveFfprobeFromFfmpegPath(string ffmpegPath).
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
