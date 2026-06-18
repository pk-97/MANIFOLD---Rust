//! Per-clip audio-layer playback (Phase 3 of the Audio Layer feature — see
//! `docs/AUDIO_LAYER_DESIGN.md` §4).
//!
//! Generalizes the single-track [`crate::audio_sync::ImportedAudioSyncController`]
//! to **one kira voice per active audio clip**, keyed by `ClipId`. Each tick the
//! content thread calls [`AudioLayerPlayback::update`]: every audio clip under
//! the playhead is played through kira (the existing output backend + mixer),
//! sample-accurately following the transport (seek-on-drift, replay-on-stop —
//! the same policy the imported-audio controller uses). Mute/solo/gain become a
//! per-voice volume tween, which also declicks start/stop/seek.
//!
//! Decoding reuses [`crate::audio_sync::preload_audio`] (symphonia + encoder-delay
//! probe), so there is no second decode path.

use std::collections::HashSet;
use std::time::Duration;

use ahash::AHashMap;
use kira::{
    manager::{AudioManager, AudioManagerSettings, backend::DefaultBackend},
    sound::PlaybackState as KiraPlaybackState,
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    tween::Tween,
};

use manifold_core::id::ClipId;
use manifold_core::project::Project;
use manifold_core::types::PlaybackState;
use manifold_core::{Beats, Seconds};

use crate::audio_sync::preload_audio;
use crate::engine::PlaybackEngine;

/// Hard-resync threshold: reseek the voice if it drifts more than this from the
/// transport-expected position while playing. Matches the imported-audio path.
const HARD_RESYNC_SECONDS: f64 = 0.20;
/// Tolerance for nudging a *paused* voice to the scrub position.
const PAUSED_SEEK_TOLERANCE_SECONDS: f64 = 0.06;
/// Short fade for start/stop/volume changes so clip edges and mutes don't click.
const DECLICK_MS: u64 = 5;

/// A short volume/transport tween that declicks an edge (start, stop, seek-jump).
fn declick() -> Tween {
    Tween { duration: Duration::from_millis(DECLICK_MS), ..Default::default() }
}

/// One playing (or paused) clip voice.
struct Voice {
    handle: StaticSoundHandle,
    /// Kept so a voice that kira auto-stops at the natural end can be replayed.
    data: StaticSoundData,
    /// The clip's file path the voice was built from — a change rebuilds it.
    path: String,
    duration: Seconds,
    encoder_delay: Seconds,
}

/// Build a fresh voice for `path` (decode + start paused at 0). `None` on a
/// decode/play failure (logged) — a genuine "no audio," not a silent stand-in.
fn make_voice(manager: &mut AudioManager<DefaultBackend>, path: &str) -> Option<Voice> {
    let pre = match preload_audio(path, Beats::ZERO) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[AudioLayerPlayback] decode failed for '{path}': {e}");
            return None;
        }
    };
    let mut handle = match manager.play(pre.sound_data.clone()) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("[AudioLayerPlayback] play failed for '{path}': {e}");
            return None;
        }
    };
    handle.pause(Tween::default());
    handle.seek_to(0.0);
    Some(Voice {
        handle,
        data: pre.sound_data,
        path: path.to_string(),
        duration: pre.clip_duration,
        encoder_delay: pre.encoder_delay,
    })
}

/// Owns the kira manager + one voice per active audio clip. Lives on the content
/// thread beside the imported-audio controller.
pub struct AudioLayerPlayback {
    manager: AudioManager<DefaultBackend>,
    voices: AHashMap<ClipId, Voice>,
}

impl AudioLayerPlayback {
    /// Create the playback manager (opens kira's default-output backend).
    pub fn new() -> Result<Self, String> {
        let manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|e| format!("Failed to create audio-layer manager: {e}"))?;
        Ok(Self { manager, voices: AHashMap::new() })
    }

    /// Drive every audio clip under the playhead. Called each content tick.
    pub fn update(&mut self, project: &Project, engine: &PlaybackEngine) {
        let beat = engine.current_beat();
        let now = engine.current_time();
        let state = engine.current_state();
        // Audio layers have their own solo bus (design §5): a soloed audio layer
        // silences other audio layers, independent of the visual solo.
        let any_solo = project.timeline.layers.iter().any(|l| l.is_audio() && l.is_solo);

        let mut active: HashSet<ClipId> = HashSet::new();
        for layer in project.timeline.layers.iter().filter(|l| l.is_audio()) {
            let audible = !layer.is_muted && (!any_solo || layer.is_solo);
            let volume = if audible { layer.audio_gain_linear() as f64 } else { 0.0 };
            let Some(clip) = layer.active_audio_clip_at(beat) else {
                continue;
            };
            active.insert(clip.id.clone());
            // Source position the playhead is over: elapsed since the clip start,
            // offset into the file by the clip's in-point. Warp (P4) would scale
            // the elapsed term; ratio 1 for now.
            let clip_start = engine.beat_to_timeline_time_immut(clip.start_beat);
            let expected = (now - clip_start) + clip.in_point;
            self.sync_clip(&clip.id, &clip.audio_file_path, expected, state, volume);
        }

        // Pause voices whose clip isn't active this tick (declicked).
        for (id, voice) in self.voices.iter_mut() {
            if !active.contains(id) && voice.handle.state() == KiraPlaybackState::Playing {
                voice.handle.pause(declick());
            }
        }

        self.evict_absent_clips(project);
    }

    /// Sync a single clip's voice to the transport-expected position + volume.
    /// The voice is removed from the map for the duration so kira's manager and
    /// the voice can be borrowed independently, then reinserted.
    fn sync_clip(
        &mut self,
        id: &ClipId,
        path: &str,
        expected: Seconds,
        state: PlaybackState,
        volume: f64,
    ) {
        // Take the existing voice if its file still matches; otherwise (re)build.
        let mut voice = match self.voices.remove(id) {
            Some(v) if v.path == path => v,
            stale => {
                if let Some(mut old) = stale {
                    old.handle.stop(Tween::default());
                }
                if path.is_empty() {
                    return;
                }
                match make_voice(&mut self.manager, path) {
                    Some(v) => v,
                    None => return,
                }
            }
        };

        voice.handle.set_volume(volume, declick());
        let duration = voice.duration;
        let in_range = expected >= Seconds::ZERO && expected < duration;
        let target = (expected + voice.encoder_delay)
            .clamp(Seconds::ZERO, (duration - Seconds(0.001)).max(Seconds::ZERO));
        let playing = voice.handle.state() == KiraPlaybackState::Playing;

        match state {
            PlaybackState::Playing => {
                if !in_range {
                    if playing {
                        voice.handle.pause(declick());
                    }
                } else if !playing {
                    if voice.handle.state() == KiraPlaybackState::Stopped {
                        // Kira stops a handle at the natural end; replay for a
                        // fresh one seeked to the expected position.
                        match self.manager.play(voice.data.clone()) {
                            Ok(mut h) => {
                                h.seek_to(target.0);
                                h.set_volume(volume, Tween::default());
                                voice.handle = h;
                            }
                            Err(e) => log::warn!("[AudioLayerPlayback] replay failed: {e}"),
                        }
                    } else {
                        voice.handle.seek_to(target.0);
                        voice.handle.resume(declick());
                    }
                } else {
                    let pos = Seconds(voice.handle.position());
                    if (pos - target).abs() > Seconds(HARD_RESYNC_SECONDS) {
                        voice.handle.seek_to(target.0);
                    }
                }
            }
            PlaybackState::Paused => {
                if playing {
                    voice.handle.pause(declick());
                }
                if in_range {
                    let pos = Seconds(voice.handle.position());
                    if (pos - target).abs() > Seconds(PAUSED_SEEK_TOLERANCE_SECONDS) {
                        voice.handle.seek_to(target.0);
                    }
                }
            }
            _ => {
                // Stopped: silence and rewind.
                if playing {
                    voice.handle.pause(declick());
                }
                if voice.handle.position() > 0.0 {
                    voice.handle.seek_to(0.0);
                }
            }
        }

        self.voices.insert(id.clone(), voice);
    }

    /// Drop voices whose clip is no longer present in the project (stopping the
    /// kira handle). Bounded scan, only when voices exist.
    fn evict_absent_clips(&mut self, project: &Project) {
        if self.voices.is_empty() {
            return;
        }
        let present: HashSet<&ClipId> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio())
            .flat_map(|l| l.clips.iter().filter(|c| c.is_audio()).map(|c| &c.id))
            .collect();
        self.voices.retain(|id, voice| {
            let keep = present.contains(id);
            if !keep {
                voice.handle.stop(Tween::default());
            }
            keep
        });
    }

    /// Stop and drop every voice (e.g. on project close / reset).
    pub fn reset(&mut self) {
        for voice in self.voices.values_mut() {
            voice.handle.stop(Tween::default());
        }
        self.voices.clear();
    }

    /// Number of live voices (test/diagnostic).
    #[cfg(test)]
    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }
}
