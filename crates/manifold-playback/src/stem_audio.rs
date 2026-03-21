// Port of Unity StemAudioController.cs (473 lines).
// Manages 4 AudioSources for Demucs stem playback (drums, bass, other, vocals).
// Syncs sample-perfectly to ImportedAudioSyncController's timeline position.
// When expanded, master audio is muted and stems play. When collapsed, master resumes.
//
// Uses kira for audio playback (replacing Unity AudioSource + AudioClip).

use kira::{
    manager::{AudioManager, AudioManagerSettings, backend::DefaultBackend},
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    sound::PlaybackState as KiraPlaybackState,
    tween::Tween,
};
use manifold_core::types::PlaybackState;
use crate::audio_sync::ImportedAudioSyncController;
use crate::engine::PlaybackEngine;
use std::path::Path;

pub const STEM_COUNT: usize = 4;

pub const STEM_NAMES: [&str; STEM_COUNT] = ["Drums", "Bass", "Other", "Vocals"];
pub const STEM_FILE_NAMES: [&str; STEM_COUNT] = ["drums", "bass", "other", "vocals"];

const HARD_RESYNC_SECONDS: f32 = 0.05;
const SEEK_TOLERANCE_SECONDS: f32 = 0.06;

/// State for a single stem's audio handle.
struct StemSlot {
    handle: Option<StaticSoundHandle>,
    data: Option<StaticSoundData>,
    clip_duration_seconds: f32,
    available: bool,
    path: Option<String>,
}

impl StemSlot {
    fn new() -> Self {
        Self {
            handle: None,
            data: None,
            clip_duration_seconds: 0.0,
            available: false,
            path: None,
        }
    }

    fn reset(&mut self) {
        if let Some(ref mut h) = self.handle {
            if h.state() == KiraPlaybackState::Playing {
                h.pause(Tween::default());
            }
            h.stop(Tween::default());
        }
        self.handle = None;
        self.data = None;
        self.clip_duration_seconds = 0.0;
        self.available = false;
        self.path = None;
    }
}

/// Port of Unity StemAudioController : MonoBehaviour.
/// Manages 4 kira sound handles for Demucs stem playback.
pub struct StemAudioController {
    audio_manager: AudioManager<DefaultBackend>,
    stems: [StemSlot; STEM_COUNT],
    stem_muted: [bool; STEM_COUNT],
    stem_soloed: [bool; STEM_COUNT],
    expanded: bool,
    stems_ready: bool,
    stems_loaded_count: usize,
}

impl StemAudioController {
    /// Port of Awake(). Creates the audio manager.
    pub fn new() -> Result<Self, String> {
        let audio_manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|e| format!("Failed to create stem audio manager: {}", e))?;

        Ok(Self {
            audio_manager,
            stems: [StemSlot::new(), StemSlot::new(), StemSlot::new(), StemSlot::new()],
            stem_muted: [false; STEM_COUNT],
            stem_soloed: [false; STEM_COUNT],
            expanded: false,
            stems_ready: false,
            stems_loaded_count: 0,
        })
    }

    // ─── Properties ───

    pub fn is_expanded(&self) -> bool { self.expanded }
    pub fn stems_ready(&self) -> bool { self.stems_ready }
    pub fn stems_loaded_count(&self) -> usize { self.stems_loaded_count }

    // ──────────────────────────────────────
    // STEM LOADING
    // ──────────────────────────────────────

    /// Load stems from the given paths. None entries are skipped (that stem is unavailable).
    /// paths must be length 4: [drums, bass, other, vocals].
    ///
    /// Port of C# LoadStems(string[] paths) + LoadStemsAsync() coroutine.
    /// In Rust we load synchronously (kira decodes in memory).
    pub fn load_stems(&mut self, paths: &[Option<String>; STEM_COUNT]) {
        self.stems_ready = false;
        self.stems_loaded_count = 0;

        for i in 0..STEM_COUNT {
            self.stems[i].available = false;

            // Stop and discard previous handle/data.
            if let Some(ref mut h) = self.stems[i].handle {
                h.stop(Tween::default());
            }
            self.stems[i].handle = None;
            self.stems[i].data = None;
            self.stems[i].clip_duration_seconds = 0.0;

            let path = match &paths[i] {
                Some(p) if !p.is_empty() && Path::new(p).exists() => p.clone(),
                _ => {
                    self.stems[i].path = paths[i].clone();
                    continue;
                }
            };

            self.stems[i].path = Some(path.clone());

            // Load and decode (equivalent to UnityWebRequestMultimedia.GetAudioClip for WAV).
            match StaticSoundData::from_file(&path) {
                Ok(sound_data) => {
                    let clip_duration = sound_data.duration().as_secs_f32();
                    if clip_duration <= 0.0 {
                        log::warn!(
                            "[StemAudioController] Decoded stem '{}' has zero duration",
                            STEM_FILE_NAMES[i]
                        );
                        continue;
                    }

                    // Play immediately paused (equivalent to audioSource.clip = clip).
                    match self.audio_manager.play(sound_data.clone()) {
                        Ok(mut handle) => {
                            handle.pause(Tween::default());
                            handle.seek_to(0.0);
                            // Volume 0 until expanded (Unity: stemSources[i].volume = 0f)
                            handle.set_volume(0.0, Tween::default());
                            self.stems[i].handle = Some(handle);
                            self.stems[i].data = Some(sound_data);
                            self.stems[i].clip_duration_seconds = clip_duration;
                            self.stems[i].available = true;
                            self.stems_loaded_count += 1;
                        }
                        Err(e) => {
                            log::warn!(
                                "[StemAudioController] Failed to play stem '{}': {}",
                                STEM_FILE_NAMES[i], e
                            );
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "[StemAudioController] Failed to load stem '{}': {}",
                        STEM_FILE_NAMES[i], e
                    );
                }
            }
        }

        self.stems_ready = self.stems_loaded_count > 0;

        if self.stems_ready {
            log::info!(
                "[StemAudioController] Loaded {}/{} stems.",
                self.stems_loaded_count, STEM_COUNT
            );
        }
    }

    /// Apply pre-loaded stem data (loaded on background thread).
    /// Fast — only does AudioManager::play (no file I/O).
    pub fn apply_preloaded_stems(&mut self, preloaded: PreloadedStemData) {
        self.stems_ready = false;
        self.stems_loaded_count = 0;

        for i in 0..STEM_COUNT {
            // Stop and discard previous handle/data.
            if let Some(ref mut h) = self.stems[i].handle {
                h.stop(Tween::default());
            }
            self.stems[i].handle = None;
            self.stems[i].data = None;
            self.stems[i].clip_duration_seconds = 0.0;
            self.stems[i].available = false;
            self.stems[i].path = preloaded.paths[i].clone();

            if let Some(ref stem) = preloaded.stems[i] {
                match self.audio_manager.play(stem.sound_data.clone()) {
                    Ok(mut handle) => {
                        handle.pause(Tween::default());
                        handle.seek_to(0.0);
                        handle.set_volume(0.0, Tween::default());
                        self.stems[i].handle = Some(handle);
                        self.stems[i].data = Some(stem.sound_data.clone());
                        self.stems[i].clip_duration_seconds = stem.clip_duration;
                        self.stems[i].available = true;
                        self.stems_loaded_count += 1;
                    }
                    Err(e) => {
                        log::warn!(
                            "[StemAudioController] Failed to play preloaded stem '{}': {}",
                            STEM_FILE_NAMES[i], e
                        );
                    }
                }
            }
        }

        self.stems_ready = self.stems_loaded_count > 0;
        if self.stems_ready {
            log::info!(
                "[StemAudioController] Applied {}/{} preloaded stems.",
                self.stems_loaded_count, STEM_COUNT
            );
        }
    }

    /// Get the clip duration in seconds for a stem (for waveform rendering).
    pub fn stem_clip_duration_seconds(&self, index: usize) -> f32 {
        if index < STEM_COUNT { self.stems[index].clip_duration_seconds } else { 0.0 }
    }

    /// Returns true if the given stem is available (loaded successfully).
    pub fn is_stem_available(&self, index: usize) -> bool {
        index < STEM_COUNT && self.stems[index].available
    }

    // ──────────────────────────────────────
    // EXPAND / COLLAPSE
    // ──────────────────────────────────────

    /// Port of C# SetExpanded(bool expand).
    /// When expanded: mute master, apply stem volumes.
    /// When collapsed: stop stems, restore master.
    pub fn set_expanded(
        &mut self,
        expand: bool,
        master: Option<&mut ImportedAudioSyncController>,
    ) {
        if self.expanded == expand {
            return;
        }

        self.expanded = expand;

        if self.expanded {
            // Mute the master AudioSource; stems will handle playback.
            // Port: masterController.Source.volume = 0f
            if let Some(m) = master {
                m.set_volume(0.0);
            }
            self.apply_mute_solo_volumes();
        } else {
            // Stop all stems and restore master volume.
            for i in 0..STEM_COUNT {
                if let Some(ref mut h) = self.stems[i].handle {
                    let state = h.state();
                    if state == KiraPlaybackState::Playing {
                        h.pause(Tween::default());
                    }
                    h.set_volume(0.0, Tween::default());
                }
            }

            // Restore master volume.
            // Port: masterController.Source.volume = 1f
            if let Some(m) = master {
                m.set_volume(1.0);
            }
        }
    }

    // ──────────────────────────────────────
    // MUTE / SOLO
    // ──────────────────────────────────────

    pub fn is_muted(&self, index: usize) -> bool {
        index < STEM_COUNT && self.stem_muted[index]
    }

    pub fn is_soloed(&self, index: usize) -> bool {
        index < STEM_COUNT && self.stem_soloed[index]
    }

    /// Port of C# ToggleMuted(int index).
    pub fn toggle_muted(&mut self, index: usize) {
        if index >= STEM_COUNT { return; }
        self.stem_muted[index] = !self.stem_muted[index];
        self.apply_mute_solo_volumes();
    }

    /// Port of C# ToggleSoloed(int index).
    pub fn toggle_soloed(&mut self, index: usize) {
        if index >= STEM_COUNT { return; }
        self.stem_soloed[index] = !self.stem_soloed[index];
        self.apply_mute_solo_volumes();
    }

    /// Standard DAW mute/solo logic: audible = (!anySoloed || isSoloed) && !isMuted
    ///
    /// Port of C# ApplyMuteSoloVolumes().
    fn apply_mute_solo_volumes(&mut self) {
        if !self.expanded { return; }

        let any_soloed = (0..STEM_COUNT).any(|i| self.stem_soloed[i] && self.stems[i].available);

        for i in 0..STEM_COUNT {
            if !self.stems[i].available {
                if let Some(ref mut h) = self.stems[i].handle {
                    h.set_volume(0.0, Tween::default());
                }
                continue;
            }

            // Standard DAW logic: audible = (!anySoloed || isSoloed) && !isMuted
            let audible = (!any_soloed || self.stem_soloed[i]) && !self.stem_muted[i];
            let volume = if audible { 1.0 } else { 0.0 };
            if let Some(ref mut h) = self.stems[i].handle {
                h.set_volume(volume, Tween::default());
            }
        }
    }

    // ──────────────────────────────────────
    // SYNC
    // ──────────────────────────────────────

    /// Called every frame. Syncs all stem handles to the master's timeline position.
    /// Only active when expanded.
    ///
    /// Port of C# UpdateSync(ImportedAudioSyncController master, PlaybackController pc).
    pub fn update_sync(
        &mut self,
        master: &ImportedAudioSyncController,
        engine: &PlaybackEngine,
    ) {
        if !self.expanded || !self.stems_ready || !master.is_ready() {
            return;
        }

        // Defensive: ensure master is muted every frame while stems are expanded.
        // Port: if (master.Source != null && master.Source.volume > 0f) master.Source.volume = 0f
        // (handled externally — master volume is set by set_expanded and caller)

        let start_time_seconds = engine.beat_to_timeline_time_immut(master.start_beat());
        let expected_time = engine.current_time() - start_time_seconds;
        // No encoder delay for WAV stems.

        let state = engine.current_state();

        for i in 0..STEM_COUNT {
            if !self.stems[i].available {
                continue;
            }
            let clip_length = self.stems[i].clip_duration_seconds;
            if clip_length <= 0.0 {
                continue;
            }
            self.sync_single_stem(i, expected_time, clip_length, state);
        }
    }

    /// Port of C# SyncSingleStem(AudioSource, AudioClip, float expectedTime, PlaybackState state).
    fn sync_single_stem(
        &mut self,
        index: usize,
        expected_time: f32,
        clip_length: f32,
        state: PlaybackState,
    ) {
        let in_range = expected_time >= 0.0 && expected_time < clip_length;
        let clamped = expected_time.clamp(0.0, (clip_length - 0.001).max(0.0));

        let handle = match self.stems[index].handle.as_mut() {
            Some(h) => h,
            None => return,
        };

        let handle_state = handle.state();
        let is_playing = handle_state == KiraPlaybackState::Playing;

        match state {
            PlaybackState::Playing => {
                if !in_range {
                    if is_playing {
                        handle.pause(Tween::default());
                    }
                    return;
                }

                if !is_playing {
                    // Kira auto-transitions to Stopped at end of sound.
                    // Must re-play from data to get a fresh handle.
                    if handle_state == KiraPlaybackState::Stopped {
                        if let Some(ref data) = self.stems[index].data {
                            match self.audio_manager.play(data.clone()) {
                                Ok(mut new_handle) => {
                                    new_handle.seek_to(clamped as f64);
                                    // Apply current volume
                                    let any_soloed = (0..STEM_COUNT).any(|j| {
                                        self.stem_soloed[j] && self.stems[j].available
                                    });
                                    let audible = (!any_soloed || self.stem_soloed[index])
                                        && !self.stem_muted[index];
                                    new_handle.set_volume(
                                        if audible { 1.0 } else { 0.0 },
                                        Tween::default(),
                                    );
                                    self.stems[index].handle = Some(new_handle);
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[StemAudioController] Failed to restart stem '{}': {}",
                                        STEM_FILE_NAMES[index], e
                                    );
                                }
                            }
                        }
                    } else {
                        handle.seek_to(clamped as f64);
                        handle.resume(Tween::default());
                    }
                    return;
                }

                // Already playing — check for drift.
                let current_pos = handle.position() as f32;
                if (current_pos - clamped).abs() > HARD_RESYNC_SECONDS {
                    handle.seek_to(clamped as f64);
                }
            }
            PlaybackState::Paused => {
                if is_playing {
                    handle.pause(Tween::default());
                }

                if in_range {
                    let current_pos = handle.position() as f32;
                    if (current_pos - clamped).abs() > SEEK_TOLERANCE_SECONDS {
                        handle.seek_to(clamped as f64);
                    }
                }
            }
            _ => {
                // Stopped
                if is_playing {
                    handle.pause(Tween::default());
                }
                let current_pos = handle.position() as f32;
                if current_pos > 0.0 {
                    handle.seek_to(0.0);
                }
            }
        }
    }

    // ──────────────────────────────────────
    // RESET
    // ──────────────────────────────────────

    /// Port of C# ResetStems().
    pub fn reset_stems(&mut self, master: Option<&mut ImportedAudioSyncController>) {
        self.expanded = false;
        self.stems_ready = false;
        self.stems_loaded_count = 0;

        for i in 0..STEM_COUNT {
            self.stem_muted[i] = false;
            self.stem_soloed[i] = false;
            self.stems[i].reset();
        }

        // Restore master volume.
        if let Some(m) = master {
            m.set_volume(1.0);
        }
    }

    // ──────────────────────────────────────
    // STEM PATH RESOLUTION (static utility)
    // ──────────────────────────────────────

    /// Scans AudioAnalysisStemCache for stems matching the given audio file.
    /// Matches by checking raw/htdemucs/{audioBaseName}/ inside each cache dir.
    /// Among matching dirs, picks the most recently written stems.
    /// Returns an array of 4 paths (None for missing stems).
    ///
    /// Port of C# ResolveStemPathsFromCache(string audioFilePath).
    pub fn resolve_stem_paths_from_cache(
        audio_file_path: &str,
        cache_base: &str,
    ) -> [Option<String>; STEM_COUNT] {
        let mut result: [Option<String>; STEM_COUNT] = Default::default();

        if audio_file_path.is_empty() {
            return result;
        }

        let cache_path = std::path::Path::new(cache_base);
        if !cache_path.is_dir() {
            return result;
        }

        let audio_base_name = std::path::Path::new(audio_file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        if audio_base_name.is_empty() {
            return result;
        }

        let mut best_stems_dir: Option<std::path::PathBuf> = None;
        let mut best_modified = std::time::SystemTime::UNIX_EPOCH;

        if let Ok(entries) = std::fs::read_dir(cache_path) {
            for entry in entries.flatten() {
                let sub_dir = entry.path();
                if !sub_dir.is_dir() {
                    continue;
                }

                // Verify this cache entry was produced from the same audio file.
                let demucs_dir = sub_dir.join("raw").join("htdemucs").join(audio_base_name);
                if !demucs_dir.is_dir() {
                    continue;
                }

                let stems_dir = sub_dir.join("stems");
                if !stems_dir.is_dir() {
                    continue;
                }

                if let Ok(meta) = std::fs::metadata(&stems_dir) {
                    if let Ok(modified) = meta.modified() {
                        if modified > best_modified {
                            best_modified = modified;
                            best_stems_dir = Some(stems_dir);
                        }
                    }
                }
            }
        }

        if let Some(stems_dir) = best_stems_dir {
            for i in 0..STEM_COUNT {
                let path = stems_dir.join(format!("{}.wav", STEM_FILE_NAMES[i]));
                if path.exists() {
                    result[i] = Some(path.to_string_lossy().to_string());
                }
            }
        }

        result
    }
}

// ─── Pre-loaded stem data (for background thread loading) ───

/// Pre-decoded data for a single stem.
pub struct PreloadedStem {
    pub sound_data: StaticSoundData,
    pub clip_duration: f32,
}

/// Pre-decoded data for all 4 stems, ready to apply on the content thread.
pub struct PreloadedStemData {
    pub stems: [Option<PreloadedStem>; STEM_COUNT],
    pub paths: [Option<String>; STEM_COUNT],
}

/// Perform heavy I/O for all 4 stems on a background thread.
/// Returns data ready for apply_preloaded_stems() on the content thread.
pub fn preload_stems(paths: &[Option<String>; STEM_COUNT]) -> PreloadedStemData {
    let mut stems: [Option<PreloadedStem>; STEM_COUNT] = Default::default();
    let mut result_paths: [Option<String>; STEM_COUNT] = Default::default();

    for i in 0..STEM_COUNT {
        result_paths[i] = paths[i].clone();

        let path = match &paths[i] {
            Some(p) if !p.is_empty() && Path::new(p).exists() => p,
            _ => continue,
        };

        match StaticSoundData::from_file(path) {
            Ok(sound_data) => {
                let clip_duration = sound_data.duration().as_secs_f32();
                if clip_duration > 0.0 {
                    stems[i] = Some(PreloadedStem { sound_data, clip_duration });
                } else {
                    log::warn!(
                        "[StemAudioController] Preloaded stem '{}' has zero duration",
                        STEM_FILE_NAMES[i]
                    );
                }
            }
            Err(e) => {
                log::warn!(
                    "[StemAudioController] Failed to preload stem '{}': {}",
                    STEM_FILE_NAMES[i], e
                );
            }
        }
    }

    PreloadedStemData { stems, paths: result_paths }
}
